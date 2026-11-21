//! The packets of one PID (ISO/IEC 13818-1 2.4.3.2 to 2.4.3.5).

use super::sink::{check_room, Packet, PacketSink};
use super::MuxError;
use crate::packet::{NULL_PID, PCR_WRAP};
use crate::{PACKET_SIZE, SYNC_BYTE};

/// A null packet: PID 0x1FFF, PUSI 0, scrambling '00', payload only, CC 0,
/// 184 bytes of 0xFF (R-ISO13818-017, R-ISO13818-018, R-ISO13818-007,
/// R-ISO13818-008). Null packets carry nothing and may be added or removed
/// anywhere (R-ISO13818-019).
pub const NULL_PACKET: Packet = null_packet();

const fn null_packet() -> Packet {
    let mut p = [0xFF; PACKET_SIZE];
    p[0] = SYNC_BYTE;
    p[1] = (NULL_PID >> 8) as u8;
    p[2] = NULL_PID as u8;
    p[3] = 0x10;
    p
}

/// Bytes after the 4-byte header: payload plus adaptation field
/// (R-ISO13818-014).
const PAYLOAD_ROOM: usize = PACKET_SIZE - 4;

/// Adaptation field contents for the first packet of a unit (ISO/IEC
/// 13818-1 2.4.3.5). The discontinuity_indicator is not here: it belongs to
/// the PID's continuity and time base, see [`TsWriter::mark_discontinuity`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
#[non_exhaustive]
pub struct AdaptationFlags {
    /// Program clock reference in 27 MHz ticks (taken modulo 2^33 x 300).
    /// It must be the T-STD arrival time of byte 10 of the packet, the one
    /// holding the last bit of program_clock_reference_base
    /// (R-ISO13818-025); computing that is the caller's job.
    pub pcr: Option<u64>,
    /// random_access_indicator: the PES starting in this packet holds an
    /// elementary stream access point (R-ISO13818-037). On a PCR_PID it may
    /// only be set together with a PCR (R-ISO13818-038).
    pub random_access: bool,
    /// elementary_stream_priority_indicator.
    pub es_priority: bool,
}

impl AdaptationFlags {
    /// No flags.
    pub fn new() -> Self {
        Self::default()
    }

    /// With a PCR (27 MHz).
    pub fn with_pcr(mut self, pcr: u64) -> Self {
        self.pcr = Some(pcr);
        self
    }

    /// With random_access_indicator set.
    pub fn with_random_access(mut self) -> Self {
        self.random_access = true;
        self
    }

    /// With elementary_stream_priority_indicator set.
    pub fn with_es_priority(mut self) -> Self {
        self.es_priority = true;
        self
    }

    /// Nothing to signal.
    pub fn is_empty(&self) -> bool {
        self.pcr.is_none() && !self.random_access && !self.es_priority
    }
}

/// Adaptation field bytes after adaptation_field_length needed to signal
/// `flags` (and a discontinuity): the flags byte plus 6 PCR bytes.
pub(crate) fn af_body(flags: &AdaptationFlags, discontinuity: bool) -> usize {
    if flags.is_empty() && !discontinuity {
        0
    } else {
        1 + if flags.pcr.is_some() { 6 } else { 0 }
    }
}

/// TS packets for `len` bytes of PES or pointer_field plus section, when
/// the first packet carries an adaptation field body of `first_af_body`
/// bytes.
pub(crate) fn packet_count(len: usize, first_af_body: usize) -> usize {
    let first = PAYLOAD_ROOM - if first_af_body > 0 { 1 + first_af_body } else { 0 };
    if len <= first {
        1
    } else {
        1 + (len - first + PAYLOAD_ROOM - 1) / PAYLOAD_ROOM
    }
}

