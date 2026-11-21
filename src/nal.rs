//! H.264, H.265 and MPEG-2 video at the level of NAL units and user data:
//! Annex B start codes, emulation prevention, SEI messages and the
//! user data messages that carry, for example, MISB ST 0604 timestamps.
//! Read side; compiled in every build.
//!
//! Nothing here decodes pictures. The functions accept any bytes, never
//! panic, do work linear in their input and allocate at most a copy of it.
//!
//! ```
//! use mpegts::nal::{find_user_data, user_data_sei, VideoCodec};
//!
//! // An H.264 access unit: access unit delimiter, then an SEI NAL unit with
//! // one user_data_unregistered message.
//! let mut au = vec![0, 0, 0, 1, 0x09, 0xF0];
//! user_data_sei(VideoCodec::H264, 1, b"MISPmicrosectime", &[0x9F, 0, 0], &mut au);
//! let mut found = Vec::new();
//! find_user_data(VideoCodec::H264, &au, &mut found);
//! assert_eq!(found.len(), 1);
//! assert_eq!(&found[0].uuid, b"MISPmicrosectime");
//! // The two zero bytes are protected by an emulation prevention byte in
//! // the stream and come back without it.
//! assert_eq!(found[0].payload, [0x9F, 0, 0]);
//! ```

use crate::Codec;

/// payloadType of the user_data_unregistered SEI message (ITU-T H.264
/// D.1.7, ITU-T H.265 D.2.7).
pub const USER_DATA_UNREGISTERED: u32 = 5;

/// MPEG-2 video user_data_start_code value (the byte after 00 00 01,
/// ITU-T H.262 6.2.2.2.2).
pub const MPEG2_USER_DATA: u8 = 0xB2;

/// A video format whose NAL units and user data this module reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum VideoCodec {
    /// H.264 / AVC: 1-byte NAL unit header, SEI in nal_unit_type 6, VCL
    /// NAL unit types 1 to 5 (and 20, 21 in SVC and MVC).
    H264,
    /// H.265 / HEVC: 2-byte NAL unit header, prefix SEI in nal_unit_type 39
    /// and suffix SEI in 40, VCL NAL unit types 0 to 31.
    H265,
    /// MPEG-2 video (H.262): user_data after start code 00 00 01 B2, slices
    /// after start codes 00 00 01 01 to 00 00 01 AF.
    Mpeg2,
}

impl VideoCodec {
    /// The format of a stream the demuxer identified as `codec`, when it is
    /// one of the three.
    pub fn from_codec(codec: Codec) -> Option<VideoCodec> {
        match codec {
            Codec::H264 => Some(VideoCodec::H264),
            Codec::H265 => Some(VideoCodec::H265),
            Codec::Mpeg2Video => Some(VideoCodec::Mpeg2),
            _ => None,
        }
    }

    /// The format of a PMT stream_type: 0x02 MPEG-2 video, 0x1B H.264,
    /// 0x24 H.265 (ISO/IEC 13818-1 Table 2-34). Other stream types,
    /// including sub-bitstreams such as 0x1F (SVC), give `None`.
    pub fn from_stream_type(stream_type: u8) -> Option<VideoCodec> {
        match stream_type {
            0x02 => Some(VideoCodec::Mpeg2),
            0x1B => Some(VideoCodec::H264),
            0x24 => Some(VideoCodec::H265),
            _ => None,
        }
    }

    /// The matching [`Codec`].
    pub fn codec(self) -> Codec {
        match self {
            VideoCodec::H264 => Codec::H264,
            VideoCodec::H265 => Codec::H265,
            VideoCodec::Mpeg2 => Codec::Mpeg2Video,
        }
    }

    /// Bytes of the NAL unit header: 1 (H.264), 2 (H.265), or 1 for the
    /// start code value of MPEG-2.
    pub fn header_len(self) -> usize {
        match self {
            VideoCodec::H265 => 2,
            _ => 1,
        }
    }

    /// nal_unit_type from the first header byte (H.264 7.3.1, H.265
    /// 7.3.1.2), or the start code value itself for MPEG-2.
    pub fn nal_type(self, first_header_byte: u8) -> u8 {
        match self {
            VideoCodec::H264 => first_header_byte & 0x1F,
            VideoCodec::H265 => (first_header_byte >> 1) & 0x3F,
            VideoCodec::Mpeg2 => first_header_byte,
        }
    }

