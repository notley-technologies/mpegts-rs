//! PES packets of one elementary stream (ISO/IEC 13818-1 2.4.3.6, 2.4.3.7),
//! in the carriage its stream calls for.

use super::cells::{CellFragment, CellHeader, CellNumbering, CELL_HEADER_LEN, MAX_CELL_DATA};
use super::sink::{check_room, PacketSink};
use super::spec::StreamSpec;
use super::ts::{af_body, packet_count, AdaptationFlags, TsWriter};
use super::MuxError;
use crate::packet::{NULL_PID, PTS_WRAP};
use crate::PesHeader;

/// Largest PES_packet_length (a 16-bit field, R-ISO13818-050).
const MAX_PES_LENGTH: usize = 65_535;

/// Most KLV bytes in one asynchronous KLV PES packet: 65,535 - 3 (the
/// flags and PES_header_data_length bytes that PES_packet_length also
/// counts) = 65,532. Only larger KLV items are split (DECISIONS X-4).
pub const MAX_ASYNC_PES_DATA: usize = MAX_PES_LENGTH - 3;

/// Longest PES header written: 9 bytes plus PTS and DTS.
const MAX_HEADER: usize = 19;

/// How a stream's units become PES packets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Carriage {
    /// One unit per PES packet: video, audio, private data. A unit too
    /// large for one PES packet goes into a PES_packet_length 0 packet for
    /// video stream_ids (0xE0-0xEF), the only streams allowed one
    /// (R-ISO13818-051); for other streams it is split over several PES
    /// packets, the first with the timestamps and data_alignment_indicator,
    /// the others without.
    Pes,
    /// Synchronous metadata in metadata access unit cells (ISO/IEC 13818-1
    /// 2.12.4, MISB ST 1402.2 synchronous method): stream_id 0xFC, a PTS on
    /// every PES packet, one access unit per PES packet in one cell, or,
    /// above [`MAX_CELL_DATA`] bytes, one fragment cell per PES packet,
    /// each with the access unit's PTS (R-ST1402TIME-33, DECISIONS
    /// section 2).
    MetadataCells {
        /// metadata_service_id written in each cell unless the unit names
        /// another one ([`PesUnit::with_service`]).
        service_id: u8,
    },
    /// Asynchronous KLV (SMPTE RP 217, MISB ST 1402.2 asynchronous method):
    /// stream_id 0xBD, no PTS, each PES packet starting with a KLV item.
    /// One unit per PES packet; units above [`MAX_ASYNC_PES_DATA`] bytes are
    /// split, with data_alignment_indicator only on the first PES packet
    /// (DECISIONS X-4, R-ST1402TIME-41, R-ST1402TIME-46).
    AsyncKlv,
}

/// One unit to write: an access unit (video frame, audio frame, KLV packet)
/// or any payload, with its timestamps and the adaptation field flags of
/// its first TS packet.
///
/// ```
/// use mpegts::mux::PesUnit;
///
/// let frame = [0u8; 100];
/// let unit = PesUnit::new(&frame).with_pts(183_003).with_dts(180_000).with_random_access();
/// assert_eq!(unit.pts, Some(183_003));
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct PesUnit<'a> {
    /// The payload.
    pub data: &'a [u8],
    /// Presentation timestamp, 90 kHz (taken modulo 2^33,
    /// R-ISO13818-065).
    pub pts: Option<u64>,
    /// Decoding timestamp, 90 kHz. Written only with a PTS and only when it
    /// differs from it (R-ISO13818-066, R-ISO13818-067).
    pub dts: Option<u64>,
    /// data_alignment_indicator, for [`Carriage::Pes`] (R-ISO13818-053).
    /// The KLV carriages set it themselves.
    pub data_alignment: bool,
    /// Adaptation field flags of the first TS packet.
    pub adaptation: AdaptationFlags,
    /// The metadata_service_id of this access unit, for
    /// [`Carriage::MetadataCells`] streams carrying several services.
    pub service_id: Option<u8>,
}

impl<'a> PesUnit<'a> {
    /// A unit with no timestamps or flags.
    pub fn new(data: &'a [u8]) -> Self {
        PesUnit { data, pts: None, dts: None, data_alignment: false, adaptation: AdaptationFlags::default(), service_id: None }
    }

