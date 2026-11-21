//! Single transport stream packets and PES headers (ISO/IEC 13818-1
//! 2.4.3.2 to 2.4.3.7), for code that works below the demuxer: pass-through
//! tools, injectors and checkers that look at one packet at a time.
//!
//! These parsers are separate from the demuxer's own inlined hot path, so
//! they can afford to report every field and to say exactly what is wrong.

use crate::demuxer::{has_optional_header, read_pcr, read_timestamp, PACKET_SIZE, SYNC_BYTE};
use std::fmt;

/// PID of null packets (ISO/IEC 13818-1 Table 2-3). In a PMT's PCR_PID it
/// means "no PCR" (2.4.4.9).
pub const NULL_PID: u16 = 0x1FFF;
/// PTS and DTS are 33-bit counts of the 90 kHz clock and wrap at 2^33
/// (ISO/IEC 13818-1 2.4.3.7, R-ISO13818-065).
pub const PTS_WRAP: u64 = 1 << 33;
/// The 27 MHz PCR wraps at 2^33 x 300 (base 33 bits, extension 0..299;
/// ISO/IEC 13818-1 2.4.2.2, R-ISO13818-042).
pub const PCR_WRAP: u64 = PTS_WRAP * 300;

/// `a - b` for two PTS or DTS values, modulo 2^33, as the signed distance
/// in (-2^32, 2^32]. A wrap-around (about every 26.5 hours) is not a jump:
/// `pts_delta(5, PTS_WRAP - 5) == 10` (DECISIONS D-ISO13818-17).
pub fn pts_delta(a: u64, b: u64) -> i64 {
    wrapping_delta(a, b, PTS_WRAP)
}

/// `a - b` for two 27 MHz PCR values, modulo 2^33 x 300, as the signed
/// distance in (-PCR_WRAP / 2, PCR_WRAP / 2].
pub fn pcr_delta(a: u64, b: u64) -> i64 {
    wrapping_delta(a, b, PCR_WRAP)
}

fn wrapping_delta(a: u64, b: u64, wrap: u64) -> i64 {
    let d = (a % wrap + wrap - b % wrap) % wrap;
    if d > wrap / 2 {
        d as i64 - wrap as i64
    } else {
        d as i64
    }
}

/// Why [`PacketHeader::parse`] rejected a packet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum PacketError {
    /// Byte 0 is not the sync byte 0x47 (2.4.3.3).
    Sync,
    /// adaptation_field_length runs past the end of the packet, or a field
    /// that the adaptation field flags announce does not fit inside the
    /// adaptation field (2.4.3.5).
    AdaptationField,
}

impl fmt::Display for PacketError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            PacketError::Sync => "first byte is not the 0x47 sync byte",
            PacketError::AdaptationField => "adaptation field is longer than the packet or its fields do not fit",
        })
    }
}

impl std::error::Error for PacketError {}

/// The fields of an adaptation field (ISO/IEC 13818-1 2.4.3.4, 2.4.3.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub struct AdaptationField {
    /// adaptation_field_length: bytes after the length byte. 0 is a single
    /// stuffing byte without flags.
    pub length: u8,
    /// discontinuity_indicator.
    pub discontinuity: bool,
    /// random_access_indicator.
    pub random_access: bool,
    /// elementary_stream_priority_indicator.
    pub es_priority: bool,
    /// Program clock reference, 27 MHz (base x 300 + extension).
    pub pcr: Option<u64>,
    /// Original program clock reference, 27 MHz.
    pub opcr: Option<u64>,
    /// splice_countdown, when splicing_point_flag is set.
    pub splice_countdown: Option<i8>,
    /// transport_private_data_length, when transport_private_data_flag is
    /// set.
    pub private_data_length: Option<u8>,
    /// adaptation_field_extension_flag (the extension is not parsed).
    pub extension: bool,
}

