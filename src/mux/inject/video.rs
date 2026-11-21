//! User data in the target program's video (MUXING_PLAN 6.8,
//! st1402-time.md D-12 to D-14): the prefix of each access unit, read while
//! its first packets are held; the frame callback's write; and the
//! re-packetization of the PES that carries the written bytes.

use super::{InjectError, UserDataSkip, VideoPrefix, VideoSei, MAX_USER_DATA_PAYLOAD};
use crate::mux::Packet;
use crate::nal::{self, UserData, VideoCodec};
use crate::packet::{PesError, PesHeader};
use crate::PACKET_SIZE;
use std::collections::VecDeque;

/// Most user data messages reported per access unit.
const MAX_MESSAGES: usize = 64;
/// Most PES header bytes (9 + PES_header_data_length).
const MAX_PES_HEADER: usize = 9 + 255;
/// TS packet payload bytes without an adaptation field.
const MAX_PAYLOAD: usize = PACKET_SIZE - 4;

/// A held input packet: the packet, its program clock and its M2TS
/// TP_extra_header (zeros for 188-byte input).
pub(super) type Held = (Packet, Option<u64>, [u8; 4]);

/// Why reading a prefix stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stop {
    /// At the first VCL NAL unit; `start` is its start code's offset in
    /// the PES payload, zero_byte included.
    FirstVcl { start: usize, temporal_id_plus1: u8 },
    /// The PES payload does not start with a start code: no access unit
    /// starts here that the reader can place anything in.
    NotAligned,
    /// The PES ended first.
    PesEnded,
    /// The byte or packet limit, or the end of input.
    Limit,
}

/// Reads the prefix of the access unit that a video PES starts: everything
/// before its first VCL NAL unit (ITU-T H.264 7.4.1.2.3, H.265 7.4.2.4.4),
/// or before the first slice in MPEG-2.
#[derive(Debug)]
pub(super) struct PrefixReader {
    codec: VideoCodec,
    /// PES bytes from its first byte, up to the limit.
    pes: Vec<u8>,
    /// PES header length once known.
    header: Option<usize>,
    /// Most payload bytes read.
    limit: usize,
    aligned: bool,
    /// Payload offset from which start codes are still to be looked for.
    scanned: usize,
    stop: Option<Stop>,
}

impl PrefixReader {
    pub fn new(codec: VideoCodec, limit: usize) -> Self {
        PrefixReader {
            codec,
            pes: Vec::new(),
            header: None,
            limit,
            aligned: false,
            scanned: 0,
            stop: None,
        }
    }

    /// The prefix is known: its first VCL NAL unit was found, or reading
    /// stopped for another reason.
    pub fn done(&self) -> bool {
        self.stop.is_some()
    }

    /// Add the payload of the PES's next packet.
    pub fn add(&mut self, payload: &[u8]) {
        if self.stop.is_some() {
            return;
        }
        let max = MAX_PES_HEADER + self.limit + 8;
        let n = payload.len().min(max.saturating_sub(self.pes.len()));
        self.pes.extend_from_slice(&payload[..n]);
        self.look();
    }

    /// Stop reading for a reason found outside: the PES ended, or a limit.
    pub fn stop(&mut self, pes_ended: bool) {
        if self.stop.is_none() {
            self.stop = Some(if pes_ended { Stop::PesEnded } else { Stop::Limit });
        }
    }