    /// With a PTS.
    pub fn with_pts(mut self, pts: u64) -> Self {
        self.pts = Some(pts);
        self
    }

    /// With a DTS.
    pub fn with_dts(mut self, dts: u64) -> Self {
        self.dts = Some(dts);
        self
    }

    /// With data_alignment_indicator set.
    pub fn with_data_alignment(mut self) -> Self {
        self.data_alignment = true;
        self
    }

    /// With a PCR (27 MHz) in the first TS packet.
    pub fn with_pcr(mut self, pcr: u64) -> Self {
        self.adaptation.pcr = Some(pcr);
        self
    }

    /// With random_access_indicator in the first TS packet.
    pub fn with_random_access(mut self) -> Self {
        self.adaptation.random_access = true;
        self
    }

    /// With these adaptation field flags in the first TS packet.
    pub fn with_adaptation(mut self, flags: AdaptationFlags) -> Self {
        self.adaptation = flags;
        self
    }

    /// For another metadata service of a [`Carriage::MetadataCells`]
    /// stream.
    pub fn with_service(mut self, service_id: u8) -> Self {
        self.service_id = Some(service_id);
        self
    }
}

#[derive(Debug, Clone, Copy)]
enum Mode {
    Pes { optional_header: bool, video: bool },
    Cells { service_id: u8 },
    Async,
}

/// A validated unit: everything needed to lay out its PES packets.
#[derive(Debug, Clone, Copy)]
struct Plan {
    stream_id: u8,
    mode: Mode,
    pts: Option<u64>,
    dts: Option<u64>,
    data_alignment: bool,
    flags: AdaptationFlags,
    len: usize,
    sequence: u8,
    numbering: CellNumbering,
}

/// One PES packet of a plan.
struct Segment {
    header: [u8; MAX_HEADER],
    header_len: usize,
    cell: Option<[u8; CELL_HEADER_LEN]>,
    start: usize,
    end: usize,
}

impl Segment {
    fn len(&self) -> usize {
        self.header_len + self.cell.map_or(0, |c| c.len()) + self.end - self.start
    }
}

struct Segments {
    plan: Plan,
    offset: usize,
    index: usize,
    done: bool,
}

impl Plan {
    fn segments(self) -> Segments {
        Segments { plan: self, offset: 0, index: 0, done: false }
    }

    fn cells(&self) -> usize {
        (self.len + MAX_CELL_DATA - 1) / MAX_CELL_DATA
    }
}

impl Iterator for Segments {
    type Item = Segment;

    fn next(&mut self) -> Option<Segment> {
        if self.done {
            return None;
        }
        let p = self.plan;
        let first = self.index == 0;
        let remaining = p.len - self.offset;
        let mut seg = Segment { header: [0; MAX_HEADER], header_len: 0, cell: None, start: self.offset, end: 0 };
        let n = match p.mode {
            Mode::Pes { optional_header, video } => {
                let (pts, dts, dai) = if first { (p.pts, p.dts, p.data_alignment) } else { (None, None, false) };
                // Bytes PES_packet_length counts before the data.
                let extra = if optional_header { 3 + timestamps_len(pts, dts) } else { 0 };
                let max = MAX_PES_LENGTH - extra;
                let (n, length) = if remaining <= max {
                    (remaining, extra + remaining)
                } else if first && video {
                    // Unbounded, allowed only for video (R-ISO13818-051).
                    (remaining, 0)
                } else {
                    (max, MAX_PES_LENGTH)
                };
                seg.header_len = write_header(&mut seg.header, p.stream_id, length, optional_header, dai, pts, dts);
                n
            }
            Mode::Cells { service_id } => {
                let n = remaining.min(MAX_CELL_DATA);
                // Every PES packet of the access unit, fragments included,
                // carries its PTS (R-ISO13818-138, DECISIONS section 2), and
                // its first payload byte starts a cell (R-ISO13818-133).
                // data_alignment_indicator is 1 when the PES packet begins
                // an access unit (a complete or first-fragment cell) and 0
                // on the continuation fragments, as ST 1402-20/-21 rule for
                // asynchronous KLV and like the cell random_access_indicator
                // (DECISIONS X-6, D-ISO13818-08).
                let length = 3 + 5 + CELL_HEADER_LEN + n;
                seg.header_len = write_header(&mut seg.header, p.stream_id, length, true, first, p.pts, None);
                let fragment = CellFragment::of(self.index, p.cells());
                // sequence_number: per stream, +1 per cell modulo 256
                // (R-ISO13818-140, DECISIONS X-8); or, as a workaround,
                // from 0 at the start of the access unit, +1 per fragment.
                let sequence = match p.numbering {
                    CellNumbering::PerAccessUnit => self.index as u8,
                    CellNumbering::PerStream => p.sequence.wrapping_add(self.index as u8),
                };
                seg.cell = Some(CellHeader::new(service_id, sequence, fragment, n as u16).to_bytes());
                n
            }
            Mode::Async => {
                let n = remaining.min(MAX_ASYNC_PES_DATA);
                // data_alignment_indicator 1 exactly when the PES packet
                // begins a KLV item (R-ST1402TIME-41); no PTS or DTS
                // (R-ST1402TIME-42).
                seg.header_len = write_header(&mut seg.header, p.stream_id, 3 + n, true, first, None, None);
                n
            }
        };
        self.offset += n;
        seg.end = self.offset;
        self.index += 1;
        self.done = self.offset >= p.len;
        Some(seg)
    }
}

