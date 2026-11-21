//! PAT and PMT (ISO/IEC 13818-1 2.4.4.3 to 2.4.4.9): allocation-free
//! section builders, and [`ProgramTables`], which keeps a whole set of
//! tables consistent and versioned.

use super::sink::{check_room, PacketSink};
use super::spec::{metadata_service_id, StreamSpec};
use super::ts::{packet_count, TsWriter};
use super::MuxError;
use crate::crc32_mpeg2;
use crate::packet::NULL_PID;
use crate::{Descriptor, Program, Report, Stream};
use std::collections::HashMap;

/// Largest PAT or PMT section in bytes: 3 header bytes plus a
/// section_length of at most 1021 (R-ISO13818-072, R-ISO13818-080,
/// R-ISO13818-091).
pub const MAX_PSI_SECTION_LEN: usize = 1024;

const MAX_SECTION_LENGTH: usize = MAX_PSI_SECTION_LEN - 3;
const PAT_PID: u16 = 0x0000;
const PAT_TABLE_ID: u8 = 0x00;
const PMT_TABLE_ID: u8 = 0x02;

/// One PAT entry: a program and its PMT PID, or with program_number 0 the
/// network_PID (Table 2-30).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PatEntry {
    /// program_number; 0 for the network information table.
    pub program_number: u16,
    /// program_map_PID, or network_PID for program 0.
    pub pid: u16,
}

/// One PMT elementary stream entry for [`write_pmt_section`] (Table 2-33).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PmtEntry<'a> {
    /// stream_type (Table 2-34).
    pub stream_type: u8,
    /// elementary_PID.
    pub pid: u16,
    /// The ES_info descriptor loop, serialized: each descriptor as tag,
    /// length and data (see [`write_descriptor`]).
    pub descriptors: &'a [u8],
}

/// A descriptor loop, serialized or as [`Descriptor`]s.
#[derive(Clone, Copy)]
enum Loop<'a> {
    Raw(&'a [u8]),
    Owned(&'a [Descriptor]),
}

impl Loop<'_> {
    fn len(self) -> usize {
        match self {
            Loop::Raw(b) => b.len(),
            Loop::Owned(d) => d.iter().map(|d| 2 + d.data.len()).sum(),
        }
    }

    /// Every descriptor is tag, length, then that many bytes
    /// (R-ISO13818-104, R-ISO13818-105).
    fn check(self) -> Result<(), MuxError> {
        match self {
            Loop::Raw(mut b) => {
                while !b.is_empty() {
                    if b.len() < 2 || b.len() < 2 + usize::from(b[1]) {
                        return Err(MuxError::InvalidDescriptor("descriptor loop ends inside a descriptor"));
                    }
                    b = &b[2 + usize::from(b[1])..];
                }
                Ok(())
            }
            Loop::Owned(d) => match d.iter().any(|d| d.data.len() > 255) {
                true => Err(MuxError::InvalidDescriptor("descriptor data longer than 255 bytes (R-ISO13818-105)")),
                false => Ok(()),
            },
        }
    }

    fn put(self, out: &mut [u8]) {
        match self {
            Loop::Raw(b) => out.copy_from_slice(b),
            Loop::Owned(ds) => {
                let mut at = 0;
                for d in ds {
                    out[at] = d.tag;
                    out[at + 1] = d.data.len() as u8;
                    out[at + 2..at + 2 + d.data.len()].copy_from_slice(&d.data);
                    at += 2 + d.data.len();
                }
            }
        }
    }
}

/// Write one descriptor (tag, length, data) at the start of `buf`; returns
/// its length. Fails when `data` is over 255 bytes (R-ISO13818-105) or
/// `buf` is too short.
pub fn write_descriptor(buf: &mut [u8], tag: u8, data: &[u8]) -> Result<usize, MuxError> {
    if data.len() > 255 {
        return Err(MuxError::InvalidDescriptor("descriptor data longer than 255 bytes (R-ISO13818-105)"));
    }
    let n = 2 + data.len();
    let dst = buf.get_mut(..n).ok_or(MuxError::BufferTooSmall { needed: n })?;
    dst[0] = tag;
    dst[1] = data.len() as u8;
    dst[2..].copy_from_slice(data);
    Ok(n)
}

fn check_version(version: u8) -> Result<(), MuxError> {
    match version {
        0..=31 => Ok(()),
        _ => Err(MuxError::InvalidValue { field: "version_number", reason: "is 5 bits, 0 to 31" }),
    }
}

/// PMT, network and elementary PIDs are 0x0010-0x1FFE (R-ISO13818-015,
/// R-ISO13818-084).
fn check_user_pid(pid: u16, what: &'static str) -> Result<(), MuxError> {
    match pid {
        0x0010..=0x1FFE => Ok(()),
        _ => Err(MuxError::InvalidPid { pid, reason: what }),
    }
}

/// A PCR_PID may be 0x0000, 0x0001 or 0x0010-0x1FFE (R-ISO13818-016), or
/// 0x1FFF for a program without PCR (R-ISO13818-047).
fn check_pcr_pid(pid: u16) -> Result<(), MuxError> {
    match pid {
        0x0000 | 0x0001 | 0x0010..=0x1FFF => Ok(()),
        _ => Err(MuxError::InvalidPid { pid, reason: "PCR_PID must be 0x0000, 0x0001, 0x0010 to 0x1FFE, or 0x1FFF for none" }),
    }
}