    /// Whether a NAL unit of this type carries picture data: H.264 types 1
    /// to 5 and the SVC and MVC slice extensions 20 and 21 (Table 7-1),
    /// H.265 types 0 to 31 (Table 7-1), MPEG-2 slice start codes 0x01 to
    /// 0xAF (H.262 Table 6-1).
    pub fn is_vcl(self, nal_type: u8) -> bool {
        match self {
            VideoCodec::H264 => matches!(nal_type, 1..=5 | 20 | 21),
            VideoCodec::H265 => nal_type < 32,
            VideoCodec::Mpeg2 => (0x01..=0xAF).contains(&nal_type),
        }
    }

    /// Whether a NAL unit of this type is an SEI NAL unit: H.264 type 6,
    /// H.265 types 39 (prefix) and 40 (suffix). MPEG-2 has none.
    pub fn is_sei(self, nal_type: u8) -> bool {
        match self {
            VideoCodec::H264 => nal_type == 6,
            VideoCodec::H265 => nal_type == 39 || nal_type == 40,
            VideoCodec::Mpeg2 => false,
        }
    }
}

/// One NAL unit of an Annex B byte stream (ITU-T H.264 B.1, H.265 B.2), or
/// one start code unit of MPEG-2 video.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct NalUnit<'a> {
    /// Offset of its start code in the scanned buffer, including the
    /// zero_byte of a 4-byte start code.
    pub start: usize,
    /// Offset of its first header byte, right after 00 00 01.
    pub offset: usize,
    /// The NAL unit as in the stream: header and payload with emulation
    /// prevention bytes, without the trailing zero bytes before the next
    /// start code. Empty for a start code at the very end of the buffer.
    pub data: &'a [u8],
    /// Offset of the next start code (its 00 00 01), or the end of the
    /// buffer: the NAL unit and the zero bytes after it end here.
    pub next: usize,
}

impl NalUnit<'_> {
    /// Offset just past the NAL unit in the scanned buffer.
    pub fn end(&self) -> usize {
        self.offset + self.data.len()
    }

    /// nal_unit_type (see [`VideoCodec::nal_type`]); `None` when the NAL
    /// unit is empty.
    pub fn nal_type(&self, codec: VideoCodec) -> Option<u8> {
        self.data.first().map(|&b| codec.nal_type(b))
    }

    /// The bytes after the header, emulation prevention bytes included;
    /// empty when the NAL unit is shorter than its header.
    pub fn payload(&self, codec: VideoCodec) -> &[u8] {
        self.data.get(codec.header_len()..).unwrap_or(&[])
    }
}

/// The NAL units of an Annex B byte stream in `data`, in order. Bytes
/// before the first start code belong to no NAL unit and are skipped.
pub fn nal_units(data: &[u8]) -> NalUnits<'_> {
    NalUnits {
        data,
        next: find_start_code(data, 0),
    }
}

/// Iterator over the NAL units of a buffer; see [`nal_units`].
#[derive(Debug, Clone)]
pub struct NalUnits<'a> {
    data: &'a [u8],
    /// Offset of the next 00 00 01.
    next: Option<usize>,
}

impl<'a> Iterator for NalUnits<'a> {
    type Item = NalUnit<'a>;

    fn next(&mut self) -> Option<NalUnit<'a>> {
        let at = self.next?;
        let offset = at + 3;
        let following = find_start_code(self.data, offset);
        self.next = following;
        let mut end = following.unwrap_or(self.data.len());
        // Zero bytes before the next start code are its zero_byte or
        // trailing_zero_8bits, not part of this NAL unit (B.1.1, B.2.1).
        while end > offset && self.data[end - 1] == 0 {
            end -= 1;
        }
        // The zero_byte of a 4-byte start code (B.1.2).
        let start = if at > 0 && self.data[at - 1] == 0 { at - 1 } else { at };
        Some(NalUnit {
            start,
            offset,
            data: &self.data[offset..end],
            next: following.unwrap_or(self.data.len()),
        })
    }
}

/// Offset of the first 00 00 01 at or after `from`.
pub(crate) fn find_start_code(data: &[u8], from: usize) -> Option<usize> {
    let mut i = from;
    while i + 3 <= data.len() {
        match data[i + 2] {
            // A start code may begin at i + 1 or i + 2.
            0 => i += 1,
            1 if data[i] == 0 && data[i + 1] == 0 => return Some(i),
            // None can begin at i, i + 1 or i + 2.
            _ => i += 3,
        }
    }
    None
}