    fn look(&mut self) {
        let header = match self.header {
            Some(h) => h,
            None => match PesHeader::parse(&self.pes) {
                Ok(h) => {
                    self.header = Some(h.payload_offset);
                    h.payload_offset
                }
                Err(PesError::Truncated) if self.pes.len() < MAX_PES_HEADER => return,
                Err(_) => {
                    self.stop = Some(Stop::NotAligned);
                    return;
                }
            },
        };
        let codec = self.codec;
        let es = self.pes.get(header..).unwrap_or(&[]);
        // Look no further than the limit.
        let es = &es[..es.len().min(self.limit)];
        if !self.aligned {
            // Zero bytes, then 00 00 01 (leading_zero_8bits and zero_byte,
            // ITU-T H.264 B.1.1): the PES starts an access unit.
            match es.iter().position(|&b| b != 0) {
                None if es.len() < self.limit => return,
                Some(z) if z >= 2 && es[z] == 1 => self.aligned = true,
                _ => {
                    self.stop = Some(Stop::NotAligned);
                    return;
                }
            }
        }
        let mut from = self.scanned;
        loop {
            match nal::find_start_code(es, from) {
                Some(s) => {
                    let h = s + 3;
                    if h + codec.header_len() > es.len() {
                        self.scanned = s;
                        break;
                    }
                    if codec.is_vcl(codec.nal_type(es[h])) {
                        let temporal_id_plus1 = match codec {
                            VideoCodec::H265 => (es[h + 1] & 7).max(1),
                            _ => 1,
                        };
                        // With the zero_byte of a 4-byte start code (H.264
                        // B.1.2); MPEG-2 has none, and user data may end in
                        // a zero byte.
                        let start = if codec != VideoCodec::Mpeg2 && s > 0 && es[s - 1] == 0 { s - 1 } else { s };
                        self.stop = Some(Stop::FirstVcl { start, temporal_id_plus1 });
                        return;
                    }
                    from = h;
                }
                None => {
                    self.scanned = es.len().saturating_sub(2);
                    break;
                }
            }
        }
        if es.len() >= self.limit {
            self.stop = Some(Stop::Limit);
        }
    }

    /// What the frame callback sees, and what a write needs.
    pub fn finish(mut self, mode: VideoSei, writable: bool) -> AuContext {
        let codec = self.codec;
        let stop = self.stop.unwrap_or(Stop::Limit);
        let header = self.header.unwrap_or(self.pes.len()).min(self.pes.len());
        let es_len = (self.pes.len() - header).min(self.limit);
        let (region, complete, terminated) = match stop {
            Stop::FirstVcl { start, .. } => (start, true, true),
            Stop::NotAligned => (0, false, true),
            Stop::PesEnded => (es_len, false, true),
            // The last NAL unit read may be cut.
            Stop::Limit => (es_len, false, false),
        };
        let region = region.min(es_len);
        let mut user_data = Vec::new();
        let mut last = None;
        let mut scratch = Vec::new();
        {
            let es = &self.pes[header..header + region];
            let mut units = nal::nal_units(es).peekable();
            while let Some(u) = units.next() {
                if !terminated && units.peek().is_none() {
                    break;
                }
                if user_data.len() < MAX_MESSAGES {
                    nal::user_data_of(codec, es, &u, &mut scratch, &mut user_data);
                }
                last = u.nal_type(codec).map(|t| (u.start, t));
            }
        }
        user_data.truncate(MAX_MESSAGES);
        let (insert_at, temporal_id_plus1) = match stop {
            // Before a prefix NAL unit that directly precedes the first VCL
            // NAL unit (R-ST1402TIME-82).
            Stop::FirstVcl { start, temporal_id_plus1 } => match last {
                Some((s, 14)) if codec == VideoCodec::H264 => (Some(s), temporal_id_plus1),
                _ => (Some(start), temporal_id_plus1),
            },
            _ => (None, 1),
        };
        self.pes.truncate(header + region);
        AuContext {
            mode,
            codec,
            writable,
            complete,
            bytes: region,
            user_data,
            pes: self.pes,
            header,
            insert_at,
            temporal_id_plus1,
            request: None,
            refused: false,
            asked: false,
        }
    }
}

/// A write the frame callback asked for.
#[derive(Debug)]
struct Request {
    uuid: [u8; 16],
    payload: Vec<u8>,
    replace: Vec<[u8; 16]>,
}