/// Write a long-form section around a body of `body_len` bytes that `body`
/// fills.
fn long_section(
    buf: &mut [u8],
    table_id: u8,
    id_ext: u16,
    version: u8,
    body_len: usize,
    body: impl FnOnce(&mut [u8]),
) -> Result<usize, MuxError> {
    check_version(version)?;
    let section_length = 5 + body_len + 4;
    if section_length > MAX_SECTION_LENGTH {
        return Err(MuxError::SectionTooLong { table_id, section_length });
    }
    let total = 3 + section_length;
    let buf = buf.get_mut(..total).ok_or(MuxError::BufferTooSmall { needed: total })?;
    buf[0] = table_id;
    // section_syntax_indicator 1, '0', reserved '11', then section_length,
    // whose first two bits are '00' (R-ISO13818-079, R-ISO13818-090,
    // R-ISO13818-103).
    buf[1] = 0xB0 | (section_length >> 8) as u8;
    buf[2] = section_length as u8;
    buf[3..5].copy_from_slice(&id_ext.to_be_bytes());
    // reserved '11', version_number, current_next_indicator 1: every
    // section written applies now (R-ISO13818-094).
    buf[5] = 0xC1 | version << 1;
    // section_number and last_section_number 0: one section per table
    // (R-ISO13818-082, R-ISO13818-095).
    buf[6] = 0;
    buf[7] = 0;
    body(&mut buf[8..8 + body_len]);
    // CRC-32/MPEG-2 over table_id .. the last body byte, so the CRC run over
    // the whole section gives 0 (R-ISO13818-100 to R-ISO13818-102).
    let crc = crc32_mpeg2(&buf[..total - 4]);
    buf[total - 4..].copy_from_slice(&crc.to_be_bytes());
    Ok(total)
}

/// Write a program association section (Table 2-30) into `buf`; returns
/// its length. Every program_number may appear once (R-ISO13818-083); PIDs
/// are 0x0010 to 0x1FFE (R-ISO13818-084). Fails with
/// [`MuxError::SectionTooLong`] above 253 entries.
///
/// ```
/// use mpegts::mux::{write_pat_section, PatEntry};
///
/// let mut buf = [0u8; 1024];
/// let n = write_pat_section(&mut buf, 1, 0, &[PatEntry { program_number: 1, pid: 0x1000 }])?;
/// assert_eq!(buf[..n], [0x00, 0xB0, 0x0D, 0x00, 0x01, 0xC1, 0x00, 0x00, 0x00, 0x01, 0xF0, 0x00, 0x2A, 0xB1, 0x04, 0xB2]);
/// # Ok::<(), mpegts::mux::MuxError>(())
/// ```
pub fn write_pat_section(
    buf: &mut [u8],
    transport_stream_id: u16,
    version: u8,
    entries: &[PatEntry],
) -> Result<usize, MuxError> {
    for (i, e) in entries.iter().enumerate() {
        check_user_pid(e.pid, "program_map_PID and network_PID must be 0x0010 to 0x1FFE")?;
        if entries[..i].iter().any(|o| o.program_number == e.program_number) {
            return Err(MuxError::InvalidProgram { number: e.program_number, reason: "listed twice in the PAT (R-ISO13818-083)" });
        }
    }
    long_section(buf, PAT_TABLE_ID, transport_stream_id, version, 4 * entries.len(), |body| {
        for (e, out) in entries.iter().zip(body.chunks_exact_mut(4)) {
            out[..2].copy_from_slice(&e.program_number.to_be_bytes());
            // reserved '111', then the 13-bit PID.
            out[2..].copy_from_slice(&(0xE000 | e.pid).to_be_bytes());
        }
    })
}

/// Write a TS program map section (Table 2-33) into `buf`; returns its
/// length. `program_info` and each entry's descriptors are serialized
/// descriptor loops. Elementary PIDs are 0x0010 to 0x1FFE and unique in the
/// program (R-ISO13818-015, R-ISO13818-020); `pcr_pid` is 0x1FFF for a
/// program without PCR (R-ISO13818-047). Fails with
/// [`MuxError::SectionTooLong`] when the program definition exceeds 1016
/// bytes (R-ISO13818-091, R-ISO13818-092): a program never spans two
/// sections (R-ISO13818-087).
pub fn write_pmt_section(
    buf: &mut [u8],
    program_number: u16,
    version: u8,
    pcr_pid: u16,
    program_info: &[u8],
    streams: &[PmtEntry<'_>],
) -> Result<usize, MuxError> {
    pmt_section(
        buf,
        program_number,
        version,
        pcr_pid,
        Loop::Raw(program_info),
        streams.iter().map(|s| (s.stream_type, s.pid, Loop::Raw(s.descriptors))),
    )
}

fn pmt_section<'a, I>(
    buf: &mut [u8],
    program_number: u16,
    version: u8,
    pcr_pid: u16,
    info: Loop<'a>,
    streams: I,
) -> Result<usize, MuxError>
where
    I: Iterator<Item = (u8, u16, Loop<'a>)> + Clone,
{
    if program_number == 0 {
        return Err(MuxError::InvalidProgram { number: 0, reason: "program_number 0 is reserved for the network PID" });
    }
    check_pcr_pid(pcr_pid)?;
    info.check()?;
    let mut body_len = 4 + info.len();
    for (i, (_, pid, d)) in streams.clone().enumerate() {
        check_user_pid(pid, "elementary PIDs must be 0x0010 to 0x1FFE")?;
        if streams.clone().take(i).any(|(_, other, _)| other == pid) {
            return Err(MuxError::PidConflict { pid, reason: "two streams of one program (R-ISO13818-020)" });
        }
        d.check()?;
        body_len += 5 + d.len();
    }
    long_section(buf, PMT_TABLE_ID, program_number, version, body_len, |body| {
        // reserved '111' + PCR_PID, reserved '1111' + program_info_length
        // ('00' prefix, R-ISO13818-096).
        body[..2].copy_from_slice(&(0xE000 | pcr_pid).to_be_bytes());
        body[2..4].copy_from_slice(&(0xF000 | info.len() as u16).to_be_bytes());
        let mut at = 4;
        info.put(&mut body[at..at + info.len()]);
        at += info.len();
        for (stream_type, pid, d) in streams {
            body[at] = stream_type;
            body[at + 1..at + 3].copy_from_slice(&(0xE000 | pid).to_be_bytes());
            // reserved '1111' + ES_info_length ('00' prefix, R-ISO13818-097).
            body[at + 3..at + 5].copy_from_slice(&(0xF000 | d.len() as u16).to_be_bytes());
            at += 5;
            d.put(&mut body[at..at + d.len()]);
            at += d.len();
        }
    })
}

/// One elementary stream entry of a [`ProgramDef`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct StreamDef {
    /// stream_type.
    pub stream_type: u8,
    /// elementary_PID.
    pub pid: u16,
    /// ES_info descriptors, in order.
    pub descriptors: Vec<Descriptor>,
}