/// Append `ebsp` with its emulation prevention bytes removed: every 0x03
/// that follows two zero bytes is discarded (ITU-T H.264 7.3.1 and 7.4.1,
/// H.265 7.3.1.1). Pass the bytes after the NAL unit header.
pub fn remove_emulation_prevention(ebsp: &[u8], out: &mut Vec<u8>) {
    out.reserve(ebsp.len());
    let mut zeros = 0;
    for &b in ebsp {
        if zeros >= 2 && b == 3 {
            zeros = 0;
            continue;
        }
        out.push(b);
        zeros = if b == 0 { zeros + 1 } else { 0 };
    }
}

/// Append `rbsp` with emulation prevention applied: a 0x03 goes after two
/// zero bytes whenever the next byte is 0x03 or less, and after two zero
/// bytes at the very end (ITU-T H.264 7.4.1, H.265 7.4.2,
/// R-ST1402TIME-81). Pass the bytes after the NAL unit header.
/// [`remove_emulation_prevention`] gives `rbsp` back.
pub fn add_emulation_prevention(rbsp: &[u8], out: &mut Vec<u8>) {
    out.reserve(rbsp.len() + rbsp.len() / 64 + 1);
    let mut zeros = 0;
    for &b in rbsp {
        if zeros >= 2 && b <= 3 {
            out.push(3);
            zeros = 0;
        }
        out.push(b);
        zeros = if b == 0 { zeros + 1 } else { 0 };
    }
    // An RBSP ending in a cabac_zero_word gets a final 0x03 (7.4.1).
    if zeros >= 2 {
        out.push(3);
    }
}

/// One SEI message (ITU-T H.264 7.3.2.3.1, H.265 7.3.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SeiMessage<'a> {
    /// payloadType.
    pub payload_type: u32,
    /// The payload, payloadSize bytes of RBSP.
    pub payload: &'a [u8],
    /// Offset of the message (its first payloadType byte) in the RBSP.
    pub offset: usize,
    /// Bytes of the whole message: payloadType bytes, payloadSize bytes and
    /// the payload.
    pub len: usize,
}

/// The SEI messages of an SEI RBSP (`sei_rbsp()`: the NAL unit payload
/// after the header, emulation prevention removed), in order.
pub fn sei_messages(rbsp: &[u8]) -> SeiMessages<'_> {
    SeiMessages {
        rbsp,
        at: 0,
        malformed: false,
    }
}

/// Iterator over SEI messages; see [`sei_messages`]. It stops at the
/// rbsp_trailing_bits, or at a message that runs past the data, after which
/// [`malformed`](Self::malformed) is true.
#[derive(Debug, Clone)]
pub struct SeiMessages<'a> {
    rbsp: &'a [u8],
    at: usize,
    malformed: bool,
}

impl SeiMessages<'_> {
    /// A message ran past the end of the data, or the data ended without
    /// rbsp_trailing_bits.
    pub fn malformed(&self) -> bool {
        self.malformed
    }
}

impl<'a> Iterator for SeiMessages<'a> {
    type Item = SeiMessage<'a>;

    fn next(&mut self) -> Option<SeiMessage<'a>> {
        let rest = self.rbsp.get(self.at..).unwrap_or(&[]);
        // more_rbsp_data(): what is left is rbsp_trailing_bits (a 0x80 byte
        // and zero bytes) or nothing.
        if rest.is_empty() {
            self.malformed = true;
            return None;
        }
        if rest[0] == 0x80 && rest[1..].iter().all(|&b| b == 0) {
            return None;
        }
        let rbsp = self.rbsp;
        let start = self.at;
        let mut at = start;
        let read_value = |at: &mut usize| -> Option<usize> {
            let mut v = 0usize;
            loop {
                let b = *rbsp.get(*at)?;
                *at += 1;
                v = v.saturating_add(usize::from(b));
                if b != 0xFF {
                    return Some(v);
                }
            }
        };
        let parsed = read_value(&mut at).and_then(|t| read_value(&mut at).map(|s| (t, s)));
        let Some((payload_type, size)) = parsed else {
            self.malformed = true;
            self.at = self.rbsp.len();
            return None;
        };
        let Some(payload) = at.checked_add(size).and_then(|end| rbsp.get(at..end)) else {
            self.malformed = true;
            self.at = self.rbsp.len();
            return None;
        };
        self.at = at + size;
        Some(SeiMessage {
            payload_type: u32::try_from(payload_type).unwrap_or(u32::MAX),
            payload,
            offset: start,
            len: self.at - start,
        })
    }
}