fn timestamps_len(pts: Option<u64>, dts: Option<u64>) -> usize {
    match (pts, dts) {
        (Some(_), Some(_)) => 10,
        (Some(_), None) => 5,
        _ => 0,
    }
}

/// A 5-byte PTS or DTS field: 4-bit prefix, the 33 bits in three groups,
/// each followed by a marker bit '1' (R-ISO13818-063, R-ISO13818-064).
fn put_timestamp(dst: &mut [u8], prefix: u8, t: u64) {
    let t = t % PTS_WRAP;
    dst[0] = prefix << 4 | ((t >> 29) & 0x0E) as u8 | 1;
    dst[1] = (t >> 22) as u8;
    dst[2] = ((t >> 14) & 0xFE) as u8 | 1;
    dst[3] = (t >> 7) as u8;
    dst[4] = ((t << 1) & 0xFE) as u8 | 1;
}

/// Write a PES header into `h`; returns its length. `length` is
/// PES_packet_length.
fn write_header(
    h: &mut [u8; MAX_HEADER],
    stream_id: u8,
    length: usize,
    optional_header: bool,
    data_alignment: bool,
    pts: Option<u64>,
    dts: Option<u64>,
) -> usize {
    debug_assert!(length <= MAX_PES_LENGTH);
    h[..3].copy_from_slice(&[0, 0, 1]);
    h[3] = stream_id;
    h[4..6].copy_from_slice(&(length as u16).to_be_bytes());
    if !optional_header {
        return 6;
    }
    // '10', PES_scrambling_control '00' (R-ISO13818-052), PES_priority 0,
    // data_alignment_indicator, copyright 0, original_or_copy 1
    // (st1402-time.md D-8, R-ST1402TIME-92, D-ISO13818-08).
    h[6] = 0x81 | u8::from(data_alignment) << 2;
    // PTS_DTS_flags; ESCR, ES_rate, DSM_trick_mode, additional_copy_info,
    // PES_CRC and PES_extension flags 0; no header stuffing
    // (R-ISO13818-055, R-ISO13818-056).
    let ts_len = timestamps_len(pts, dts);
    h[7] = match (pts, dts) {
        (Some(_), Some(_)) => 0xC0,
        (Some(_), None) => 0x80,
        _ => 0x00,
    };
    h[8] = ts_len as u8;
    if let Some(pts) = pts {
        put_timestamp(&mut h[9..14], if dts.is_some() { 0b0011 } else { 0b0010 }, pts);
    }
    if let Some(dts) = dts {
        put_timestamp(&mut h[14..19], 0b0001, dts);
    }
    9 + ts_len
}

