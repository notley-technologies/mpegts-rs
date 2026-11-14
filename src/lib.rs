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

#![warn(missing_docs)]
#![deny(unsafe_code)]

// Lets unit tests share the integration tests' stream builder.
#[cfg(test)]
extern crate self as mpegts;

#[doc(hidden)]
pub mod capi;
mod codec;
mod config;
mod crc;
mod demuxer;
mod output;
mod psi;
mod report;

pub use codec::{Basis, Codec, StreamKind};
pub use config::{Config, Error, Overflow, MAX_HISTORY_PACKETS};
pub use crc::crc32_mpeg2;
pub use demuxer::{Demuxer, PACKET_SIZE, SYNC_BYTE};
pub use output::{Output, OutputStats, Select, Subscription, SubscriptionId, Unit};
pub use report::{
    CaSystem, Descriptor, FeedStats, FourCc, Language, PidInfo, Program, Report, Stream, PCR_HZ,
    PTS_HZ,
};

/// Analyse a complete buffer (for example a whole file) in one call.
pub fn analyze(data: &[u8]) -> Report {
    let mut d = Demuxer::new();
    d.push(data);
    d.finish();
    d.report()
}