/// A parsed TS packet header (ISO/IEC 13818-1 2.4.3.2, 2.4.3.3) with its
/// adaptation field.
///
/// ```
/// use mpegts::PacketHeader;
///
/// let mut p = [0xFFu8; 188];
/// p[..4].copy_from_slice(&[0x47, 0x41, 0x00, 0x17]); // PUSI, PID 0x100, CC 7
/// let h = PacketHeader::parse(&p).unwrap();
/// assert_eq!((h.pid, h.payload_unit_start, h.continuity_counter), (0x100, true, 7));
/// assert_eq!(h.payload(&p).len(), 184);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct PacketHeader {
    /// transport_error_indicator. When set the rest of the header may be
    /// damaged too.
    pub transport_error: bool,
    /// payload_unit_start_indicator.
    pub payload_unit_start: bool,
    /// transport_priority.
    pub transport_priority: bool,
    /// The 13-bit PID.
    pub pid: u16,
    /// transport_scrambling_control (2 bits; 0 means not scrambled).
    pub scrambling: u8,
    /// adaptation_field_control (2 bits): 1 payload only, 2 adaptation
    /// field only, 3 both. 0 is reserved; such packets carry nothing
    /// (decoders discard them, R-ISO13818-008).
    pub adaptation_field_control: u8,
    /// continuity_counter (4 bits).
    pub continuity_counter: u8,
    /// The adaptation field, when adaptation_field_control says there is
    /// one.
    pub adaptation_field: Option<AdaptationField>,
    /// Offset of the first payload byte in the packet; 188 when the packet
    /// carries no payload.
    pub payload_offset: usize,
}

impl PacketHeader {
    /// Parse the header and adaptation field of one 188-byte packet.
    pub fn parse(p: &[u8; PACKET_SIZE]) -> Result<PacketHeader, PacketError> {
        if p[0] != SYNC_BYTE {
            return Err(PacketError::Sync);
        }
        let afc = (p[3] >> 4) & 0x03;
        let mut h = PacketHeader {
            transport_error: p[1] & 0x80 != 0,
            payload_unit_start: p[1] & 0x40 != 0,
            transport_priority: p[1] & 0x20 != 0,
            pid: (u16::from(p[1] & 0x1F) << 8) | u16::from(p[2]),
            scrambling: p[3] >> 6,
            adaptation_field_control: afc,
            continuity_counter: p[3] & 0x0F,
            adaptation_field: None,
            payload_offset: 4,
        };
        if afc & 0b10 != 0 {
            let af = parse_adaptation_field(p)?;
            h.payload_offset = 5 + usize::from(af.length);
            h.adaptation_field = Some(af);
        }
        if afc & 0b01 == 0 {
            h.payload_offset = PACKET_SIZE;
        }
        Ok(h)
    }

    /// The packet carries at least one payload byte.
    pub fn has_payload(&self) -> bool {
        self.payload_offset < PACKET_SIZE
    }

    /// The payload bytes of `packet` (the packet this header was parsed
    /// from); empty when there are none.
    pub fn payload<'a>(&self, packet: &'a [u8; PACKET_SIZE]) -> &'a [u8] {
        &packet[self.payload_offset.min(PACKET_SIZE)..]
    }

    /// PCR from the adaptation field, 27 MHz.
    pub fn pcr(&self) -> Option<u64> {
        self.adaptation_field.and_then(|af| af.pcr)
    }

    /// discontinuity_indicator from the adaptation field.
    pub fn discontinuity(&self) -> bool {
        self.adaptation_field.is_some_and(|af| af.discontinuity)
    }

    /// random_access_indicator from the adaptation field.
    pub fn random_access(&self) -> bool {
        self.adaptation_field.is_some_and(|af| af.random_access)
    }

    /// A null packet (PID 0x1FFF).
    pub fn is_null(&self) -> bool {
        self.pid == NULL_PID
    }
}

fn parse_adaptation_field(p: &[u8; PACKET_SIZE]) -> Result<AdaptationField, PacketError> {
    let length = p[4];
    // 183 is the most that fits after the 4-byte header and the length
    // byte (2.4.3.5).
    if length > 183 {
        return Err(PacketError::AdaptationField);
    }
    let mut af = AdaptationField { length, ..AdaptationField::default() };
    if length == 0 {
        return Ok(af);
    }
    let end = 5 + usize::from(length);
    let flags = p[5];
    af.discontinuity = flags & 0x80 != 0;
    af.random_access = flags & 0x40 != 0;
    af.es_priority = flags & 0x20 != 0;
    af.extension = flags & 0x01 != 0;
    let mut at = 6;
    let mut take = |n: usize| -> Result<usize, PacketError> {
        if at + n > end {
            return Err(PacketError::AdaptationField);
        }
        at += n;
        Ok(at - n)
    };
    if flags & 0x10 != 0 {
        let i = take(6)?;
        af.pcr = Some(read_pcr(&p[i..i + 6]));
    }
    if flags & 0x08 != 0 {
        let i = take(6)?;
        af.opcr = Some(read_pcr(&p[i..i + 6]));
    }
    if flags & 0x04 != 0 {
        let i = take(1)?;
        af.splice_countdown = Some(p[i] as i8);
    }
    if flags & 0x02 != 0 {
        // The private data must end inside the adaptation field
        // (R-ISO13818-028).
        let i = take(1)?;
        take(usize::from(p[i]))?;
        af.private_data_length = Some(p[i]);
    }
    Ok(af)
}