/// The access unit whose frame callback is running: what was read, and the
/// write asked for.
#[derive(Debug)]
pub(super) struct AuContext {
    mode: VideoSei,
    codec: VideoCodec,
    /// VideoSei::Write and stream_type 0x1B or 0x24.
    writable: bool,
    complete: bool,
    bytes: usize,
    pub user_data: Vec<UserData>,
    /// The PES bytes read (header and prefix).
    pes: Vec<u8>,
    header: usize,
    /// Payload offset where a new SEI NAL unit goes.
    insert_at: Option<usize>,
    temporal_id_plus1: u8,
    request: Option<Request>,
    /// A write was refused because the prefix was incomplete.
    pub refused: bool,
    /// A write was asked for.
    asked: bool,
}

/// A change to a PES: bytes `at..remove_end` (offsets from the PES's first
/// byte) become `insert`, which is never shorter.
#[derive(Debug, Clone)]
pub(super) struct Edit {
    pub at: usize,
    pub remove_end: usize,
    pub insert: Vec<u8>,
    /// An existing message rewritten (else a NAL unit inserted).
    pub rewrite: bool,
}

impl AuContext {
    pub fn prefix(&self) -> VideoPrefix {
        VideoPrefix {
            codec: self.codec,
            complete: self.complete,
            bytes: self.bytes,
        }
    }

    pub fn request_write(&mut self, uuid: [u8; 16], payload: &[u8], replace: &[[u8; 16]]) -> Result<(), InjectError> {
        if self.mode != VideoSei::Write {
            return Err(InjectError::UserData("writing needs VideoSei::Write"));
        }
        if !self.writable {
            return Err(InjectError::UserData(
                "only H.264 (stream_type 0x1B) and H.265 (stream_type 0x24) video is written",
            ));
        }
        if self.asked {
            return Err(InjectError::UserData("one write per access unit"));
        }
        if payload.len() > MAX_USER_DATA_PAYLOAD {
            return Err(InjectError::UserData("payload larger than MAX_USER_DATA_PAYLOAD"));
        }
        self.asked = true;
        if !self.complete {
            self.refused = true;
            return Err(InjectError::UserData(
                "the access unit prefix was not read up to its first VCL NAL unit (VideoPrefix::complete)",
            ));
        }
        self.request = Some(Request {
            uuid,
            payload: payload.to_vec(),
            replace: replace.to_vec(),
        });
        Ok(())
    }

    /// PES_packet_length of the PES.
    pub fn pes_length(&self) -> u16 {
        match self.pes.get(4..6) {
            Some(b) => u16::from_be_bytes([b[0], b[1]]),
            None => 0,
        }
    }

    /// The change the write asks for, if any.
    pub fn edit(&self) -> Option<Edit> {
        let r = self.request.as_ref()?;
        let codec = self.codec;
        let es = &self.pes[self.header..];
        let found = self
            .user_data
            .iter()
            .find(|m| m.uuid == r.uuid || r.replace.contains(&m.uuid));
        if let Some(m) = found {
            if let Some(old) = es.get(m.position.header..m.position.end) {
                let mut payload = Vec::with_capacity(16 + r.payload.len());
                payload.extend_from_slice(&r.uuid);
                payload.extend_from_slice(&r.payload);
                let mut new = Vec::new();
                if nal::replace_sei_payload(codec, old, m.position.message, &payload, &mut new) {
                    // A shorter NAL unit is followed by trailing_zero_8bits,
                    // so the PES keeps its size (ITU-T H.264 B.1.1, H.265
                    // B.2.1).
                    if new.len() < old.len() {
                        new.resize(old.len(), 0);
                    }
                    return Some(Edit {
                        at: self.header + m.position.header,
                        remove_end: self.header + m.position.end,
                        insert: new,
                        rewrite: true,
                    });
                }
            }
        }
        let at = self.insert_at?;
        let mut insert = Vec::with_capacity(r.payload.len() + 32);
        if !nal::user_data_sei(codec, self.temporal_id_plus1, &r.uuid, &r.payload, &mut insert) {
            return None;
        }
        Some(Edit {
            at: self.header + at,
            remove_end: self.header + at,
            insert,
            rewrite: false,
        })
    }
}

