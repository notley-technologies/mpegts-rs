//! MPEG transport stream (ISO/IEC 13818-1 / ITU-T H.222.0) analyser.
//!
//! The crate tells you what a transport stream carries - programs, and for
//! each elementary stream its kind (video, audio, metadata, ...) and format
//! (H.264, AAC, synchronous or asynchronous KLV, ...) - and can extract data
//! from any stream as raw packets, PES packets or sections (see
//! [`Demuxer::subscribe`]). It does not decode the elementary streams
//! themselves and never modifies the input. All buffers are bounded by
//! [`Config`]. A stable C ABI is described in `include/mpegts.h`.
//!
//! It performs no I/O. Either hand it a complete buffer:
//!
//! ```
//! let report = mpegts::analyze(&[]);
//! assert!(report.programs.is_empty());
//! ```
//!
//! or push bytes as they arrive and ask for a report whenever you like; the
//! report always reflects the most recent PAT/PMT:
//!
//! ```no_run
//! # fn recv() -> Vec<u8> { vec![] }
//! let mut demux = mpegts::Demuxer::new();
//! let mut seen = demux.revision();
//! loop {
//!     demux.push(&recv());
//!     if demux.revision() != seen {
//!         seen = demux.revision();
//!         println!("{}", demux.report());
//!     }
//! }
//! ```
//!
//! For working on single packets there are [`PacketHeader`] (the TS header
//! and adaptation field), [`PesHeader`] (PES header fields and timestamps)
//! and [`PcrTimeline`] (the program clock time of every packet, interpolated
//! between PCRs).
//!
//! # Features
//!
//! - `capi` (default) exports the C ABI. Disable default features for a
//!   Rust-only library compiled with `#![forbid(unsafe_code)]`.
//! - `mux` adds the `mux` module, the write primitives: TS packets, PAT and PMT,
//!   PES packets, metadata access unit cells and the MISB ST 1402 KLV
//!   carriage presets, writing into caller-provided sinks without heap
//!   allocation. With `capi`, their C functions too. A build without it
//!   compiles none of that code.
//! - `serde` derives `Serialize` for the report types.

#![warn(missing_docs)]
#![cfg_attr(not(feature = "capi"), forbid(unsafe_code))]
#![cfg_attr(feature = "capi", deny(unsafe_code))]

// Lets unit tests share the integration tests' stream builder.
#[cfg(test)]
extern crate self as mpegts;

#[cfg(feature = "capi")]
#[doc(hidden)]
pub mod capi;
mod codec;
mod config;
mod crc;
mod demuxer;
#[cfg(feature = "mux")]
pub mod mux;
mod output;
mod packet;
mod psi;
mod report;
mod timeline;

pub use codec::{Basis, Codec, StreamKind};
pub use config::{Config, Error, Overflow, MAX_HISTORY_PACKETS};
pub use crc::crc32_mpeg2;
pub use demuxer::{Demuxer, PACKET_SIZE, SYNC_BYTE};
pub use output::{Output, OutputStats, Select, Subscription, SubscriptionId, Unit};
pub use packet::{
    pcr_delta, pts_delta, AdaptationField, PacketError, PacketHeader, PesError, PesHeader, NULL_PID,
    PCR_WRAP, PTS_WRAP,
};
pub use report::{
    CaSystem, Descriptor, FeedStats, FourCc, Language, PidInfo, Program, Report, Stream, PCR_HZ,
    PTS_HZ,
};
pub use timeline::{PcrTimeline, TimedPacket, DEFAULT_MAX_HELD};

/// Analyse a complete buffer (for example a whole file) in one call.
pub fn analyze(data: &[u8]) -> Report {
    let mut d = Demuxer::new();
    d.push(data);
    d.finish();
    d.report()
}