/// Where a [`UserData`] message was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct UserDataPosition {
    /// Offset of the start code of the NAL unit (or MPEG-2 user_data)
    /// holding the message, the zero_byte of a 4-byte start code included.
    pub start: usize,
    /// Offset of the NAL unit header, right after 00 00 01.
    pub header: usize,
    /// Offset just past the NAL unit, trailing zero bytes excluded (for
    /// MPEG-2, the next start code: user data may end in zero bytes).
    pub end: usize,
    /// Index of the message among the SEI messages of its NAL unit (0 for
    /// MPEG-2).
    pub message: usize,
    /// nal_unit_type (6 in H.264, 39 or 40 in H.265), or
    /// [`MPEG2_USER_DATA`].
    pub nal_type: u8,
}

/// A user data message: an SEI user_data_unregistered message (H.264,
/// H.265) or an MPEG-2 user_data structure read the same way
/// (st1402-time.md D-14).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct UserData {
    /// uuid_iso_iec_11578, or the first 16 bytes of MPEG-2 user data.
    pub uuid: [u8; 16],
    /// user_data_payload_byte: the bytes after the identifier, emulation
    /// prevention removed.
    pub payload: Vec<u8>,
    /// Where the message is.
    pub position: UserDataPosition,
}

impl UserData {
    /// A message with no position, for comparisons and tests.
    pub fn new(uuid: [u8; 16], payload: Vec<u8>) -> Self {
        UserData {
            uuid,
            payload,
            position: UserDataPosition {
                start: 0,
                header: 0,
                end: 0,
                message: 0,
                nal_type: 0,
            },
        }
    }
}

/// Append to `out` every user data message of `data`, in stream order:
/// for H.264 and H.265 each user_data_unregistered SEI message (payloadType
/// 5) of at least 16 bytes in an SEI NAL unit, for MPEG-2 each user_data
/// of at least 16 bytes. `data` is an Annex B byte stream or MPEG-2 video
/// elementary stream bytes (a whole access unit, a prefix of one, or
/// more). Messages in malformed SEI NAL units are reported up to the
/// damage. MPEG-2 user data runs up to the next start code prefix, zero
/// bytes included (a time stamp may end in one).
pub fn find_user_data(codec: VideoCodec, data: &[u8], out: &mut Vec<UserData>) {
    let mut rbsp = Vec::new();
    for nal in nal_units(data) {
        user_data_of(codec, data, &nal, &mut rbsp, out);
    }
}

/// The user data messages of one NAL unit of `buf` (see
/// [`find_user_data`]); `rbsp` is scratch space.
pub(crate) fn user_data_of(
    codec: VideoCodec,
    buf: &[u8],
    nal: &NalUnit<'_>,
    rbsp: &mut Vec<u8>,
    out: &mut Vec<UserData>,
) {
    let Some(nal_type) = nal.nal_type(codec) else {
        return;
    };
    let mut position = UserDataPosition {
        start: nal.start,
        header: nal.offset,
        end: nal.end(),
        message: 0,
        nal_type,
    };
    if codec == VideoCodec::Mpeg2 {
        // ITU-T H.262 6.2.2.2.2: user_data_start_code, then bytes up to the
        // next start code; no emulation prevention.
        if nal_type == MPEG2_USER_DATA {
            position.end = nal.next.max(nal.end());
            if let Some(data) = buf.get(nal.offset + 1..position.end) {
                if data.len() >= 16 {
                    let mut uuid = [0u8; 16];
                    uuid.copy_from_slice(&data[..16]);
                    out.push(UserData {
                        uuid,
                        payload: data[16..].to_vec(),
                        position,
                    });
                }
            }
        }
        return;
    }
    if !codec.is_sei(nal_type) || nal.data.len() < codec.header_len() {
        return;
    }
    rbsp.clear();
    remove_emulation_prevention(nal.payload(codec), rbsp);
    for (i, m) in sei_messages(rbsp).enumerate() {
        if m.payload_type != USER_DATA_UNREGISTERED || m.payload.len() < 16 {
            continue;
        }
        let mut uuid = [0u8; 16];
        uuid.copy_from_slice(&m.payload[..16]);
        position.message = i;
        out.push(UserData {
            uuid,
            payload: m.payload[16..].to_vec(),
            position,
        });
    }
}