/// End (packet offset) of the adaptation field without its stuffing, or 4
/// when there is none: the room the packet's payload may take. A field
/// whose layout does not add up, or whose tail is not 0xFF stuffing, is
/// kept whole (ISO/IEC 13818-1 2.4.3.5).
pub(super) fn min_af_end(p: &Packet) -> usize {
    if p[3] & 0x20 == 0 {
        return 4;
    }
    let len = usize::from(p[4]);
    let end = (5 + len).min(PACKET_SIZE);
    if len == 0 {
        return 5;
    }
    let flags = p[5];
    let mut at = 6;
    for (bit, n) in [(0x10, 6), (0x08, 6), (0x04, 1)] {
        if flags & bit != 0 {
            at += n;
        }
    }
    for bit in [0x02, 0x01] {
        // transport_private_data and adaptation_field_extension: a length
        // byte, then that many bytes.
        if flags & bit != 0 {
            if at >= end {
                return end;
            }
            at += 1 + usize::from(p[at]);
        }
    }
    if at > end || p[at..end].iter().any(|&b| b != 0xFF) {
        return end;
    }
    at
}

/// The packet that replaces `p`, keeping its header, adaptation field
/// flags and fields (PCR included) and carrying as many carried bytes as
/// fit, with stuffing for the rest (R-ST1402TIME-11). The continuity
/// counter is still the input's. With no byte left to carry the packet
/// becomes adaptation-field-only.
fn repack(p: &Packet, carry: &mut VecDeque<u8>) -> Packet {
    let min_end = min_af_end(p);
    let had_af = p[3] & 0x20 != 0;
    let mut q = [0xFFu8; PACKET_SIZE];
    q[..4].copy_from_slice(&p[..4]);
    let cap = PACKET_SIZE - min_end;
    let n = cap.min(carry.len());
    let stuffing = cap - n;
    if n == 0 {
        q[3] = (q[3] & 0xCF) | 0x20;
        q[4] = 183;
        if had_af && min_end > 5 {
            q[5..min_end].copy_from_slice(&p[5..min_end]);
        } else {
            q[5] = 0;
        }
        return q;
    }
    if had_af {
        if min_end > 5 {
            q[5..min_end].copy_from_slice(&p[5..min_end]);
            q[4] = (min_end - 5 + stuffing) as u8;
        } else {
            q[4] = stuffing as u8;
            if stuffing > 0 {
                q[5] = 0;
            }
        }
    } else if stuffing > 0 {
        q[3] |= 0x20;
        q[4] = (stuffing - 1) as u8;
        if stuffing >= 2 {
            q[5] = 0;
        }
    }
    for (d, b) in q[PACKET_SIZE - n..].iter_mut().zip(carry.drain(..n)) {
        *d = b;
    }
    q
}

/// The re-packetization of one video PES after an [`Edit`]: its packets
/// carry the changed byte stream in order, bytes that no longer fit wait
/// in `carry` for the stuffing of later packets, and what is left when the
/// PES ends goes into packets of its own (VBR) or null packets (CBR).
#[derive(Debug, Clone)]
pub(super) struct Reflow {
    pub pid: u16,
    edit: Edit,
    /// The new PES_packet_length bytes (R-ST1402TIME-14).
    new_len: Option<[u8; 2]>,
    /// Total input PES length when PES_packet_length is not 0.
    end: Option<usize>,
    /// Input PES offset of the next byte.
    pos: usize,
    pub carry: VecDeque<u8>,
    /// The PES's first packet went through.
    pub started: bool,
}