impl StreamDef {
    /// An entry with no descriptors.
    pub fn new(stream_type: u8, pid: u16) -> Self {
        StreamDef { stream_type, pid, descriptors: Vec::new() }
    }

    /// Append a descriptor (tag and body).
    pub fn with_descriptor(mut self, tag: u8, data: &[u8]) -> Self {
        self.descriptors.push(Descriptor { tag, data: data.to_vec() });
        self
    }

    /// The entry a demuxer reported: stream_type, PID and every
    /// descriptor verbatim.
    pub fn from_stream(s: &Stream) -> Self {
        StreamDef { stream_type: s.stream_type, pid: s.pid, descriptors: s.descriptors.clone() }
    }
}

/// One program of a [`ProgramTables`]: what its PMT section says.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ProgramDef {
    /// program_number.
    pub number: u16,
    /// PID of its PMT.
    pub pmt_pid: u16,
    /// PCR_PID; 0x1FFF when the program has no PCR.
    pub pcr_pid: u16,
    /// version_number of its PMT section.
    pub version: u8,
    /// program_info descriptors.
    pub descriptors: Vec<Descriptor>,
    /// Elementary streams in PMT order.
    pub streams: Vec<StreamDef>,
}

impl ProgramDef {
    /// The program a demuxer reported, with its PMT version. Fails when the
    /// program's PMT has not been received yet.
    pub fn from_program(p: &Program) -> Result<Self, MuxError> {
        let version = p
            .version
            .ok_or(MuxError::InvalidProgram { number: p.number, reason: "its PMT has not been received" })?;
        Ok(ProgramDef {
            number: p.number,
            pmt_pid: p.pmt_pid,
            pcr_pid: p.pcr_pid.unwrap_or(NULL_PID),
            version,
            descriptors: p.descriptors.clone(),
            streams: p.streams.iter().map(StreamDef::from_stream).collect(),
        })
    }

    /// The stream on `pid`.
    pub fn stream(&self, pid: u16) -> Option<&StreamDef> {
        self.streams.iter().find(|s| s.pid == pid)
    }

    fn section_into(&self, buf: &mut [u8]) -> Result<usize, MuxError> {
        pmt_section(
            buf,
            self.number,
            self.version,
            self.pcr_pid,
            Loop::Owned(&self.descriptors),
            self.streams.iter().map(|s| (s.stream_type, s.pid, Loop::Owned(&s.descriptors))),
        )
    }

    /// Bytes of the PMT section.
    fn section_len(&self) -> usize {
        let streams: usize = self.streams.iter().map(|s| 5 + Loop::Owned(&s.descriptors).len()).sum();
        3 + 5 + 4 + Loop::Owned(&self.descriptors).len() + streams + 4
    }

    /// Same content apart from the version.
    fn same_content(&self, other: &ProgramDef) -> bool {
        self.pmt_pid == other.pmt_pid
            && self.pcr_pid == other.pcr_pid
            && self.descriptors == other.descriptors
            && self.streams == other.streams
    }
}

/// The proposed state of a [`ProgramTables`] during a change.
struct Draft {
    transport_stream_id: u16,
    network_pid: Option<u16>,
    programs: Vec<ProgramDef>,
}

impl Draft {
    fn program(&mut self, number: u16) -> Result<&mut ProgramDef, MuxError> {
        self.programs.iter_mut().find(|p| p.number == number).ok_or(MuxError::UnknownProgram(number))
    }