/// Append the payloadType or payloadSize coding of `value`: 0xFF bytes,
/// then the last byte (H.264 7.3.2.3.1).
fn push_sei_value(mut value: usize, out: &mut Vec<u8>) {
    while value >= 255 {
        out.push(0xFF);
        value -= 255;
    }
    out.push(value as u8);
}

/// Append an SEI NAL unit in byte stream form holding one
/// user_data_unregistered message: a 4-byte start code (R-ST1402TIME-83),
/// the NAL unit header (H.264: 0x06, nal_ref_idc 0, R-ST1402TIME-79; H.265:
/// PREFIX_SEI_NUT 39, nuh_layer_id 0 and `temporal_id_plus1` (1 to 7),
/// R-ST1402TIME-84, -85), payloadType 5, payloadSize, the uuid and payload,
/// rbsp_trailing_bits, with emulation prevention (R-ST1402TIME-80, -81).
/// Returns false, appending nothing, for MPEG-2 or a `temporal_id_plus1`
/// out of range in H.265.
pub fn user_data_sei(
    codec: VideoCodec,
    temporal_id_plus1: u8,
    uuid: &[u8; 16],
    payload: &[u8],
    out: &mut Vec<u8>,
) -> bool {
    match codec {
        VideoCodec::H264 => out.extend_from_slice(&[0, 0, 0, 1, 0x06]),
        VideoCodec::H265 if (1..=7).contains(&temporal_id_plus1) => {
            out.extend_from_slice(&[0, 0, 0, 1, 39 << 1, temporal_id_plus1])
        }
        _ => return false,
    }
    let mut rbsp = Vec::with_capacity(payload.len() + 24);
    push_sei_value(USER_DATA_UNREGISTERED as usize, &mut rbsp);
    push_sei_value(16 + payload.len(), &mut rbsp);
    rbsp.extend_from_slice(uuid);
    rbsp.extend_from_slice(payload);
    rbsp.push(0x80);
    add_emulation_prevention(&rbsp, out);
    true
}