impl Reflow {
    pub fn new(pid: u16, edit: Edit, pes_length: u16) -> Result<Self, UserDataSkip> {
        let delta = edit.insert.len().saturating_sub(edit.remove_end - edit.at);
        let new_len = match pes_length {
            // Unbounded video PES stay unbounded (R-ST1402TIME-14).
            0 => None,
            n => match u16::try_from(usize::from(n) + delta) {
                Ok(n) => Some(n.to_be_bytes()),
                Err(_) => return Err(UserDataSkip::PesTooLong),
            },
        };
        Ok(Reflow {
            pid,
            edit,
            new_len,
            end: (pes_length != 0).then(|| 6 + usize::from(pes_length)),
            pos: 0,
            carry: VecDeque::new(),
            started: false,
        })
    }

    /// Bytes the edit adds.
    pub fn added(&self) -> usize {
        self.edit.insert.len() - (self.edit.remove_end - self.edit.at)
    }

    /// Whether the edit rewrote a message (else it inserted one).
    pub fn rewrite(&self) -> bool {
        self.edit.rewrite
    }

    /// The PES reached its declared length.
    pub fn ended(&self) -> bool {
        self.end.is_some_and(|e| self.pos >= e)
    }

    /// Nothing more to do: the PES ended and every byte went out.
    pub fn done(&self) -> bool {
        self.ended() && self.carry.is_empty()
    }

    /// The packet that replaces input packet `p` of the PES, whose payload
    /// starts at `payload_offset`; its continuity counter is the input's.
    pub fn take(&mut self, p: &Packet, payload_offset: usize) -> Packet {
        self.started = true;
        let payload = &p[payload_offset.min(PACKET_SIZE)..];
        let n0 = payload.len();
        let (s, e) = (self.pos, self.pos + n0);
        let mut buf = [0u8; MAX_PAYLOAD];
        buf[..n0].copy_from_slice(payload);
        if let Some(l) = self.new_len {
            for (o, v) in [(4, l[0]), (5, l[1])] {
                if (s..e).contains(&o) {
                    buf[o - s] = v;
                }
            }
        }
        let (a, b) = (self.edit.at, self.edit.remove_end);
        let x = a.clamp(s, e);
        self.carry.extend(buf[..x - s].iter().copied());
        if (s..e).contains(&a) {
            self.carry.extend(self.edit.insert.iter().copied());
        }
        let y = b.clamp(s, e).max(x);
        self.carry.extend(buf[y - s..n0].iter().copied());
        self.pos = e;
        repack(p, &mut self.carry)
    }

