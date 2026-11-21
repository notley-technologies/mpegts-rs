//! Transport stream write primitives (feature `mux`).
//!
//! The building blocks for producing an MPEG transport stream (ISO/IEC
//! 13818-1 / ITU-T H.222.0), including the KLV metadata carriage of MISB ST
//! 1402.2:
//!
//! - [`TsWriter`]: the packets of one PID. Continuity counter, payload unit
//!   start, adaptation fields (PCR, discontinuity, random access) and
//!   stuffing.
//! - [`ProgramTables`]: the PAT and PMTs, with descriptors, version numbers
//!   and CRCs; built from scratch or from what the demuxer reported
//!   ([`ProgramTables::from_report`]). [`write_pat_section`] and
//!   [`write_pmt_section`] are the allocation-free section builders under
//!   it.
//! - [`PesWriter`]: PES packets of one elementary stream, in one of three
//!   [`Carriage`]s: plain PES (video, audio, private data), synchronous
//!   metadata in access unit cells ([`CellHeader`]), or asynchronous KLV
//!   per SMPTE RP 217.
//! - [`StreamSpec`]: what a PMT entry and its PES writer need, with the KLV
//!   presets [`StreamSpec::klv_sync`] and [`StreamSpec::klv_async`].
//! - [`Injector`]: built on the above, inserts KLV into an existing
//!   transport stream as it passes through (pass-through invariant, PSI
//!   rewrite, the T-STD insertion window, VBR or strict CBR), with
//!   [`FrameReorder`] to give per-frame metadata presentation order. It
//!   can also read and write SEI user data in the video, such as MISB ST
//!   0604 timestamps ([`VideoSei`]), take and write 192-byte M2TS records
//!   ([`RecordFormat`]), and time live input without holding it
//!   ([`ClockMode::Extrapolate`]).
//!
//! Every writer emits whole 188-byte packets into a [`PacketSink`]: a
//! closure, a `Vec<u8>`, or a fixed buffer through [`SliceSink`]. Writing
//! does not allocate: packets, PES headers and sections are built on the
//! stack, so the writers suit targets without an allocator (configuring a
//! [`StreamSpec`] or [`ProgramTables`] does allocate; the [`Injector`]
//! allocates its queues as it starts and then reuses them). A write either
//! completes or, when the sink is too small, fails with
//! [`MuxError::BufferTooSmall`] before emitting anything or changing any
//! counter.
//!
//! The values written follow the phase 0 decisions recorded in
//! `docs/standards/DECISIONS.md` and the rules in `iso13818.md` and
//! `st1402-time.md` next to it; comments cite the rule ids.
//!
//! # Example
//!
//! A program with H.264 video and synchronous KLV:
//!
//! ```
//! use mpegts::mux::{PesUnit, PesWriter, ProgramTables, StreamSpec};
//!
//! let mut tables = ProgramTables::new(1);
//! tables.add_program(1, 0x1000, Some(0x100))?;
//! let video = StreamSpec::h264();
//! let klv = StreamSpec::klv_sync(0);
//! tables.add_stream(1, 0x100, &video)?;
//! tables.add_stream(1, 0x101, &klv)?;
//!
//! let mut out = Vec::new();
//! tables.write_all(&mut out)?;
//!
//! let mut video_writer = PesWriter::new(0x100, &video)?;
//! let mut klv_writer = PesWriter::new(0x101, &klv)?;
//! let pts = 126_000;
//! let au = [0x00, 0x00, 0x00, 0x01, 0x09, 0xF0];
//! video_writer.write(&PesUnit::new(&au).with_pts(pts).with_pcr(pts * 300 - 27_000_000 / 10), &mut out)?;
//! klv_writer.write(&PesUnit::new(b"\x06\x0E\x2B\x34 ...").with_pts(pts), &mut out)?;
//!
//! let report = mpegts::analyze(&out);
//! assert_eq!(report.klv().count(), 1);
//! # Ok::<(), mpegts::mux::MuxError>(())
//! ```

mod cells;
mod error;
mod inject;
mod pes;
mod psi;
mod sink;
mod spec;
mod ts;

pub use cells::{CellFragment, CellHeader, CellNumbering, CELL_HEADER_LEN, MAX_CELL_DATA};
pub use error::MuxError;
pub use crate::ClockMode;
pub use inject::{
    DropReason, ExistingKlv, Frame, FrameReorder, InjectConfig, InjectError, InjectEvent, InjectStats, InjectWarning,
    Injector, InsertUnit, KlvCarriage, OnOverflow, ProgramSelect, RateMode, RecordFormat, Target, Units, UserDataSkip,
    VideoPrefix, VideoSei, DEFAULT_VIDEO_SEI_MAX_PREFIX, MAX_USER_DATA_PAYLOAD,
};
pub use pes::{Carriage, PesUnit, PesWriter, MAX_ASYNC_PES_DATA};
pub use psi::{
    write_descriptor, write_pat_section, write_pmt_section, PatEntry, PmtEntry, ProgramDef, ProgramTables,
    StreamDef, MAX_PSI_SECTION_LEN,
};
pub use sink::{Packet, PacketSink, Record, RecordFn, SliceSink, M2TS_RECORD_SIZE};
pub use spec::{
    klv_metadata_descriptor, klv_metadata_pointer_descriptor, metadata_std_descriptor, StreamSpec,
    SyncKlvOptions, KLVA, KLVA_REGISTRATION_DESCRIPTOR, METADATA_STD_DESCRIPTOR,
};
pub use ts::{AdaptationFlags, TsWriter, NULL_PACKET};
