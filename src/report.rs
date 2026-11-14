//! Snapshot of what a feed currently carries, with query helpers.

use crate::codec::{Basis, Codec, StreamKind};
use std::fmt;
use std::time::Duration;

/// PTS/DTS clock rate.
pub const PTS_HZ: u64 = 90_000;
/// PCR clock rate.
pub const PCR_HZ: u64 = 27_000_000;

/// A four-character code such as a registration `format_identifier`.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct FourCc(pub [u8; 4]);

impl FourCc {
    /// The four raw bytes.
    pub fn as_bytes(&self) -> &[u8; 4] {
        &self.0
    }
}

impl PartialEq<&[u8; 4]> for FourCc {
    fn eq(&self, other: &&[u8; 4]) -> bool {
        &self.0 == *other
    }
}

impl fmt::Display for FourCc {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.0.iter().all(|b| b.is_ascii_graphic() || *b == b' ') {
            f.write_str(&String::from_utf8_lossy(&self.0))
        } else {
            write!(f, "0x{:08x}", u32::from_be_bytes(self.0))
        }
    }
}

impl fmt::Debug for FourCc {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "FourCc({self})")
    }
}

/// ISO 639-2 language code.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Language(pub [u8; 3]);

impl Language {
    /// The code as text, or `"???"` if it is not valid UTF-8.
    pub fn as_str(&self) -> &str {
        std::str::from_utf8(&self.0).unwrap_or("???")
    }
}

impl fmt::Display for Language {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl fmt::Debug for Language {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Language({self})")
    }
}

#[cfg(feature = "serde")]
mod ser {
    use super::*;
    use serde::{Serialize, Serializer};

    impl Serialize for FourCc {
        fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
            s.collect_str(self)
        }
    }

    impl Serialize for Language {
        fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
            s.serialize_str(self.as_str())
        }
    }
}

/// A raw descriptor from a PMT, kept even if this crate does not parse it.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct Descriptor {
    /// descriptor_tag.
    pub tag: u8,
    /// Descriptor body, excluding the tag and length bytes.
    pub data: Vec<u8>,
}

/// A conditional access system announced in the CAT.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct CaSystem {
    /// CA_system_ID.
    pub system_id: u16,
    /// PID carrying EMMs for this system.
    pub pid: u16,
}

/// One elementary stream as currently declared by its program's PMT.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct Stream {
    /// program_number of the program that declares this stream.
    pub program: u16,
    /// Elementary PID.
    pub pid: u16,
    /// Raw PMT stream_type.
    pub stream_type: u8,
    /// Identified format.
    pub codec: Codec,
    /// How `codec` was determined.
    pub basis: Basis,
    /// First language from an ISO_639_language_descriptor.
    pub language: Option<Language>,
    /// ES-level registration descriptor format_identifier.
    pub registration: Option<FourCc>,
    /// metadata_descriptor format identifier (`KLVA` for sync KLV).
    pub metadata_format: Option<FourCc>,
    /// Every ES-level descriptor, in PMT order.
    pub descriptors: Vec<Descriptor>,
    /// PES stream_id seen on the wire, once a PES header has been observed.
    pub pes_stream_id: Option<u8>,
    /// First PES PTS seen (90 kHz, raw 33-bit value).
    pub first_pts: Option<u64>,
    /// Most recent PES PTS.
    pub last_pts: Option<u64>,
    /// Most recent PES DTS, for streams that send one.
    pub last_dts: Option<u64>,
    /// Span between the earliest and latest PTS seen, in 90 kHz ticks,
    /// accounting for 33-bit wraparound.
    pub pts_span: Option<u64>,
    /// Packets seen on this PID since the feed started.
    pub packets: u64,
    /// Continuity counter errors on this PID.
    pub cc_errors: u64,
    /// Packets with scrambled payload.
    pub scrambled_packets: u64,
    /// The most recent payload packet was scrambled.
    pub scrambled: bool,
    /// Feed packet number (1-based) of the latest packet on this PID.
    pub last_packet: Option<u64>,
    /// Average bitrate in bits/s (TS packets, including headers) over the
    /// stream's PTS span, or the program's PCR span for streams without PTS.
    pub bitrate: Option<u64>,
}

impl Stream {
    /// Broad category of [`Self::codec`].
    pub fn kind(&self) -> StreamKind {
        self.codec.kind()
    }

    /// Sync or async KLV.
    pub fn is_klv(&self) -> bool {
        self.codec.is_klv()
    }

    /// The stream is scrambled right now (its latest payload packet was).
    /// See [`Self::scrambled_packets`] for the history.
    pub fn is_scrambled(&self) -> bool {
        self.scrambled
    }

    /// Time covered by the PTS values seen so far.
    pub fn duration(&self) -> Option<Duration> {
        self.pts_span.map(|t| ticks(t, PTS_HZ))
    }