    /// A packet of the PID with up to 184 carried bytes and continuity
    /// counter `cc`; `None` when nothing is carried.
    pub fn carried_packet(&mut self, cc: u8) -> Option<Packet> {
        if self.carry.is_empty() {
            return None;
        }
        let mut q = [0xFFu8; PACKET_SIZE];
        q[0] = crate::SYNC_BYTE;
        q[1] = (self.pid >> 8) as u8 & 0x1F;
        q[2] = self.pid as u8;
        q[3] = 0x10 | (cc & 0x0F);
        let n = MAX_PAYLOAD.min(self.carry.len());
        let stuffing = MAX_PAYLOAD - n;
        if stuffing > 0 {
            q[3] |= 0x20;
            q[4] = (stuffing - 1) as u8;
            if stuffing >= 2 {
                q[5] = 0;
            }
        }
        for (d, b) in q[PACKET_SIZE - n..].iter_mut().zip(self.carry.drain(..n)) {
            *d = b;
        }
        Some(q)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn packet(af: Option<&[u8]>, payload: &[u8]) -> Packet {
        let mut p = [0xFFu8; PACKET_SIZE];
        p[..3].copy_from_slice(&[0x47, 0x01, 0x00]);
        match af {
            Some(af) => {
                p[3] = 0x37;
                let len = PACKET_SIZE - 5 - payload.len();
                p[4] = len as u8;
                p[5..5 + af.len()].copy_from_slice(af);
            }
            None => p[3] = 0x17,
        }
        p[PACKET_SIZE - payload.len()..].copy_from_slice(payload);
        p
    }

    #[test]
    fn stuffing_is_room_and_fields_stay() {
        // PCR flag and 6 PCR bytes, then stuffing.
        let pcr = [0x10, 1, 2, 3, 4, 5, 6];
        let p = packet(Some(&pcr), &[9; 100]);
        assert_eq!(min_af_end(&p), 12);
        let mut carry: VecDeque<u8> = (0..200).collect();
        let q = repack(&p, &mut carry);
        assert_eq!(q[..4], p[..4]);
        assert_eq!((q[4], &q[5..12]), (7, &pcr[..]));
        assert_eq!(q[12..], (0..176).map(|b| b as u8).collect::<Vec<u8>>()[..]);
        // 176 bytes fit; the rest is still carried.
        assert_eq!(carry.len(), 24);
        // Fewer bytes than room: stuffing again.
        let mut carry: VecDeque<u8> = [1, 2, 3].into_iter().collect();
        let q = repack(&packet(None, &[0; 184]), &mut carry);
        assert_eq!((q[3], q[4], q[5]), (0x37, 180, 0));
        assert_eq!(q[185..], [1, 2, 3]);
        // A field whose tail is not stuffing is kept whole.
        let mut odd = packet(Some(&[0x00]), &[0; 10]);
        odd[20] = 0x12;
        assert_eq!(min_af_end(&odd), PACKET_SIZE - 10);
    }

    /// Re-flow `packets` of one PES through an edit: the bytes carried and
    /// the packets added at its end.
    fn reflow(packets: &[Packet], edit: Edit, pes_length: u16) -> (Vec<u8>, usize) {
        let mut r = Reflow::new(0x100, edit, pes_length).unwrap();
        let mut got = Vec::new();
        for p in packets {
            let off = crate::PacketHeader::parse(p).unwrap().payload_offset;
            let q = r.take(p, off);
            assert_eq!(q[..3], p[..3]);
            got.extend_from_slice(crate::PacketHeader::parse(&q).unwrap().payload(&q));
        }
        assert_eq!(r.ended(), pes_length != 0);
        let mut added = 0;
        while let Some(q) = r.carried_packet(5) {
            added += 1;
            got.extend_from_slice(crate::PacketHeader::parse(&q).unwrap().payload(&q));
        }
        (got, added)
    }

    #[test]
    fn edit_moves_bytes_forward_in_order() {
        // A 400-byte PES, PES_packet_length 394; its last packet has 150
        // bytes of stuffing.
        let mut pes: Vec<u8> = (0..400).map(|i| i as u8).collect();
        pes[4..6].copy_from_slice(&394u16.to_be_bytes());
        let packets = [packet(None, &pes[..184]), packet(None, &pes[184..368]), packet(Some(&[0x00]), &pes[368..])];
        let insert = Edit { at: 20, remove_end: 20, insert: vec![0xEE; 30], rewrite: false };
        let mut want = pes.clone();
        want[4..6].copy_from_slice(&424u16.to_be_bytes());
        want.splice(20..20, [0xEE; 30]);
        assert_eq!(reflow(&packets, insert.clone(), 394), (want, 0));
        // Without stuffing, an unbounded PES: one packet more.
        let pes = &pes[..368];
        let packets = [packet(None, &pes[..184]), packet(None, &pes[184..])];
        let mut want = pes.to_vec();
        want.splice(20..20, [0xEE; 30]);
        assert_eq!(reflow(&packets, insert, 0), (want, 1));
        // A rewrite spanning two packets.
        let rewrite = Edit { at: 180, remove_end: 190, insert: vec![0xDD; 12], rewrite: true };
        let mut want = pes.to_vec();
        want.splice(180..190, [0xDD; 12]);
        assert_eq!(reflow(&packets, rewrite, 0), (want, 1));
    }
}