    fn pat_entries(&self) -> impl Iterator<Item = PatEntry> + '_ {
        let network = self.network_pid.map(|pid| PatEntry { program_number: 0, pid });
        network.into_iter().chain(self.programs.iter().map(|p| PatEntry { program_number: p.number, pid: p.pmt_pid }))
    }

    /// Every table fits and every PID is used consistently.
    fn check(&self) -> Result<(), MuxError> {
        let mut buf = [0u8; MAX_PSI_SECTION_LEN];
        let pat: Vec<PatEntry> = self.pat_entries().collect();
        write_pat_section(&mut buf, self.transport_stream_id, 0, &pat)?;
        let mut tables: HashMap<u16, &'static str> = HashMap::new();
        if let Some(pid) = self.network_pid {
            tables.insert(pid, "the network PID");
        }
        for p in &self.programs {
            p.section_into(&mut buf)?;
            if Some(p.pmt_pid) == self.network_pid {
                return Err(MuxError::PidConflict { pid: p.pmt_pid, reason: "the network PID" });
            }
            tables.insert(p.pmt_pid, "a PMT PID");
        }
        // One PID carries one elementary stream (R-ISO13818-020): a stream
        // listed by two programs must be the same stream, and no stream may
        // sit on a table PID.
        let mut es: HashMap<u16, u8> = HashMap::new();
        for p in &self.programs {
            for s in &p.streams {
                if let Some(&what) = tables.get(&s.pid) {
                    return Err(MuxError::PidConflict { pid: s.pid, reason: what });
                }
                match es.insert(s.pid, s.stream_type) {
                    Some(t) if t != s.stream_type => {
                        return Err(MuxError::PidConflict {
                            pid: s.pid,
                            reason: "another program lists a different stream on it (R-ISO13818-020)",
                        })
                    }
                    _ => {}
                }
            }
        }
        Ok(())
    }
}

/// A transport stream's PAT and PMTs, kept consistent: every change is
/// checked (PID ranges and conflicts, section_length at most 1021) and
/// either applied whole, with the version_number of each changed table
/// incremented modulo 32 (R-ISO13818-081, R-ISO13818-093), or refused with
/// nothing changed.
///
/// It also owns the continuity counters of PID 0 and the PMT PIDs, and
/// writes the tables as TS packets.
///
/// ```
/// use mpegts::mux::{ProgramTables, StreamSpec};
///
/// let mut tables = ProgramTables::new(1);
/// tables.add_program(1, 0x1000, Some(0x100))?;
/// tables.add_stream(1, 0x100, &StreamSpec::h264())?;
/// assert_eq!(tables.program(1).unwrap().version, 1);
/// tables.add_stream(1, 0x101, &StreamSpec::klv_sync(0))?;
/// assert_eq!(tables.program(1).unwrap().version, 2);
/// assert_eq!(tables.pat_version(), 1); // the PAT changed once, when program 1 was added
///
/// let mut out = Vec::new();
/// let packets = tables.write_all(&mut out)?;
/// assert_eq!(packets, 2);
/// # Ok::<(), mpegts::mux::MuxError>(())
/// ```
#[derive(Debug, Clone)]
pub struct ProgramTables {
    transport_stream_id: u16,
    pat_version: u8,
    network_pid: Option<u16>,
    /// Sorted by program_number.
    programs: Vec<ProgramDef>,
    /// Last PMT version of removed programs, so a program that comes back
    /// does not repeat a version receivers may have cached.
    retired: Vec<(u16, u8)>,
    pat_writer: TsWriter,
    pmt_writers: Vec<TsWriter>,
}

impl ProgramTables {
    /// Empty tables: PAT version 0, no programs.
    pub fn new(transport_stream_id: u16) -> Self {
        ProgramTables {
            transport_stream_id,
            pat_version: 0,
            network_pid: None,
            programs: Vec::new(),
            retired: Vec::new(),
            pat_writer: TsWriter::new(PAT_PID).expect("PID 0 is valid"),
            pmt_writers: Vec::new(),
        }
    }

    /// The tables a demuxer reported: transport_stream_id, PAT and PMT
    /// versions, network PID, and every program with its descriptors and
    /// streams verbatim. A change made afterwards bumps the reported
    /// version by one, as a rewriting injector should (D-ISO13818-12).
    ///
    /// Fails when the report has no PAT yet, when a listed program's PMT
    /// has not arrived, when the demuxer dropped streams over
    /// [`Config::max_streams`](crate::Config::max_streams), or when the
    /// input tables break a rule this type enforces.
    pub fn from_report(report: &Report) -> Result<Self, MuxError> {
        let transport_stream_id = report
            .transport_stream_id
            .ok_or(MuxError::InvalidValue { field: "report", reason: "no PAT has been received" })?;
        if report.stats.streams_dropped > 0 {
            return Err(MuxError::InvalidValue {
                field: "report",
                reason: "the demuxer did not track every stream (Config::max_streams)",
            });
        }
        let mut programs = report.programs.iter().map(ProgramDef::from_program).collect::<Result<Vec<_>, _>>()?;
        programs.sort_by_key(|p| p.number);
        for p in &programs {
            check_version(p.version)?;
        }
        let draft = Draft { transport_stream_id, network_pid: report.network_pid, programs };
        draft.check()?;
        let mut t = ProgramTables::new(transport_stream_id);
        t.pat_version = report.pat_version.unwrap_or(0) & 0x1F;
        t.network_pid = draft.network_pid;
        t.programs = draft.programs;
        t.add_writers();
        Ok(t)
    }

    fn add_writers(&mut self) {
        for p in &self.programs {
            if !self.pmt_writers.iter().any(|w| w.pid() == p.pmt_pid) {
                self.pmt_writers.push(TsWriter::new(p.pmt_pid).expect("checked PMT PID"));
            }
        }
    }