    /// First descriptor with the given tag.
    pub fn descriptor(&self, tag: u8) -> Option<&Descriptor> {
        self.descriptors.iter().find(|d| d.tag == tag)
    }
}

/// One program from the PAT, with its PMT contents once received.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct Program {
    /// program_number from the PAT.
    pub number: u16,
    /// PID carrying this program's PMT.
    pub pmt_pid: u16,
    /// PMT version, or `None` until the first PMT arrives.
    pub version: Option<u8>,
    /// PID carrying the program clock, if any.
    pub pcr_pid: Option<u16>,
    /// Program-level registration descriptor format_identifier.
    pub registration: Option<FourCc>,
    /// Every program-level descriptor, in PMT order.
    pub descriptors: Vec<Descriptor>,
    /// First PCR seen on `pcr_pid` (27 MHz, raw value).
    pub first_pcr: Option<u64>,
    /// Most recent PCR.
    pub last_pcr: Option<u64>,
    /// PCR time covered, in 27 MHz ticks. Wraparound-aware; timeline
    /// discontinuities (splices) add up rather than distort it.
    pub pcr_span: Option<u64>,
    /// Elementary streams in PMT order.
    pub streams: Vec<Stream>,
}

impl Program {
    /// The stream on `pid` in this program.
    pub fn stream(&self, pid: u16) -> Option<&Stream> {
        self.streams.iter().find(|s| s.pid == pid)
    }

    /// Streams of one kind in this program.
    pub fn streams_of(&self, kind: StreamKind) -> impl Iterator<Item = &Stream> {
        self.streams.iter().filter(move |s| s.kind() == kind)
    }

    /// Time covered by the PCR values seen so far.
    pub fn duration(&self) -> Option<Duration> {
        self.pcr_span.map(|t| ticks(t, PCR_HZ))
    }
}

/// Packet counters for one PID, including PIDs no table references.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct PidInfo {
    /// The PID.
    pub pid: u16,
    /// Packets seen.
    pub packets: u64,
    /// Continuity counter errors.
    pub cc_errors: u64,
    /// Packets with transport_error_indicator set (not otherwise parsed).
    pub transport_errors: u64,
    /// Packets with scrambled payload.
    pub scrambled_packets: u64,
    /// Feed packet number (1-based) of the latest packet.
    pub last_packet: u64,
}

/// Feed-wide counters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct FeedStats {
    /// Detected packet stride: 188, 192 (M2TS) or 204 (with RS parity).
    pub packet_size: Option<usize>,
    /// Packets parsed.
    pub packets: u64,
    /// Bytes discarded while (re)acquiring sync or as a truncated tail.
    pub bytes_skipped: u64,
    /// Times packet alignment was lost and re-acquired.
    pub sync_losses: u64,
    /// PSI sections rejected for a bad CRC.
    pub crc_errors: u64,
    /// Elementary stream entries in the current PMTs that are not tracked
    /// because the feed declares more than
    /// [`Config::max_streams`](crate::Config::max_streams).
    pub streams_dropped: u64,
}

/// Snapshot of a feed at the time [`Demuxer::report`](crate::Demuxer::report)
/// was called.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct Report {
    /// Changes whenever the program/stream structure changes.
    pub revision: u64,
    /// transport_stream_id from the PAT.
    pub transport_stream_id: Option<u16>,
    /// PAT version.
    pub pat_version: Option<u8>,
    /// PID of the network information table (PAT program 0).
    pub network_pid: Option<u16>,
    /// Conditional access systems listed in the CAT.
    pub ca_systems: Vec<CaSystem>,
    /// Programs in PAT order of program number.
    pub programs: Vec<Program>,
    /// Every PID that has carried at least one packet.
    pub pids: Vec<PidInfo>,
    /// Feed-wide counters.
    pub stats: FeedStats,
}

impl Report {
    /// All streams of all programs.
    pub fn streams(&self) -> impl Iterator<Item = &Stream> {
        self.programs.iter().flat_map(|p| &p.streams)
    }

    /// Streams of one kind across all programs.
    pub fn streams_of(&self, kind: StreamKind) -> impl Iterator<Item = &Stream> {
        self.streams().filter(move |s| s.kind() == kind)
    }

    /// Video streams.
    pub fn video(&self) -> impl Iterator<Item = &Stream> {
        self.streams_of(StreamKind::Video)
    }

    /// Audio streams.
    pub fn audio(&self) -> impl Iterator<Item = &Stream> {
        self.streams_of(StreamKind::Audio)
    }

    /// Metadata streams (KLV, ID3, SCTE-35, ...).
    pub fn metadata(&self) -> impl Iterator<Item = &Stream> {
        self.streams_of(StreamKind::Metadata)
    }

    /// Sync and async KLV streams.
    pub fn klv(&self) -> impl Iterator<Item = &Stream> {
        self.streams().filter(|s| s.is_klv())
    }