/// Writes the PES packets of one elementary stream on its own PID.
///
/// ```
/// use mpegts::mux::{PesUnit, PesWriter, SliceSink};
///
/// // Synchronous KLV on PID 0x101, service 0, written into a fixed buffer.
/// let mut writer = PesWriter::klv_sync(0x101, 0)?;
/// let klv = [0x06, 0x0E, 0x2B, 0x34, 0x02, 0x0B, 0x01, 0x01];
/// let mut buf = [0u8; 188 * 2];
/// let mut sink = SliceSink::new(&mut buf);
/// let packets = writer.write(&PesUnit::new(&klv).with_pts(900_000), &mut sink)?;
/// assert_eq!(packets, 1);
/// // One cell counter per PID (DECISIONS X-8): the next cell is number 1.
/// assert_eq!(writer.sequence_number(), 1);
/// # Ok::<(), mpegts::mux::MuxError>(())
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PesWriter {
    ts: TsWriter,
    stream_id: u8,
    carriage: Carriage,
    /// The per-stream cell counter ([`CellNumbering::PerStream`]).
    sequence: u8,
    numbering: CellNumbering,
}

impl PesWriter {
    /// A writer for the stream `spec` describes, on `pid`.
    pub fn new(pid: u16, spec: &StreamSpec) -> Result<Self, MuxError> {
        Self::with_carriage(pid, spec.stream_id, spec.carriage)
    }

    /// A writer for `stream_id` PES packets in `carriage` on `pid`, without
    /// building a [`StreamSpec`] (and so without allocating).
    pub fn with_carriage(pid: u16, stream_id: u8, carriage: Carriage) -> Result<Self, MuxError> {
        // Elementary PIDs are 0x0010-0x1FFE (R-ISO13818-015).
        if !(0x0010..NULL_PID).contains(&pid) {
            return Err(MuxError::InvalidPid { pid, reason: "elementary stream PIDs are 0x0010 to 0x1FFE" });
        }
        StreamSpec::check_pes(stream_id, carriage)?;
        Ok(PesWriter { ts: TsWriter::new(pid)?, stream_id, carriage, sequence: 0, numbering: CellNumbering::PerStream })
    }

    /// Synchronous KLV on `pid` (stream_id 0xFC, metadata cells for
    /// `service_id`), as [`StreamSpec::klv_sync`] describes.
    pub fn klv_sync(pid: u16, service_id: u8) -> Result<Self, MuxError> {
        Self::with_carriage(pid, 0xFC, Carriage::MetadataCells { service_id })
    }

    /// Asynchronous KLV on `pid` (stream_id 0xBD), as
    /// [`StreamSpec::klv_async`] describes.
    pub fn klv_async(pid: u16) -> Result<Self, MuxError> {
        Self::with_carriage(pid, 0xBD, Carriage::AsyncKlv)
    }

    /// The PID.
    pub fn pid(&self) -> u16 {
        self.ts.pid()
    }

    /// The PES stream_id.
    pub fn stream_id(&self) -> u8 {
        self.stream_id
    }

    /// The carriage.
    pub fn carriage(&self) -> Carriage {
        self.carriage
    }

    /// The PID's packet writer, for its continuity counter.
    pub fn ts(&self) -> &TsWriter {
        &self.ts
    }

    /// The PID's packet writer: set its continuity counter, mark a
    /// discontinuity, or write an adaptation-field-only packet (for
    /// example a PCR on a video PID between frames).
    pub fn ts_mut(&mut self) -> &mut TsWriter {
        &mut self.ts
    }

    /// The sequence_number the next access unit's first cell gets: the
    /// stream's counter with [`CellNumbering::PerStream`] (the default),
    /// always 0 with [`CellNumbering::PerAccessUnit`].
    pub fn sequence_number(&self) -> u8 {
        match self.numbering {
            CellNumbering::PerAccessUnit => 0,
            CellNumbering::PerStream => self.sequence,
        }
    }

    /// Set the per-stream counter ([`CellNumbering::PerStream`]): when
    /// taking over a metadata PID numbered per stream, start from the last
    /// input sequence_number + 1 so receivers see no loss (D-ISO13818-07).
    /// No effect on cells numbered per access unit.
    pub fn set_sequence_number(&mut self, next: u8) {
        self.sequence = next;
    }