    /// Apply a change through a draft: check it whole, bump the versions of
    /// the tables it changed, then commit, or change nothing.
    fn update(&mut self, change: impl FnOnce(&mut Draft) -> Result<(), MuxError>) -> Result<(), MuxError> {
        let mut d = Draft {
            transport_stream_id: self.transport_stream_id,
            network_pid: self.network_pid,
            programs: self.programs.clone(),
        };
        change(&mut d)?;
        d.programs.sort_by_key(|p| p.number);
        d.check()?;
        for p in &mut d.programs {
            if let Some(old) = self.programs.iter().find(|o| o.number == p.number) {
                if p.version == old.version && !p.same_content(old) {
                    p.version = (old.version + 1) & 0x1F;
                }
            }
        }
        let pat_changed = d.transport_stream_id != self.transport_stream_id
            || d.network_pid != self.network_pid
            || !d.programs.iter().map(|p| (p.number, p.pmt_pid)).eq(self.programs.iter().map(|p| (p.number, p.pmt_pid)));
        for old in &self.programs {
            if !d.programs.iter().any(|p| p.number == old.number) {
                self.retired.retain(|(n, _)| *n != old.number);
                self.retired.push((old.number, old.version));
            }
        }
        self.transport_stream_id = d.transport_stream_id;
        self.network_pid = d.network_pid;
        self.programs = d.programs;
        if pat_changed {
            self.pat_version = (self.pat_version + 1) & 0x1F;
        }
        self.add_writers();
        Ok(())
    }

    /// transport_stream_id.
    pub fn transport_stream_id(&self) -> u16 {
        self.transport_stream_id
    }

    /// Change the transport_stream_id (a PAT change).
    pub fn set_transport_stream_id(&mut self, id: u16) -> Result<(), MuxError> {
        self.update(|d| {
            d.transport_stream_id = id;
            Ok(())
        })
    }

    /// version_number of the PAT.
    pub fn pat_version(&self) -> u8 {
        self.pat_version
    }

    /// Set the PAT version_number, for example to continue an input's
    /// numbering. The next change increments it from there.
    pub fn set_pat_version(&mut self, version: u8) -> Result<(), MuxError> {
        check_version(version)?;
        self.pat_version = version;
        Ok(())
    }

    /// The network_PID (PAT program 0), if any.
    pub fn network_pid(&self) -> Option<u16> {
        self.network_pid
    }

    /// Set or remove the PAT's program 0 entry, the network_PID of the NIT
    /// (2.4.4.5).
    pub fn set_network_pid(&mut self, pid: Option<u16>) -> Result<(), MuxError> {
        self.update(|d| {
            d.network_pid = pid;
            Ok(())
        })
    }

    /// The programs, by program_number.
    pub fn programs(&self) -> &[ProgramDef] {
        &self.programs
    }

    /// The program with this program_number.
    pub fn program(&self, number: u16) -> Option<&ProgramDef> {
        self.programs.iter().find(|p| p.number == number)
    }

    /// Add a program with no streams, its PMT on `pmt_pid`, its PCR on
    /// `pcr_pid` (`None`: a program of private streams without PCR,
    /// R-ISO13818-047). Programs may share a PMT PID.
    pub fn add_program(&mut self, number: u16, pmt_pid: u16, pcr_pid: Option<u16>) -> Result<(), MuxError> {
        if number == 0 {
            return Err(MuxError::InvalidProgram { number, reason: "program_number 0 is reserved for the network PID" });
        }
        let version = self.retired.iter().find(|(n, _)| *n == number).map_or(0, |(_, v)| (v + 1) & 0x1F);
        self.update(|d| {
            if d.programs.iter().any(|p| p.number == number) {
                return Err(MuxError::InvalidProgram { number, reason: "already present (R-ISO13818-083)" });
            }
            d.programs.push(ProgramDef {
                number,
                pmt_pid,
                pcr_pid: pcr_pid.unwrap_or(NULL_PID),
                version,
                descriptors: Vec::new(),
                streams: Vec::new(),
            });
            Ok(())
        })
    }

    /// Remove a program.
    pub fn remove_program(&mut self, number: u16) -> Result<(), MuxError> {
        self.update(|d| {
            d.program(number)?;
            d.programs.retain(|p| p.number != number);
            Ok(())
        })
    }

    /// Set a program's PCR_PID (`None`: no PCR).
    pub fn set_pcr_pid(&mut self, number: u16, pcr_pid: Option<u16>) -> Result<(), MuxError> {
        self.update(|d| {
            d.program(number)?.pcr_pid = pcr_pid.unwrap_or(NULL_PID);
            Ok(())
        })
    }

    /// Replace a program's program_info descriptors.
    pub fn set_program_descriptors(&mut self, number: u16, descriptors: Vec<Descriptor>) -> Result<(), MuxError> {
        self.update(|d| {
            d.program(number)?.descriptors = descriptors;
            Ok(())
        })
    }

    /// Append a program_info descriptor.
    pub fn add_program_descriptor(&mut self, number: u16, tag: u8, data: &[u8]) -> Result<(), MuxError> {
        self.update(|d| {
            d.program(number)?.descriptors.push(Descriptor { tag, data: data.to_vec() });
            Ok(())
        })
    }

    /// Add the stream `spec` describes on `pid` to a program. A stream
    /// carrying metadata services must use service ids no other stream in
    /// the tables uses (R-ST1402TIME-35, R-ISO13818-128).
    pub fn add_stream(&mut self, number: u16, pid: u16, spec: &StreamSpec) -> Result<(), MuxError> {
        spec.check()?;
        let program = self.program(number).ok_or(MuxError::UnknownProgram(number))?;
        if program.stream(pid).is_some() {
            return Err(MuxError::PidConflict { pid, reason: "two streams of one program (R-ISO13818-020)" });
        }
        for service in spec.metadata_services() {
            let taken = self.programs.iter().flat_map(|p| &p.streams).flat_map(|s| &s.descriptors).any(|d| {
                d.tag == 0x26 && metadata_service_id(&d.data) == Some(service)
            });
            if taken {
                return Err(MuxError::InvalidValue {
                    field: "metadata_service_id",
                    reason: "already used by another metadata service in these tables (R-ST1402TIME-35)",
                });
            }
        }
        self.add_stream_def(number, StreamDef { stream_type: spec.stream_type, pid, descriptors: spec.descriptors.clone() })
    }