    /// Number of streams of the given kind.
    pub fn count(&self, kind: StreamKind) -> usize {
        self.streams_of(kind).count()
    }

    /// Whether any stream has this format.
    pub fn has(&self, codec: Codec) -> bool {
        self.streams().any(|s| s.codec == codec)
    }

    /// The stream on `pid`, in whichever program declares it.
    pub fn stream(&self, pid: u16) -> Option<&Stream> {
        self.streams().find(|s| s.pid == pid)
    }

    /// The program with this program_number.
    pub fn program(&self, number: u16) -> Option<&Program> {
        self.programs.iter().find(|p| p.number == number)
    }

    /// Declared streams that carried a packet within the last `window`
    /// packets of the feed. PMTs can keep declaring a stream after its
    /// source stops; this shows what is still flowing.
    pub fn active_streams(&self, window: u64) -> impl Iterator<Item = &Stream> {
        let now = self.stats.packets;
        self.streams()
            .filter(move |s| s.last_packet.is_some_and(|l| now - l < window))
    }

    /// Longest program duration measured by PCR, or for feeds without PCR
    /// the longest stream duration measured by PTS.
    pub fn duration(&self) -> Option<Duration> {
        self.programs
            .iter()
            .filter_map(Program::duration)
            .max()
            .or_else(|| self.streams().filter_map(Stream::duration).max())
    }

    /// Average bitrate of the whole feed in bits/s, over [`Self::duration`].
    pub fn bitrate(&self) -> Option<u64> {
        let d = self.duration()?.as_nanos();
        if d == 0 {
            return None;
        }
        let bits = u128::from(self.stats.packets) * (crate::PACKET_SIZE as u128 * 8);
        Some((bits * 1_000_000_000 / d) as u64)
    }
}

fn ticks(t: u64, hz: u64) -> Duration {
    Duration::new(t / hz, ((t % hz) * 1_000_000_000 / hz) as u32)
}

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = &self.stats;
        write!(f, "Transport stream")?;
        if let Some(id) = self.transport_stream_id {
            write!(f, " id {id}")?;
        }
        if let Some(v) = self.pat_version {
            write!(f, " (PAT v{v})")?;
        }
        writeln!(
            f,
            ": {} packets of {} bytes, {} bytes skipped, {} sync losses, {} CRC errors",
            s.packets,
            s.packet_size.map_or("?".into(), |n| n.to_string()),
            s.bytes_skipped,
            s.sync_losses,
            s.crc_errors
        )?;
        if let (Some(d), Some(b)) = (self.duration(), self.bitrate()) {
            writeln!(f, "  {:.3}s, {:.1} kb/s", d.as_secs_f64(), b as f64 / 1000.0)?;
        }
        for ca in &self.ca_systems {
            writeln!(f, "  CA system 0x{:04x} on PID 0x{:04x}", ca.system_id, ca.pid)?;
        }
        for p in &self.programs {
            write!(f, "  Program {} (PMT 0x{:04x}", p.number, p.pmt_pid)?;
            if let Some(v) = p.version {
                write!(f, " v{v}")?;
            }
            if let Some(pcr) = p.pcr_pid {
                write!(f, ", PCR 0x{pcr:04x}")?;
            }
            if let Some(r) = p.registration {
                write!(f, ", reg {r}")?;
            }
            if let Some(d) = p.duration() {
                write!(f, ", {:.3}s", d.as_secs_f64())?;
            }
            writeln!(f, ")")?;
            if p.version.is_none() {
                writeln!(f, "    (PMT not yet received)")?;
            }
            for st in &p.streams {
                write!(
                    f,
                    "    0x{:04x}  {:<8}  {:<16}  type 0x{:02x}",
                    st.pid,
                    st.kind().to_string(),
                    st.codec.name(),
                    st.stream_type
                )?;
                if let Some(l) = st.language {
                    write!(f, "  lang {l}")?;
                }
                if let Some(r) = st.registration {
                    write!(f, "  reg {r}")?;
                }
                if let Some(m) = st.metadata_format {
                    write!(f, "  fmt {m}")?;
                }
                if st.basis == Basis::Payload {
                    write!(f, "  (inferred from payload)")?;
                }
                write!(f, "  {} pkts", st.packets)?;
                if let Some(d) = st.duration() {
                    write!(f, ", {:.3}s", d.as_secs_f64())?;
                }
                if let Some(b) = st.bitrate {
                    write!(f, ", {:.1} kb/s", b as f64 / 1000.0)?;
                }
                if st.cc_errors > 0 {
                    write!(f, ", {} CC errors", st.cc_errors)?;
                }
                if st.is_scrambled() {
                    write!(f, ", scrambled")?;
                }
                writeln!(f)?;
            }
        }
        Ok(())
    }
}