fn put_pcr(dst: &mut [u8], pcr: u64) {
    // R-ISO13818-042: base = (t / 300) mod 2^33, extension = t mod 300;
    // the six reserved bits between them are '111111' (R-ISO13818-103).
    let pcr = pcr % PCR_WRAP;
    let (b, e) = (pcr / 300, pcr % 300);
    dst[0] = (b >> 25) as u8;
    dst[1] = (b >> 17) as u8;
    dst[2] = (b >> 9) as u8;
    dst[3] = (b >> 1) as u8;
    dst[4] = ((b & 1) << 7) as u8 | 0x7E | (e >> 8) as u8;
    dst[5] = e as u8;
}

/// Bytes of several slices read as one.
struct Gather<'a> {
    parts: &'a [&'a [u8]],
    part: usize,
    at: usize,
    remaining: usize,
}

impl<'a> Gather<'a> {
    fn new(parts: &'a [&'a [u8]]) -> Self {
        Gather { parts, part: 0, at: 0, remaining: parts.iter().map(|p| p.len()).sum() }
    }

    fn fill(&mut self, mut dst: &mut [u8]) {
        while !dst.is_empty() {
            let cur = &self.parts[self.part][self.at..];
            if cur.is_empty() {
                self.part += 1;
                self.at = 0;
                continue;
            }
            let n = cur.len().min(dst.len());
            dst[..n].copy_from_slice(&cur[..n]);
            self.at += n;
            self.remaining -= n;
            dst = &mut dst[n..];
        }
    }

    /// The first `N` bytes, if there are that many.
    fn peek<const N: usize>(&self) -> Option<[u8; N]> {
        let mut out = [0u8; N];
        let mut i = 0;
        for part in self.parts {
            for &b in *part {
                if i == N {
                    return Some(out);
                }
                out[i] = b;
                i += 1;
            }
        }
        (i == N).then_some(out)
    }
}

/// Writes the packets of one PID: keeps its continuity counter, sets
/// payload_unit_start_indicator, writes adaptation fields and stuffing.
///
/// Every packet it writes has transport_error_indicator 0,
/// transport_priority 0 and transport_scrambling_control '00'
/// (R-ISO13818-003, R-ISO13818-007), and never adaptation_field_control
/// '00' (R-ISO13818-008).
///
/// ```
/// use mpegts::mux::{AdaptationFlags, TsWriter};
///
/// let mut writer = TsWriter::new(0x100)?;
/// let pes = [0, 0, 1, 0xE0, 0, 0, 0x80, 0, 0, 0xAA];
/// let mut out = Vec::new();
/// writer.write_pes(&[&pes], &AdaptationFlags::new().with_pcr(27_000_000), &mut out)?;
/// let h = mpegts::PacketHeader::parse(out[..188].try_into().unwrap()).unwrap();
/// assert_eq!((h.pid, h.payload_unit_start, h.pcr()), (0x100, true, Some(27_000_000)));
/// assert_eq!(writer.continuity_counter(), 1);
/// # Ok::<(), mpegts::mux::MuxError>(())
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TsWriter {
    pid: u16,
    /// Continuity counter of the next packet with payload.
    cc: u8,
    /// Set discontinuity_indicator in the next packet.
    discontinuity: bool,
}

impl TsWriter {
    /// A writer for `pid`, any PID but the null PID 0x1FFF (use
    /// [`NULL_PACKET`] for those). Its continuity counter starts at 0
    /// (D-ISO13818-10).
    pub fn new(pid: u16) -> Result<Self, MuxError> {
        if pid >= NULL_PID {
            return Err(MuxError::InvalidPid {
                pid,
                reason: "must be at most 0x1FFE; null packets are written as NULL_PACKET",
            });
        }
        Ok(TsWriter { pid, cc: 0, discontinuity: false })
    }

    /// The PID.
    pub fn pid(&self) -> u16 {
        self.pid
    }

    /// The continuity_counter the next packet with payload gets.
    pub fn continuity_counter(&self) -> u8 {
        self.cc
    }

    /// Continue an existing sequence: the next packet with payload gets
    /// `next & 0x0F`. Taking over a PID from an input stream continues its
    /// counter this way (R-ISO13818-013). A jump in the counter is legal
    /// only in a packet with discontinuity_indicator set
    /// ([`mark_discontinuity`](Self::mark_discontinuity)).
    pub fn set_continuity_counter(&mut self, next: u8) {
        self.cc = next & 0x0F;
    }