/// Why [`PesHeader::parse`] rejected a PES header.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum PesError {
    /// The data ends inside the header.
    Truncated,
    /// The data does not start with packet_start_code_prefix 00 00 01.
    StartCode,
    /// The optional header does not start with '10', PTS_DTS_flags is the
    /// forbidden '01', or PES_header_data_length is shorter than the
    /// timestamps the flags announce (2.4.3.7).
    Malformed,
}

impl fmt::Display for PesError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            PesError::Truncated => "data ends inside the PES header",
            PesError::StartCode => "no PES start code (00 00 01)",
            PesError::Malformed => "malformed PES header",
        })
    }
}

impl std::error::Error for PesError {}

/// The fixed fields of a PES packet header and its timestamps (ISO/IEC
/// 13818-1 2.4.3.6, 2.4.3.7).
///
/// ```
/// use mpegts::PesHeader;
///
/// // stream_id 0xFC, PTS 126000 (a sync KLV header, ISO/IEC 13818-1 Table 2-21).
/// let pes = [0, 0, 1, 0xFC, 0x00, 0x0D, 0x85, 0x80, 0x05, 0x21, 0x00, 0x07, 0xD8, 0x61];
/// let h = PesHeader::parse(&pes).unwrap();
/// assert_eq!((h.stream_id, h.pts, h.payload_offset), (0xFC, Some(126_000), 14));
/// assert!(h.data_alignment && h.original && h.markers_ok);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct PesHeader {
    /// stream_id.
    pub stream_id: u8,
    /// PES_packet_length: bytes after this field; 0 means unbounded (video
    /// only, R-ISO13818-051).
    pub packet_length: u16,
    /// PES_scrambling_control (2 bits).
    pub scrambling: u8,
    /// PES_priority.
    pub priority: bool,
    /// data_alignment_indicator.
    pub data_alignment: bool,
    /// copyright.
    pub copyright: bool,
    /// original_or_copy: true for an original.
    pub original: bool,
    /// Presentation timestamp, 90 kHz, 33 bits.
    pub pts: Option<u64>,
    /// Decoding timestamp, 90 kHz, 33 bits.
    pub dts: Option<u64>,
    /// Every marker bit and 4-bit prefix of the timestamp fields has the
    /// value 2.4.3.7 requires (R-ISO13818-063, R-ISO13818-064). Timestamps
    /// are decoded either way.
    pub markers_ok: bool,
    /// PES_header_data_length (0 for stream_ids without the optional
    /// header).
    pub header_data_length: u8,
    /// Offset of the first PES_packet_data_byte.
    pub payload_offset: usize,
}

impl PesHeader {
    /// Parse the header at the start of a PES packet. `data` needs to hold
    /// the whole header (at most 9 + 255 bytes), not the whole packet.
    pub fn parse(data: &[u8]) -> Result<PesHeader, PesError> {
        if data.len() < 6 {
            return Err(PesError::Truncated);
        }
        if data[..3] != [0, 0, 1] {
            return Err(PesError::StartCode);
        }
        let stream_id = data[3];
        let mut h = PesHeader {
            stream_id,
            packet_length: u16::from_be_bytes([data[4], data[5]]),
            scrambling: 0,
            priority: false,
            data_alignment: false,
            copyright: false,
            original: false,
            pts: None,
            dts: None,
            markers_ok: true,
            header_data_length: 0,
            payload_offset: 6,
        };
        if !Self::has_optional_header(stream_id) {
            return Ok(h);
        }
        if data.len() < 9 {
            return Err(PesError::Truncated);
        }
        let (b6, b7, len) = (data[6], data[7], data[8]);
        if b6 >> 6 != 0b10 {
            return Err(PesError::Malformed);
        }
        h.scrambling = (b6 >> 4) & 0x03;
        h.priority = b6 & 0x08 != 0;
        h.data_alignment = b6 & 0x04 != 0;
        h.copyright = b6 & 0x02 != 0;
        h.original = b6 & 0x01 != 0;
        h.header_data_length = len;
        h.payload_offset = 9 + usize::from(len);
        if data.len() < h.payload_offset {
            return Err(PesError::Truncated);
        }
        let fields = &data[9..h.payload_offset];
        match b7 >> 6 {
            0b00 => {}
            0b10 => {
                let pts = fields.get(..5).ok_or(PesError::Malformed)?;
                h.markers_ok = timestamp_ok(pts, 0b0010);
                h.pts = Some(read_timestamp(pts));
            }
            0b11 => {
                let both = fields.get(..10).ok_or(PesError::Malformed)?;
                h.markers_ok = timestamp_ok(&both[..5], 0b0011) && timestamp_ok(&both[5..], 0b0001);
                h.pts = Some(read_timestamp(&both[..5]));
                h.dts = Some(read_timestamp(&both[5..]));
            }
            // '01' is forbidden (R-ISO13818-066).
            _ => return Err(PesError::Malformed),
        }
        Ok(h)
    }