    /// How cell sequence numbers are counted.
    pub fn cell_numbering(&self) -> CellNumbering {
        self.numbering
    }

    /// Count cell sequence numbers this way from the next cell on.
    pub fn set_cell_numbering(&mut self, numbering: CellNumbering) {
        self.numbering = numbering;
    }

    /// Check `unit` against the carriage's rules.
    fn plan(&self, unit: &PesUnit<'_>) -> Result<Plan, MuxError> {
        let invalid = |why| Err(MuxError::InvalidUnit(why));
        let mut plan = Plan {
            stream_id: self.stream_id,
            mode: Mode::Async,
            pts: unit.pts.map(|t| t % PTS_WRAP),
            dts: unit.dts.map(|t| t % PTS_WRAP),
            data_alignment: unit.data_alignment,
            flags: unit.adaptation,
            len: unit.data.len(),
            sequence: self.sequence,
            numbering: self.numbering,
        };
        if unit.service_id.is_some() && !matches!(self.carriage, Carriage::MetadataCells { .. }) {
            return invalid("a metadata_service_id applies only to metadata cell carriage");
        }
        match self.carriage {
            Carriage::Pes => {
                let optional_header = PesHeader::has_optional_header(self.stream_id);
                if plan.dts.is_some() && plan.pts.is_none() {
                    return invalid("a DTS needs a PTS (PTS_DTS_flags '01' is forbidden, R-ISO13818-066)");
                }
                // A DTS equal to the PTS is not written (R-ISO13818-067).
                if plan.dts == plan.pts {
                    plan.dts = None;
                }
                if !optional_header && (plan.pts.is_some() || plan.data_alignment) {
                    return invalid(
                        "this stream_id has no optional PES header, so no timestamps or data_alignment_indicator (R-ISO13818-049)",
                    );
                }
                plan.mode = Mode::Pes { optional_header, video: (0xE0..=0xEF).contains(&self.stream_id) };
            }
            Carriage::MetadataCells { service_id } => {
                if plan.pts.is_none() {
                    return invalid("synchronous metadata needs a PTS in every PES packet (R-ST1402TIME-17)");
                }
                if plan.dts.is_some() {
                    return invalid("metadata is decoded at its PTS and never has a DTS (R-ISO13818-067)");
                }
                if unit.data.is_empty() {
                    return invalid("an access unit needs at least one byte");
                }
                if !plan.flags.is_empty() {
                    return invalid(
                        "KLV PIDs carry no PCR and no random_access_indicator (DECISIONS section 2, D-ISO13818-10)",
                    );
                }
                plan.mode = Mode::Cells { service_id: unit.service_id.unwrap_or(service_id) };
            }
            Carriage::AsyncKlv => {
                if plan.pts.is_some() || plan.dts.is_some() {
                    return invalid("asynchronous KLV has no PTS or DTS (R-ST1402TIME-42)");
                }
                if unit.data.is_empty() {
                    return invalid("a KLV item needs at least one byte");
                }
                if !plan.flags.is_empty() {
                    return invalid(
                        "KLV PIDs carry no PCR and no random_access_indicator (DECISIONS section 2, D-ISO13818-10)",
                    );
                }
                plan.mode = Mode::Async;
            }
        }
        Ok(plan)
    }

    fn count(&self, plan: Plan) -> usize {
        let first = af_body(&plan.flags, self.ts.discontinuity_pending());
        plan.segments().enumerate().map(|(i, s)| packet_count(s.len(), if i == 0 { first } else { 0 })).sum()
    }

    /// TS packets [`write`](Self::write) would emit for `unit`, or why it
    /// would refuse it.
    pub fn packets_needed(&self, unit: &PesUnit<'_>) -> Result<usize, MuxError> {
        self.plan(unit).map(|plan| self.count(plan))
    }