/// Append SEI NAL unit `nal` (header and payload as in the stream, see
/// [`NalUnit::data`]) with the payload of its message `index` replaced by
/// `payload` and payloadSize coded again; every other byte of the RBSP
/// stays, and emulation prevention is applied to the result
/// (R-ST1402TIME-81, st1402-time.md D-12). For a user_data_unregistered
/// message, `payload` holds the uuid and then the bytes. Returns false,
/// appending nothing, when `nal` is not an SEI NAL unit of `codec` or has
/// no message `index`.
pub fn replace_sei_payload(codec: VideoCodec, nal: &[u8], index: usize, payload: &[u8], out: &mut Vec<u8>) -> bool {
    let header_len = codec.header_len();
    if nal.len() < header_len || !codec.is_sei(codec.nal_type(nal[0])) {
        return false;
    }
    let mut rbsp = Vec::new();
    remove_emulation_prevention(&nal[header_len..], &mut rbsp);
    let Some(m) = sei_messages(&rbsp).nth(index) else {
        return false;
    };
    let mut new = Vec::with_capacity(rbsp.len() + payload.len());
    new.extend_from_slice(&rbsp[..m.offset]);
    push_sei_value(m.payload_type as usize, &mut new);
    push_sei_value(payload.len(), &mut new);
    new.extend_from_slice(payload);
    new.extend_from_slice(&rbsp[m.offset + m.len..]);
    out.extend_from_slice(&nal[..header_len]);
    add_emulation_prevention(&new, out);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn units(data: &[u8]) -> Vec<(usize, usize, Vec<u8>)> {
        nal_units(data).map(|n| (n.start, n.offset, n.data.to_vec())).collect()
    }

    #[test]
    fn start_codes_of_three_and_four_bytes() {
        let data = [
            0xAA, 0, 0, 0, 1, 0x09, 0xF0, 0, 0, 1, 0x67, 1, 2, 0, 0, 0, 0, 0, 1, 0x65, 7, 0, 0, 1,
        ];
        assert_eq!(
            units(&data),
            [
                (1, 5, vec![0x09, 0xF0]),
                (7, 10, vec![0x67, 1, 2]),
                // Trailing zero bytes belong to nobody; the last one is the
                // zero_byte of the 4-byte start code.
                (15, 19, vec![0x65, 7]),
                (21, 24, vec![]),
            ]
        );
        assert!(units(&[0, 0, 2, 0, 1]).is_empty());
        assert!(units(&[]).is_empty());
    }

    #[test]
    fn emulation_prevention_round_trips() {
        let rbsp = [0, 0, 0, 0xFF, 0, 0, 3, 0, 0, 1, 0, 0, 2, 7, 0, 0];
        let mut ebsp = Vec::new();
        add_emulation_prevention(&rbsp, &mut ebsp);
        assert_eq!(ebsp, [0, 0, 3, 0, 0xFF, 0, 0, 3, 3, 0, 0, 3, 1, 0, 0, 3, 2, 7, 0, 0, 3]);
        let mut back = Vec::new();
        remove_emulation_prevention(&ebsp, &mut back);
        assert_eq!(back, rbsp);
    }

    #[test]
    fn sei_messages_with_extension_bytes() {
        // payloadType 5 + 255 + 1 = 261 (FF 06), payloadSize 2; then type
        // 5 with size 300 (FF 2D); trailing bits.
        let mut rbsp = vec![0xFF, 0x06, 0x02, 0xAA, 0xBB, 0x05, 0xFF, 0x2D];
        rbsp.extend(std::iter::repeat(0x11).take(300));
        rbsp.push(0x80);
        let m: Vec<_> = sei_messages(&rbsp).map(|m| (m.payload_type, m.payload.len(), m.offset, m.len)).collect();
        assert_eq!(m, [(261, 2, 0, 5), (5, 300, 5, 303)]);
        let mut it = sei_messages(&rbsp[..20]);
        assert_eq!(it.by_ref().count(), 1);
        assert!(it.malformed());
    }

    #[test]
    fn user_data_from_each_codec() {
        let uuid = *b"0123456789abcdef";
        let mut es = Vec::new();
        assert!(user_data_sei(VideoCodec::H265, 2, &uuid, &[0, 0, 0, 1], &mut es));
        // Followed by a second message in a suffix SEI.
        es.extend_from_slice(&[0, 0, 1, 40 << 1, 1, 0x01, 0x01, 0x42, 0x05, 17]);
        es.extend_from_slice(&uuid);
        es.extend_from_slice(&[0x77, 0x80]);
        let mut out = Vec::new();
        find_user_data(VideoCodec::H265, &es, &mut out);
        assert_eq!(out.len(), 2);
        assert_eq!((out[0].payload.as_slice(), out[0].position.nal_type), (&[0, 0, 0, 1][..], 39));
        assert_eq!((out[1].payload.as_slice(), out[1].position.message), (&[0x77][..], 1));
        assert_eq!(es[out[0].position.start..out[0].position.header], [0, 0, 0, 1]);

        let mut m2 = vec![0, 0, 1, 0x00, 1, 2, 0, 0, 1, MPEG2_USER_DATA];
        m2.extend_from_slice(&uuid);
        m2.extend_from_slice(&[9, 9, 0, 0, 1, 0x01, 5]);
        out.clear();
        find_user_data(VideoCodec::Mpeg2, &m2, &mut out);
        assert_eq!(out, [UserData { position: out[0].position, ..UserData::new(uuid, vec![9, 9]) }]);
    }

    #[test]
    fn replace_keeps_the_other_messages() {
        let uuid = [7u8; 16];
        let mut nal = vec![0x06];
        let mut rbsp = vec![0x01, 0x01, 0x33, 0x05, 17];
        rbsp.extend_from_slice(&uuid);
        rbsp.extend_from_slice(&[0x10, 0x80]);
        add_emulation_prevention(&rbsp, &mut nal);
        let mut new_payload = uuid.to_vec();
        new_payload.extend_from_slice(&[0, 0, 0, 0, 0xFF]);
        let mut out = Vec::new();
        assert!(replace_sei_payload(VideoCodec::H264, &nal, 1, &new_payload, &mut out));
        let mut found = Vec::new();
        let mut au = vec![0, 0, 1];
        au.extend_from_slice(&out);
        find_user_data(VideoCodec::H264, &au, &mut found);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].payload, [0, 0, 0, 0, 0xFF]);
        assert_eq!(found[0].position.message, 1);
        let mut back = Vec::new();
        remove_emulation_prevention(&out[1..], &mut back);
        assert_eq!(&back[..3], [0x01, 0x01, 0x33]);
        assert!(!replace_sei_payload(VideoCodec::H264, &nal, 2, &new_payload, &mut out));
        assert!(!replace_sei_payload(VideoCodec::H264, &[0x65, 1], 0, &new_payload, &mut out));
    }
}