    /// Whether PES packets with this stream_id carry the optional header
    /// (flags, PES_header_data_length, timestamps). Program stream map,
    /// padding, private_stream_2, ECM, EMM, DSM-CC, H.222.1 type E and the
    /// program stream directory do not (ISO/IEC 13818-1 Table 2-21,
    /// R-ISO13818-049).
    pub fn has_optional_header(stream_id: u8) -> bool {
        has_optional_header(stream_id)
    }

    /// Length of the whole PES packet (header included) when
    /// PES_packet_length is not 0.
    pub fn total_length(&self) -> Option<usize> {
        (self.packet_length != 0).then(|| 6 + usize::from(self.packet_length))
    }
}

/// The 4-bit prefix and the three marker bits of a 5-byte timestamp field.
fn timestamp_ok(b: &[u8], prefix: u8) -> bool {
    b[0] >> 4 == prefix && b[0] & 1 == 1 && b[2] & 1 == 1 && b[4] & 1 == 1
}

#[cfg(test)]
mod tests {
    use super::*;

    fn packet(header: [u8; 4], af: &[u8]) -> [u8; PACKET_SIZE] {
        let mut p = [0xFF; PACKET_SIZE];
        p[..4].copy_from_slice(&header);
        p[4..4 + af.len()].copy_from_slice(af);
        p
    }

    #[test]
    fn header_fields() {
        let p = packet([0x47, 0xE1, 0x23, 0xD5], &[]);
        let h = PacketHeader::parse(&p).unwrap();
        assert!(h.transport_error && h.payload_unit_start && h.transport_priority);
        assert_eq!((h.pid, h.scrambling, h.adaptation_field_control, h.continuity_counter), (0x0123, 3, 1, 5));
        assert_eq!(h.payload_offset, 4);
        assert!(h.has_payload() && h.adaptation_field.is_none() && !h.is_null());
        assert_eq!(PacketHeader::parse(&packet([0x46, 0, 0, 0x10], &[])), Err(PacketError::Sync));
    }

    #[test]
    fn adaptation_field_forms() {
        // One stuffing byte.
        let h = PacketHeader::parse(&packet([0x47, 0, 0x20, 0x30], &[0])).unwrap();
        assert_eq!(h.payload_offset, 5);
        assert_eq!(h.adaptation_field.unwrap().length, 0);
        // Adaptation field only: no payload whatever its length says.
        let h = PacketHeader::parse(&packet([0x47, 0, 0x20, 0x20], &[183, 0x00])).unwrap();
        assert!(!h.has_payload());
        assert_eq!(h.payload(&packet([0x47, 0, 0x20, 0x20], &[183, 0x00])).len(), 0);
        // Reserved adaptation_field_control '00': nothing.
        let h = PacketHeader::parse(&packet([0x47, 0, 0x20, 0x00], &[])).unwrap();
        assert!(!h.has_payload() && h.adaptation_field.is_none());
        // Too long.
        assert_eq!(PacketHeader::parse(&packet([0x47, 0, 0, 0x30], &[184])), Err(PacketError::AdaptationField));
    }