    /// Add an elementary stream entry as given, for example one copied
    /// from an input's PMT ([`StreamDef::from_stream`]).
    pub fn add_stream_def(&mut self, number: u16, stream: StreamDef) -> Result<(), MuxError> {
        self.update(|d| {
            let p = d.program(number)?;
            if p.stream(stream.pid).is_some() {
                return Err(MuxError::PidConflict { pid: stream.pid, reason: "two streams of one program (R-ISO13818-020)" });
            }
            p.streams.push(stream);
            Ok(())
        })
    }

    /// Remove the stream on `pid` from a program.
    pub fn remove_stream(&mut self, number: u16, pid: u16) -> Result<(), MuxError> {
        self.update(|d| {
            let p = d.program(number)?;
            let i = p.streams.iter().position(|s| s.pid == pid).ok_or(MuxError::UnknownStream { program: number, pid })?;
            p.streams.remove(i);
            Ok(())
        })
    }

    /// Append an ES_info descriptor to the stream on `pid`.
    pub fn add_stream_descriptor(&mut self, number: u16, pid: u16, tag: u8, data: &[u8]) -> Result<(), MuxError> {
        self.update(|d| {
            let p = d.program(number)?;
            let s = p.streams.iter_mut().find(|s| s.pid == pid).ok_or(MuxError::UnknownStream { program: number, pid })?;
            s.descriptors.push(Descriptor { tag, data: data.to_vec() });
            Ok(())
        })
    }

    /// Set a program's PMT version_number, for example to continue an
    /// input's numbering. The next change increments it from there.
    pub fn set_pmt_version(&mut self, number: u16, version: u8) -> Result<(), MuxError> {
        check_version(version)?;
        let p = self.programs.iter_mut().find(|p| p.number == number).ok_or(MuxError::UnknownProgram(number))?;
        p.version = version;
        Ok(())
    }

    fn pat_into(&self, buf: &mut [u8]) -> Result<usize, MuxError> {
        let mut entries = [PatEntry { program_number: 0, pid: 0 }; 256];
        let mut n = 0;
        if let Some(pid) = self.network_pid {
            entries[0] = PatEntry { program_number: 0, pid };
            n = 1;
        }
        // The tables are checked to fit one section, so at most 253
        // entries.
        for p in &self.programs {
            entries[n] = PatEntry { program_number: p.number, pid: p.pmt_pid };
            n += 1;
        }
        write_pat_section(buf, self.transport_stream_id, self.pat_version, &entries[..n])
    }

    /// The PAT section, written into `buf` (at most
    /// [`MAX_PSI_SECTION_LEN`] bytes); returns its length.
    pub fn pat_section_into(&self, buf: &mut [u8]) -> Result<usize, MuxError> {
        self.pat_into(buf)
    }

    /// The PMT section of a program, written into `buf`; returns its
    /// length.
    pub fn pmt_section_into(&self, number: u16, buf: &mut [u8]) -> Result<usize, MuxError> {
        self.program(number).ok_or(MuxError::UnknownProgram(number))?.section_into(buf)
    }

    /// The PAT section.
    pub fn pat_section(&self) -> Vec<u8> {
        let mut buf = [0u8; MAX_PSI_SECTION_LEN];
        let n = self.pat_into(&mut buf).expect("checked tables fit");
        buf[..n].to_vec()
    }

    /// The PMT section of a program.
    pub fn pmt_section(&self, number: u16) -> Result<Vec<u8>, MuxError> {
        let mut buf = [0u8; MAX_PSI_SECTION_LEN];
        let n = self.pmt_section_into(number, &mut buf)?;
        Ok(buf[..n].to_vec())
    }

    /// The packet writer of PID 0 or of a PMT PID, for its continuity
    /// counter.
    pub fn writer(&self, pid: u16) -> Option<&TsWriter> {
        match pid {
            PAT_PID => Some(&self.pat_writer),
            _ => self.pmt_writers.iter().find(|w| w.pid() == pid),
        }
    }

    /// The packet writer of PID 0 or of a PMT PID, to continue an input's
    /// continuity counter or mark a discontinuity.
    pub fn writer_mut(&mut self, pid: u16) -> Option<&mut TsWriter> {
        match pid {
            PAT_PID => Some(&mut self.pat_writer),
            _ => self.pmt_writers.iter_mut().find(|w| w.pid() == pid),
        }
    }

    fn pmt_writer(&mut self, pid: u16) -> &mut TsWriter {
        self.pmt_writers.iter_mut().find(|w| w.pid() == pid).expect("every PMT PID has a writer")
    }

    fn pmt_writer_ref(&self, pid: u16) -> &TsWriter {
        self.pmt_writers.iter().find(|w| w.pid() == pid).expect("every PMT PID has a writer")
    }

    /// Write the PAT as TS packets on PID 0; returns the packet count.
    pub fn write_pat<S: PacketSink + ?Sized>(&mut self, sink: &mut S) -> Result<usize, MuxError> {
        let mut buf = [0u8; MAX_PSI_SECTION_LEN];
        let n = self.pat_into(&mut buf)?;
        self.pat_writer.write_section(&buf[..n], sink)
    }