    /// Set discontinuity_indicator in the next packet written on this PID:
    /// for a continuity counter jump (R-ISO13818-013, R-ISO13818-032) or,
    /// on a PCR_PID, in the packet with the first PCR of a new time base
    /// (R-ISO13818-029). After a counter discontinuity the next elementary
    /// stream byte must start an access point (R-ISO13818-034), which every
    /// [`PesWriter`](crate::mux::PesWriter) unit does.
    pub fn mark_discontinuity(&mut self) {
        self.discontinuity = true;
    }

    /// A discontinuity_indicator is waiting for the next packet.
    pub fn discontinuity_pending(&self) -> bool {
        self.discontinuity
    }

    /// Packets [`write_pes`](Self::write_pes) needs for a PES packet of
    /// `len` bytes with `flags` on its first packet.
    pub fn packets_for_pes(&self, len: usize, flags: &AdaptationFlags) -> usize {
        packet_count(len, af_body(flags, self.discontinuity))
    }

    /// Packets [`write_section`](Self::write_section) needs for a section of
    /// `len` bytes.
    pub fn packets_for_section(&self, len: usize) -> usize {
        packet_count(1 + len, af_body(&AdaptationFlags::default(), self.discontinuity))
    }

    /// Write one PES packet, given as the concatenation of `parts` (for
    /// example a header and a payload, so neither needs copying).
    ///
    /// The PES starts at the first payload byte of a packet with
    /// payload_unit_start_indicator 1 (R-ISO13818-004, R-ISO13818-048) and
    /// `flags` go in that packet's adaptation field. The last packet is
    /// filled by enlarging its adaptation field, never with payload bytes
    /// (R-ISO13818-023), so the next PES starts in a new packet
    /// (R-ISO13818-005). Returns the number of packets written.
    pub fn write_pes<S: PacketSink + ?Sized>(
        &mut self,
        parts: &[&[u8]],
        flags: &AdaptationFlags,
        sink: &mut S,
    ) -> Result<usize, MuxError> {
        let src = Gather::new(parts);
        if src.peek::<6>().map_or(true, |b| b[..3] != [0, 0, 1]) {
            return Err(MuxError::InvalidUnit("a PES packet starts with 00 00 01 and has at least 6 bytes"));
        }
        let len = src.remaining;
        check_room(sink, self.packets_for_pes(len, flags))?;
        Ok(self.emit(parts, flags, false, sink))
    }

    /// Write one PSI section: pointer_field 0x00 and the section from the
    /// first packet's payload on (R-ISO13818-006, R-ISO13818-074), then
    /// 0xFF to the end of the last packet (R-ISO13818-073). Every section
    /// starts a new packet, so sections on one PID never interleave
    /// (R-ISO13818-075). `section` must be a whole section of at most 4096
    /// bytes whose section_length matches its length. Returns the number of
    /// packets written.
    pub fn write_section<S: PacketSink + ?Sized>(&mut self, section: &[u8], sink: &mut S) -> Result<usize, MuxError> {
        if section.len() < 3 {
            return Err(MuxError::InvalidSection("shorter than a section header"));
        }
        let declared = (usize::from(section[1] & 0x0F) << 8) | usize::from(section[2]);
        if section.len() != 3 + declared {
            return Err(MuxError::InvalidSection("section_length does not match the bytes given"));
        }
        // 4093 is the largest section_length of any section (2.4.4.11).
        if declared > 4093 {
            return Err(MuxError::InvalidSection("section_length over 4093"));
        }
        check_room(sink, self.packets_for_section(section.len()))?;
        Ok(self.emit(&[&[0x00], section], &AdaptationFlags::default(), true, sink))
    }