    /// Write `unit` as one or more PES packets. Returns the number of TS
    /// packets written. On error nothing is written and the continuity
    /// counter and cell sequence number are unchanged.
    pub fn write<S: PacketSink + ?Sized>(&mut self, unit: &PesUnit<'_>, sink: &mut S) -> Result<usize, MuxError> {
        let plan = self.plan(unit)?;
        let packets = self.count(plan);
        check_room(sink, packets)?;
        let mut written = 0;
        let mut cells = 0usize;
        for (i, seg) in plan.segments().enumerate() {
            let flags = if i == 0 { plan.flags } else { AdaptationFlags::default() };
            let data = &unit.data[seg.start..seg.end];
            let header = &seg.header[..seg.header_len];
            written += match &seg.cell {
                Some(cell) => {
                    cells += 1;
                    self.ts.emit(&[header, cell, data], &flags, false, sink)
                }
                None => self.ts.emit(&[header, data], &flags, false, sink),
            };
        }
        self.sequence = self.sequence.wrapping_add(cells as u8);
        debug_assert_eq!(written, packets);
        Ok(written)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamp_test_vectors() {
        // iso13818.md 3.10.
        let mut b = [0u8; 5];
        for (t, want) in [
            (0u64, [0x21, 0x00, 0x01, 0x00, 0x01]),
            (126_000, [0x21, 0x00, 0x07, 0xD8, 0x61]),
            (900_000, [0x21, 0x00, 0x37, 0x77, 0x41]),
            (PTS_WRAP - 1, [0x2F, 0xFF, 0xFF, 0xFF, 0xFF]),
            (PTS_WRAP, [0x21, 0x00, 0x01, 0x00, 0x01]),
        ] {
            put_timestamp(&mut b, 0b0010, t);
            assert_eq!(b, want, "{t}");
        }
    }

    #[test]
    fn header_layouts() {
        let mut h = [0u8; MAX_HEADER];
        // Sync KLV: 85 80 05 (st1402-time.md 3.1).
        let n = write_header(&mut h, 0xFC, 8 + 205, true, true, Some(900_000), None);
        assert_eq!(h[..n], [0, 0, 1, 0xFC, 0x00, 0xD5, 0x85, 0x80, 0x05, 0x21, 0x00, 0x37, 0x77, 0x41]);
        // Async KLV: 85|81 00 00 (st1402-time.md 3.6).
        let n = write_header(&mut h, 0xBD, 3 + 16, true, true, None, None);
        assert_eq!(h[..n], [0, 0, 1, 0xBD, 0x00, 0x13, 0x85, 0x00, 0x00]);
        let n = write_header(&mut h, 0xBD, 3 + 16, true, false, None, None);
        assert_eq!(h[6], 0x81);
        assert_eq!(n, 9);
        // PTS and DTS prefixes '0011' and '0001'.
        let n = write_header(&mut h, 0xE0, 0, true, false, Some(0), Some(0));
        assert_eq!(h[..n], [0, 0, 1, 0xE0, 0, 0, 0x81, 0xC0, 10, 0x31, 0, 1, 0, 1, 0x11, 0, 1, 0, 1]);
        // No optional header.
        assert_eq!(write_header(&mut h, 0xBE, 10, false, false, None, None), 6);
    }

    #[test]
    fn unit_rules_per_carriage() {
        let sync = PesWriter::klv_sync(0x101, 0).unwrap();
        let data = [1u8; 10];
        assert!(sync.packets_needed(&PesUnit::new(&data)).is_err(), "PTS required");
        assert!(sync.packets_needed(&PesUnit::new(&data).with_pts(1).with_dts(0)).is_err());
        assert!(sync.packets_needed(&PesUnit::new(&[]).with_pts(1)).is_err());
        assert!(sync.packets_needed(&PesUnit::new(&data).with_pts(1).with_pcr(0)).is_err());
        assert_eq!(sync.packets_needed(&PesUnit::new(&data).with_pts(1).with_service(4)), Ok(1));

        let async_klv = PesWriter::klv_async(0x102).unwrap();
        assert!(async_klv.packets_needed(&PesUnit::new(&data).with_pts(1)).is_err());
        assert!(async_klv.packets_needed(&PesUnit::new(&data).with_service(1)).is_err());
        assert_eq!(async_klv.packets_needed(&PesUnit::new(&data)), Ok(1));

        let video = PesWriter::with_carriage(0x100, 0xE0, Carriage::Pes).unwrap();
        assert!(video.packets_needed(&PesUnit::new(&data).with_dts(1)).is_err());
        assert_eq!(video.packets_needed(&PesUnit::new(&[])), Ok(1));
        let padding = PesWriter::with_carriage(0x100, 0xBE, Carriage::Pes).unwrap();
        assert!(padding.packets_needed(&PesUnit::new(&data).with_pts(0)).is_err());

        assert!(PesWriter::with_carriage(0x000F, 0xE0, Carriage::Pes).is_err());
        assert!(PesWriter::with_carriage(0x1FFF, 0xE0, Carriage::Pes).is_err());
        assert!(PesWriter::with_carriage(0x100, 0xFC, Carriage::Pes).is_err());
        assert!(PesWriter::with_carriage(0x100, 0xBD, Carriage::MetadataCells { service_id: 0 }).is_err());
        assert!(PesWriter::with_carriage(0x100, 0xFC, Carriage::AsyncKlv).is_err());
    }

    #[test]
    fn segment_layout() {
        let plan = |len, mode, pts| Plan {
            stream_id: 0xC0,
            mode,
            pts,
            dts: None,
            data_alignment: true,
            flags: AdaptationFlags::default(),
            len,
            sequence: 250,
            numbering: CellNumbering::PerStream,
        };
        let pes = Mode::Pes { optional_header: true, video: false };
        // 65527 bytes fit beside a PTS; one more splits.
        let segs: Vec<Segment> = plan(65_527, pes, Some(0)).segments().collect();
        assert_eq!(segs.len(), 1);
        assert_eq!(segs[0].header[4..6], [0xFF, 0xFF]);
        let segs: Vec<Segment> = plan(65_528 + 65_532, pes, Some(0)).segments().collect();
        assert_eq!(segs.iter().map(|s| s.end - s.start).collect::<Vec<_>>(), [65_527, 65_532, 1]);
        assert_eq!(segs[1].header[6..9], [0x81, 0x00, 0x00], "continuation: no PTS, not aligned");
        // Video: one unbounded PES.
        let video = Mode::Pes { optional_header: true, video: true };
        let segs: Vec<Segment> = plan(100_000, video, Some(0)).segments().collect();
        assert_eq!(segs.len(), 1);
        assert_eq!(segs[0].header[4..6], [0, 0]);
        // Cells: 65522 per cell, sequence numbers wrap.
        let cells = Mode::Cells { service_id: 7 };
        let segs: Vec<Segment> = plan(65_522 * 2 + 1, cells, Some(5)).segments().collect();
        let heads: Vec<[u8; 5]> = segs.iter().map(|s| s.cell.unwrap()).collect();
        assert_eq!(heads[0], [7, 250, 0x9F, 0xFF, 0xF2]);
        assert_eq!(heads[1], [7, 251, 0x0F, 0xFF, 0xF2]);
        assert_eq!(heads[2], [7, 252, 0x4F, 0x00, 0x01]);
        // Per access unit (the workaround of DECISIONS X-8): from 0, +1
        // per fragment.
        let per_au = Plan { numbering: CellNumbering::PerAccessUnit, ..plan(65_522 * 2 + 1, cells, Some(5)) };
        let heads: Vec<[u8; 5]> = per_au.segments().map(|s| s.cell.unwrap()).collect();
        assert_eq!(heads.iter().map(|h| h[1]).collect::<Vec<_>>(), [0, 1, 2]);
        // DECISIONS X-6: data_alignment_indicator only on the first PES.
        assert!(segs.iter().all(|s| s.header[7] == 0x80));
        assert_eq!(segs.iter().map(|s| s.header[6]).collect::<Vec<_>>(), [0x85, 0x81, 0x81]);
        assert_eq!(segs[0].header[4..6], [0xFF, 0xFF]);
        // Async: 65532 per PES.
        let segs: Vec<Segment> = plan(65_533, Mode::Async, None).segments().collect();
        assert_eq!(segs.iter().map(|s| (s.end - s.start, s.header[6])).collect::<Vec<_>>(), [(65_532, 0x85), (1, 0x81)]);
    }
}