    #[test]
    fn adaptation_field_flags_and_clocks() {
        // discontinuity, random access, priority, PCR base 2^33-1 ext 299,
        // OPCR 27_000_000, splice countdown -2, 2 bytes of private data.
        let pcr = [0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x2B];
        let opcr_base = 90_000u64;
        let opcr = [
            (opcr_base >> 25) as u8,
            (opcr_base >> 17) as u8,
            (opcr_base >> 9) as u8,
            (opcr_base >> 1) as u8,
            ((opcr_base & 1) << 7) as u8 | 0x7E,
            0,
        ];
        let mut af = vec![0, 0xFE];
        af.extend_from_slice(&pcr);
        af.extend_from_slice(&opcr);
        af.extend_from_slice(&[0xFE, 2, 0xAA, 0xBB]);
        af[0] = (af.len() - 1) as u8;
        let p = packet([0x47, 0x01, 0x00, 0x30], &af);
        let h = PacketHeader::parse(&p).unwrap();
        let a = h.adaptation_field.unwrap();
        assert!(a.discontinuity && a.random_access && a.es_priority && !a.extension);
        assert_eq!(a.pcr, Some((PTS_WRAP - 1) * 300 + 299));
        assert_eq!(a.opcr, Some(27_000_000));
        assert_eq!(a.splice_countdown, Some(-2));
        assert_eq!(a.private_data_length, Some(2));
        assert_eq!(h.payload_offset, 5 + af.len() - 1);
        assert_eq!(h.pcr(), a.pcr);
        assert!(h.discontinuity() && h.random_access());

        // A PCR flag without room for the PCR.
        let p = packet([0x47, 0x01, 0x00, 0x30], &[3, 0x10, 0, 0]);
        assert_eq!(PacketHeader::parse(&p), Err(PacketError::AdaptationField));
        // Private data running past the adaptation field.
        let p = packet([0x47, 0x01, 0x00, 0x30], &[3, 0x02, 5, 0]);
        assert_eq!(PacketHeader::parse(&p), Err(PacketError::AdaptationField));
    }

    #[test]
    fn pes_headers() {
        // PTS and DTS (R-ISO13818-063 test vectors: 0 and 2^33 - 1).
        let pes = [0, 0, 1, 0xE0, 0, 0, 0x80, 0xC0, 10, 0x31, 0, 1, 0, 1, 0x1F, 0xFF, 0xFF, 0xFF, 0xFF];
        let h = PesHeader::parse(&pes).unwrap();
        assert_eq!((h.pts, h.dts, h.packet_length, h.total_length()), (Some(0), Some(PTS_WRAP - 1), 0, None));
        assert!(h.markers_ok && !h.data_alignment && !h.original);
        // Without the optional header.
        let h = PesHeader::parse(&[0, 0, 1, 0xBE, 0, 4]).unwrap();
        assert_eq!((h.payload_offset, h.pts, h.total_length()), (6, None, Some(10)));
        // A zero marker bit is reported, not fatal.
        let pes = [0, 0, 1, 0xFC, 0, 8, 0x85, 0x80, 5, 0x21, 0, 0, 0, 1];
        assert!(!PesHeader::parse(&pes).unwrap().markers_ok);
        // Errors.
        assert_eq!(PesHeader::parse(&[0, 0, 1, 0xE0, 0]), Err(PesError::Truncated));
        assert_eq!(PesHeader::parse(&[0, 0, 2, 0xE0, 0, 0]), Err(PesError::StartCode));
        assert_eq!(PesHeader::parse(&[0, 0, 1, 0xE0, 0, 0, 0x80, 0x40, 0]), Err(PesError::Malformed));
        assert_eq!(PesHeader::parse(&[0, 0, 1, 0xE0, 0, 0, 0x80, 0x80, 2, 0x21, 0]), Err(PesError::Malformed));
        assert_eq!(PesHeader::parse(&[0, 0, 1, 0xE0, 0, 0, 0x40, 0x00, 0]), Err(PesError::Malformed));
        assert_eq!(PesHeader::parse(&[0, 0, 1, 0xE0, 0, 0, 0x80, 0x80, 5, 0x21]), Err(PesError::Truncated));
    }

    #[test]
    fn wrapping_deltas() {
        assert_eq!(pts_delta(5, 3), 2);
        assert_eq!(pts_delta(3, 5), -2);
        assert_eq!(pts_delta(5, PTS_WRAP - 5), 10);
        assert_eq!(pts_delta(PTS_WRAP - 5, 5), -10);
        assert_eq!(pts_delta(PTS_WRAP / 2, 0), (PTS_WRAP / 2) as i64);
        assert_eq!(pts_delta(PTS_WRAP / 2 + 1, 0), -((PTS_WRAP / 2 - 1) as i64));
        assert_eq!(pcr_delta(10, PCR_WRAP - 290), 300);
    }
}