    /// Write a packet with an adaptation field and no payload
    /// (adaptation_field_control '10', adaptation_field_length 183,
    /// R-ISO13818-021), for example to carry a PCR when no payload is due.
    /// Its continuity_counter repeats the previous packet's
    /// (R-ISO13818-010).
    pub fn write_adaptation_only<S: PacketSink + ?Sized>(
        &mut self,
        flags: &AdaptationFlags,
        sink: &mut S,
    ) -> Result<usize, MuxError> {
        check_room(sink, 1)?;
        let mut p = [0xFF; PACKET_SIZE];
        let discontinuity = std::mem::take(&mut self.discontinuity);
        p[0] = SYNC_BYTE;
        p[1] = (self.pid >> 8) as u8;
        p[2] = self.pid as u8;
        p[3] = 0x20 | (self.cc.wrapping_sub(1) & 0x0F);
        p[4] = 183;
        p[5] = flags_byte(flags, discontinuity);
        if let Some(pcr) = flags.pcr {
            put_pcr(&mut p[6..12], pcr);
        }
        sink.packet(&p);
        Ok(1)
    }

    /// Split `parts` into packets. The caller has checked the sink's room.
    /// `psi` selects 0xFF fill after the data instead of adaptation field
    /// stuffing.
    pub(crate) fn emit<S: PacketSink + ?Sized>(
        &mut self,
        parts: &[&[u8]],
        flags: &AdaptationFlags,
        psi: bool,
        sink: &mut S,
    ) -> usize {
        let mut src = Gather::new(parts);
        debug_assert!(src.remaining > 0);
        let mut first = true;
        let mut count = 0;
        loop {
            let mut p = [0xFF; PACKET_SIZE];
            let discontinuity = std::mem::take(&mut self.discontinuity);
            let f = if first { *flags } else { AdaptationFlags::default() };
            let body = af_body(&f, discontinuity);
            let af_bytes = if body > 0 { 1 + body } else { 0 };
            let room = PAYLOAD_ROOM - af_bytes;
            let n = src.remaining.min(room);
            // A PES packet that ends short of the packet end is padded by
            // the adaptation field (R-ISO13818-023); a section by 0xFF after
            // it (R-ISO13818-073), already in `p`.
            let stuffing = if psi { 0 } else { room - n };
            let af_len = af_bytes + stuffing;
            p[0] = SYNC_BYTE;
            p[1] = u8::from(first) << 6 | (self.pid >> 8) as u8;
            p[2] = self.pid as u8;
            p[3] = if af_len > 0 { 0x30 } else { 0x10 } | self.cc;
            if af_len > 0 {
                // adaptation_field_length 0 is one stuffing byte without a
                // flags byte; otherwise flags, the PCR, then 0xFF stuffing
                // bytes (R-ISO13818-022, R-ISO13818-024).
                p[4] = (af_len - 1) as u8;
                if af_len > 1 {
                    p[5] = flags_byte(&f, discontinuity);
                    if let Some(pcr) = f.pcr {
                        put_pcr(&mut p[6..12], pcr);
                    }
                }
            }
            src.fill(&mut p[4 + af_len..4 + af_len + n]);
            // +1 per packet with payload, wrapping 15 -> 0 (R-ISO13818-009).
            self.cc = (self.cc + 1) & 0x0F;
            sink.packet(&p);
            count += 1;
            first = false;
            if src.remaining == 0 {
                return count;
            }
        }
    }
}