    /// Write a program's PMT as TS packets on its PMT PID; returns the
    /// packet count.
    pub fn write_pmt<S: PacketSink + ?Sized>(&mut self, number: u16, sink: &mut S) -> Result<usize, MuxError> {
        let mut buf = [0u8; MAX_PSI_SECTION_LEN];
        let n = self.pmt_section_into(number, &mut buf)?;
        let pid = self.program(number).map(|p| p.pmt_pid).expect("found above");
        self.pmt_writer(pid).write_section(&buf[..n], sink)
    }

    /// Write the PAT, then every PMT in program_number order, as TS
    /// packets; returns the packet count. Repeating this at least every
    /// 100 ms keeps the PSI interval MISB ST 1402.2 asks for
    /// (R-ST1402TIME-03, DECISIONS X-3); the timing is the caller's.
    pub fn write_all<S: PacketSink + ?Sized>(&mut self, sink: &mut S) -> Result<usize, MuxError> {
        let mut buf = [0u8; MAX_PSI_SECTION_LEN];
        let pat_len = self.pat_into(&mut buf)?;
        let mut needed = self.pat_writer.packets_for_section(pat_len);
        for (i, p) in self.programs.iter().enumerate() {
            // A pending discontinuity only widens the first section written
            // on a PID.
            let shared = self.programs[..i].iter().any(|o| o.pmt_pid == p.pmt_pid);
            needed += if shared {
                packet_count(1 + p.section_len(), 0)
            } else {
                self.pmt_writer_ref(p.pmt_pid).packets_for_section(p.section_len())
            };
        }
        check_room(sink, needed)?;
        let mut written = self.pat_writer.write_section(&buf[..pat_len], sink)?;
        for i in 0..self.programs.len() {
            let n = self.programs[i].section_into(&mut buf)?;
            let pid = self.programs[i].pmt_pid;
            written += self.pmt_writer(pid).write_section(&buf[..n], sink)?;
        }
        debug_assert_eq!(written, needed);
        Ok(written)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        s.split_whitespace().map(|b| u8::from_str_radix(b, 16).unwrap()).collect()
    }

    #[test]
    fn pat_and_async_pmt_test_vectors() {
        // iso13818.md 3.5 and 3.6 (the async vector is unchanged by
        // DECISIONS X-1 and X-2).
        let mut t = ProgramTables::new(1);
        t.add_program(1, 0x1000, Some(0x100)).unwrap();
        t.set_pat_version(0).unwrap();
        assert_eq!(t.pat_section(), hex("00 B0 0D 00 01 C1 00 00 00 01 F0 00 2A B1 04 B2"));
        t.add_stream_def(1, StreamDef::new(0x1B, 0x100)).unwrap();
        t.add_stream(1, 0x102, &StreamSpec::klv_async()).unwrap();
        t.set_pmt_version(1, 1).unwrap();
        assert_eq!(
            t.pmt_section(1).unwrap(),
            hex("02 B0 1D 00 01 C3 00 00 E1 00 F0 00 1B E1 00 F0 00 06 E1 02 F0 06 05 04 4B 4C 56 41 18 29 F9 69")
        );
    }

    #[test]
    fn sync_klv_es_entry() {
        // st1402-time.md 3.5: the ES entry of sync KLV on PID 0x0102.
        let mut t = ProgramTables::new(1);
        t.add_program(1, 0x1000, None).unwrap();
        t.add_stream(1, 0x102, &StreamSpec::klv_sync(0)).unwrap();
        let s = t.pmt_section(1).unwrap();
        let entry = hex("15 E1 02 F0 16 26 09 01 00 FF 4B 4C 56 41 00 0F 27 09 FF FF FF C0 04 00 C0 00 00");
        assert_eq!(s[12..s.len() - 4], entry[..]);
        assert_eq!(s[8..12], [0xFF, 0xFF, 0xF0, 0x00], "PCR_PID 0x1FFF, no program descriptors");
        assert_eq!(crc32_mpeg2(&s), 0);
    }

    #[test]
    fn raw_builders_validate() {
        let mut buf = [0u8; 1024];
        let pat = |entries: &[PatEntry]| write_pat_section(&mut [0u8; 1024], 1, 0, entries);
        assert!(pat(&[PatEntry { program_number: 1, pid: 0x000F }]).is_err());
        assert!(pat(&[PatEntry { program_number: 1, pid: 0x20 }, PatEntry { program_number: 1, pid: 0x21 }]).is_err());
        assert!(write_pat_section(&mut buf, 1, 32, &[]).is_err());
        assert_eq!(write_pat_section(&mut buf[..15], 1, 0, &[PatEntry { program_number: 1, pid: 0x20 }]), Err(MuxError::BufferTooSmall { needed: 16 }));
        let entries: Vec<PatEntry> = (1..=253).map(|n| PatEntry { program_number: n, pid: 0x20 }).collect();
        assert_eq!(write_pat_section(&mut buf, 1, 0, &entries), Ok(1024));
        let entries: Vec<PatEntry> = (1..=254).map(|n| PatEntry { program_number: n, pid: 0x20 }).collect();
        assert_eq!(write_pat_section(&mut buf, 1, 0, &entries), Err(MuxError::SectionTooLong { table_id: 0, section_length: 1025 }));

        let d = [0x05, 0x04, b'K', b'L', b'V', b'A'];
        let s = |pid, descriptors| PmtEntry { stream_type: 6, pid, descriptors };
        assert!(write_pmt_section(&mut buf, 1, 0, 0x1FFF, &[], &[s(0x100, &d)]).is_ok());
        assert!(write_pmt_section(&mut buf, 1, 0, 0x1FFF, &[], &[s(0x100, &d[..5])]).is_err());
        assert!(write_pmt_section(&mut buf, 1, 0, 0x1FFF, &[0x05], &[]).is_err());
        assert!(write_pmt_section(&mut buf, 1, 0, 0x0005, &[], &[]).is_err());
        assert!(write_pmt_section(&mut buf, 0, 0, 0x1FFF, &[], &[]).is_err());
        assert!(write_pmt_section(&mut buf, 1, 0, 0x1FFF, &[], &[s(0x100, &d), s(0x100, &d)]).is_err());
        assert_eq!(write_descriptor(&mut buf, 0x05, b"KLVA"), Ok(6));
        assert_eq!(buf[..6], d);
        assert!(write_descriptor(&mut buf, 0x05, &[0; 256]).is_err());
    }