fn flags_byte(flags: &AdaptationFlags, discontinuity: bool) -> u8 {
    u8::from(discontinuity) << 7
        | u8::from(flags.random_access) << 6
        | u8::from(flags.es_priority) << 5
        | u8::from(flags.pcr.is_some()) << 4
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PacketHeader;

    fn packets(out: &[u8]) -> Vec<(PacketHeader, &[u8])> {
        out.chunks(188)
            .map(|c| {
                let p: &[u8; 188] = c.try_into().unwrap();
                let h = PacketHeader::parse(p).unwrap();
                (h, &c[h.payload_offset..])
            })
            .collect()
    }

    #[test]
    fn null_packet_bytes() {
        assert_eq!(NULL_PACKET[..4], [0x47, 0x1F, 0xFF, 0x10]);
        assert!(NULL_PACKET[4..].iter().all(|&b| b == 0xFF));
        assert!(TsWriter::new(0x1FFF).is_err());
        assert!(TsWriter::new(0x1FFE).is_ok());
    }

    #[test]
    fn every_pes_length_fills_packets_exactly() {
        for len in 6..=800 {
            for flags in [AdaptationFlags::new(), AdaptationFlags::new().with_pcr(123_456_789).with_random_access()] {
                let pes: Vec<u8> = [0, 0, 1, 0xE0].iter().copied().chain((4..len).map(|i| i as u8)).collect();
                let mut w = TsWriter::new(0x31).unwrap();
                let mut out = Vec::new();
                let n = w.write_pes(&[&pes[..3], &pes[3..]], &flags, &mut out).unwrap();
                assert_eq!(n, w.packets_for_pes(len, &flags));
                assert_eq!(out.len(), n * 188);
                let mut joined = Vec::new();
                for (i, (h, payload)) in packets(&out).into_iter().enumerate() {
                    assert_eq!((h.pid, h.payload_unit_start, h.continuity_counter), (0x31, i == 0, i as u8 & 0xF));
                    assert_ne!(h.adaptation_field_control, 0b10);
                    if let Some(af) = h.adaptation_field {
                        assert!(af.length <= 182, "len {len}");
                    }
                    if i == 0 {
                        assert_eq!(h.pcr(), flags.pcr);
                        assert_eq!(h.random_access(), flags.random_access);
                    } else {
                        assert!(h.pcr().is_none() && !h.random_access());
                    }
                    joined.extend_from_slice(payload);
                }
                assert_eq!(joined, pes, "len {len}");
            }
        }
    }

    #[test]
    fn stuffing_forms() {
        // 183 PES bytes leave one byte: adaptation_field_length 0.
        let mut pes = vec![0xAB; 183];
        pes[..3].copy_from_slice(&[0, 0, 1]);
        let mut out = Vec::new();
        TsWriter::new(0x100).unwrap().write_pes(&[&pes], &AdaptationFlags::new(), &mut out).unwrap();
        assert_eq!(out[..5], [0x47, 0x41, 0x00, 0x30, 0x00]);
        // 182 bytes: length 1, flags byte 0.
        out.clear();
        TsWriter::new(0x100).unwrap().write_pes(&[&pes[..182]], &AdaptationFlags::new(), &mut out).unwrap();
        assert_eq!(out[..6], [0x47, 0x41, 0x00, 0x30, 0x01, 0x00]);
        // 180 bytes: length 3, flags 0, two 0xFF.
        out.clear();
        TsWriter::new(0x100).unwrap().write_pes(&[&pes[..180]], &AdaptationFlags::new(), &mut out).unwrap();
        assert_eq!(out[..8], [0x47, 0x41, 0x00, 0x30, 0x03, 0x00, 0xFF, 0xFF]);
        assert_eq!(out[8..11], [0, 0, 1]);
    }

    #[test]
    fn worked_example_from_iso13818_notes() {
        // iso13818.md 3.11: a 200-byte ST 0601 packet at PTS 126000 on PID
        // 0x0101 is a 219-byte PES in two TS packets.
        let mut pes = vec![0x00, 0x00, 0x01, 0xFC, 0x00, 0xD5, 0x85, 0x80, 0x05, 0x21, 0x00, 0x07, 0xD8, 0x61];
        pes.extend_from_slice(&[0x00, 0x00, 0xDF, 0x00, 0xC8]);
        pes.extend((0..200).map(|i| i as u8));
        let mut w = TsWriter::new(0x0101).unwrap();
        w.set_continuity_counter(0xC);
        let mut out = Vec::new();
        assert_eq!(w.write_pes(&[&pes], &AdaptationFlags::new(), &mut out).unwrap(), 2);
        assert_eq!(out[..4], [0x47, 0x41, 0x01, 0x1C]);
        assert_eq!(out[4..188], pes[..184]);
        assert_eq!(out[188..194], [0x47, 0x01, 0x01, 0x3D, 0x94, 0x00]);
        assert!(out[194..188 + 153].iter().all(|&b| b == 0xFF));
        assert_eq!(out[188 + 153..], pes[184..]);
    }

    #[test]
    fn continuity_wraps_and_adaptation_only_packets_do_not_count() {
        let mut w = TsWriter::new(0x200).unwrap();
        let pes = [0, 0, 1, 0xBD, 0, 3, 0x81, 0, 0];
        let mut out = Vec::new();
        for _ in 0..17 {
            w.write_pes(&[&pes], &AdaptationFlags::new(), &mut out).unwrap();
        }
        assert_eq!(w.continuity_counter(), 1);
        w.write_adaptation_only(&AdaptationFlags::new().with_pcr(PCR_WRAP + 5), &mut out).unwrap();
        assert_eq!(w.continuity_counter(), 1);
        let all = packets(&out);
        let last = all.last().unwrap().0;
        assert_eq!(last.adaptation_field_control, 0b10);
        assert_eq!(last.continuity_counter, 0, "repeats the previous packet's counter");
        assert_eq!(last.adaptation_field.unwrap().length, 183);
        assert_eq!(last.pcr(), Some(5), "PCR taken modulo 2^33 x 300");
        let ccs: Vec<u8> = all[..17].iter().map(|(h, _)| h.continuity_counter).collect();
        assert_eq!(ccs, [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 0]);
    }

    #[test]
    fn discontinuity_goes_in_the_next_packet_only() {
        let mut w = TsWriter::new(0x200).unwrap();
        w.set_continuity_counter(9);
        w.mark_discontinuity();
        assert!(w.discontinuity_pending());
        let pes = vec![0u8; 400];
        let mut pes = pes;
        pes[2] = 1;
        let mut out = Vec::new();
        w.write_pes(&[&pes], &AdaptationFlags::new(), &mut out).unwrap();
        let all = packets(&out);
        assert!(all[0].0.discontinuity());
        assert_eq!(all[0].0.continuity_counter, 9);
        assert!(all[1..].iter().all(|(h, _)| !h.discontinuity()));
        assert!(!w.discontinuity_pending());
    }

    #[test]
    fn sections_use_pointer_field_and_ff_fill() {
        let mut section = vec![0x02, 0xB0, 0x00];
        section.extend(vec![0x11; 300]);
        let len = section.len() - 3;
        section[1] |= (len >> 8) as u8;
        section[2] = len as u8;
        let mut w = TsWriter::new(0x1000).unwrap();
        let mut out = Vec::new();
        assert_eq!(w.write_section(&section, &mut out).unwrap(), 2);
        let all = packets(&out);
        assert!(all[0].0.payload_unit_start && !all[1].0.payload_unit_start);
        assert_eq!(all[0].1[0], 0, "pointer_field");
        assert_eq!(all[0].1[1..], section[..183]);
        assert_eq!(all[1].1[..section.len() - 183], section[183..]);
        assert!(all[1].1[section.len() - 183..].iter().all(|&b| b == 0xFF));
        assert!(all.iter().all(|(h, _)| h.adaptation_field.is_none()));
        // Malformed sections are refused.
        assert!(w.write_section(&section[..100], &mut out).is_err());
        assert!(w.write_section(&[0x00], &mut out).is_err());
    }

    #[test]
    fn short_sink_fails_without_writing() {
        let mut w = TsWriter::new(0x100).unwrap();
        let pes = vec![0, 0, 1, 0xE0, 0, 0, 0x80, 0, 0];
        let mut pes = pes;
        pes.resize(400, 0x55);
        let mut buf = [0u8; 188 * 2];
        let mut sink = crate::mux::SliceSink::new(&mut buf);
        assert_eq!(
            w.write_pes(&[&pes], &AdaptationFlags::new(), &mut sink),
            Err(MuxError::BufferTooSmall { needed: 3 * 188 })
        );
        assert!(sink.is_empty());
        assert_eq!(w.continuity_counter(), 0);
        assert!(w.write_pes(&[&[1, 2, 3]], &AdaptationFlags::new(), &mut Vec::new()).is_err());
    }
}