    #[test]
    fn versions_bump_on_change_only() {
        let mut t = ProgramTables::new(7);
        assert_eq!(t.pat_version(), 0);
        t.add_program(1, 0x1000, Some(0x100)).unwrap();
        assert_eq!((t.pat_version(), t.program(1).unwrap().version), (1, 0));
        t.set_pcr_pid(1, Some(0x100)).unwrap();
        assert_eq!(t.program(1).unwrap().version, 0, "no change, no new version");
        for i in 1..=40u32 {
            t.add_program_descriptor(1, 0x80, &[i as u8]).unwrap();
            assert_eq!(u32::from(t.program(1).unwrap().version), i % 32, "wraps modulo 32");
        }
        assert_eq!(t.pat_version(), 1, "PMT changes leave the PAT alone");
        t.set_network_pid(Some(0x10)).unwrap();
        assert_eq!(t.pat_version(), 2);
        t.set_transport_stream_id(8).unwrap();
        assert_eq!(t.pat_version(), 3);
        // A removed program that comes back continues its numbering.
        let v = t.program(1).unwrap().version;
        t.remove_program(1).unwrap();
        t.add_program(1, 0x1000, None).unwrap();
        assert_eq!(t.program(1).unwrap().version, (v + 1) & 0x1F);
    }

    #[test]
    fn refused_changes_leave_everything_alone() {
        let mut t = ProgramTables::new(1);
        t.add_program(1, 0x1000, Some(0x100)).unwrap();
        t.add_stream(1, 0x100, &StreamSpec::h264()).unwrap();
        let before = (t.pat_section(), t.pmt_section(1).unwrap());
        assert!(matches!(t.add_stream(1, 0x100, &StreamSpec::klv_async()), Err(MuxError::PidConflict { .. })));
        assert!(matches!(t.add_stream(1, 0x1000, &StreamSpec::klv_async()), Err(MuxError::PidConflict { .. })));
        assert!(matches!(t.add_stream(2, 0x200, &StreamSpec::klv_async()), Err(MuxError::UnknownProgram(2))));
        assert!(t.add_program(1, 0x1001, None).is_err());
        assert!(t.add_program(0, 0x1001, None).is_err());
        assert!(t.add_program(2, 0x100, None).is_err(), "PMT on an elementary PID");
        assert_eq!((t.pat_section(), t.pmt_section(1).unwrap()), before, "refused changes change nothing");
        t.add_program(2, 0x1001, None).unwrap();
        assert!(t.add_stream(2, 0x100, &StreamSpec::klv_async()).is_err(), "different stream on a shared PID");
        t.add_stream(2, 0x100, &StreamSpec::h264()).unwrap();
        t.remove_program(2).unwrap();
        let pat = t.pat_section();
        assert!(t.add_stream(1, 0x101, &StreamSpec::klv_sync(0)).is_ok());
        assert!(t.add_stream(1, 0x102, &StreamSpec::klv_sync(0)).is_err(), "service id taken");
        // Grow the PMT until it no longer fits in one section.
        let mut pid = 0x200;
        let err = loop {
            match t.add_stream(1, pid, &StreamSpec::h264().with_descriptor(0x80, &[0xAA; 200])) {
                Ok(()) => pid += 1,
                Err(e) => break e,
            }
        };
        assert!(matches!(err, MuxError::SectionTooLong { table_id: 2, section_length } if section_length > 1021));
        let pmt = t.pmt_section(1).unwrap();
        assert!(pmt.len() <= MAX_PSI_SECTION_LEN);
        assert_eq!(t.pat_section(), pat);
        assert!(t.remove_stream(1, 0x999).is_err());
    }

    #[test]
    fn write_all_counts_shared_pmt_pids() {
        let mut t = ProgramTables::new(1);
        t.add_program(1, 0x1000, None).unwrap();
        t.add_program(2, 0x1000, None).unwrap();
        t.writer_mut(0x1000).unwrap().mark_discontinuity();
        let mut buf = [0u8; 188 * 2];
        let mut sink = crate::mux::SliceSink::new(&mut buf);
        assert_eq!(t.write_all(&mut sink), Err(MuxError::BufferTooSmall { needed: 3 * 188 }));
        assert_eq!(t.writer(0x1000).unwrap().continuity_counter(), 0);
        let mut out = Vec::new();
        assert_eq!(t.write_all(&mut out), Ok(3));
        assert_eq!(t.writer(0x1000).unwrap().continuity_counter(), 2);
        assert_eq!(t.writer(0).unwrap().continuity_counter(), 1);
        assert!(t.writer(0x1001).is_none());
    }
}
