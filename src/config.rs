//! Demuxer configuration and errors.

use std::fmt;

/// What happens to new output when the output queue is full.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub enum Overflow {
    /// Discard the new output; the queue keeps the oldest data (default).
    #[default]
    DropNewest,
    /// Discard the oldest queued outputs to make room for the new one.
    DropOldest,
}

/// Buffer sizes and limits for a [`Demuxer`](crate::Demuxer).
///
/// Start from [`Config::default`] and change the fields you need:
///
/// ```
/// let mut config = mpegts::Config::default();
/// config.history_packets = 20_000; // keep ~3.7 MB of recent packets
/// let demux = mpegts::Demuxer::with_config(config).unwrap();
/// ```
///
/// Every buffer the demuxer allocates is bounded by one of these limits.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct Config {
    /// Sync bytes that must line up after a candidate before packet
    /// alignment is trusted (1 to 8). Higher is safer on noisy input; lower
    /// locks on sooner. Default 2.
    pub sync_confirmations: usize,
    /// Most elementary stream entries tracked across all programs. Real
    /// multiplexes stay far below the default of 4096; extra entries are
    /// counted in [`FeedStats::streams_dropped`](crate::FeedStats).
    pub max_streams: usize,
    /// PES packets inspected per stream when its PMT declaration is too
    /// vague to identify it (0 turns payload identification off). Default 4.
    pub probe_pes: u32,
    /// Recent packets kept in memory, about 200 bytes each (0 keeps none,
    /// the default). A subscription with replay receives matching packets
    /// from this history, so data that arrived before it was created (for
    /// example the first KLV, before the PMT announced it) is not lost.
    pub history_packets: usize,
    /// Most outputs waiting to be collected. Default 65,536.
    pub queue_max_items: usize,
    /// Most bytes of outputs waiting to be collected. Default 64 MiB.
    pub queue_max_bytes: usize,
    /// What happens when the output queue is full.
    pub overflow: Overflow,
    /// Largest PES packet assembled for a subscription. Longer ones are
    /// delivered cut short and flagged `truncated`. Default 8 MiB.
    pub max_pes_bytes: usize,
    /// Total bytes held by PES packets still being assembled, across all
    /// subscriptions. Default 64 MiB.
    pub max_reassembly_bytes: usize,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            sync_confirmations: 2,
            max_streams: 4096,
            probe_pes: 4,
            history_packets: 0,
            queue_max_items: 65_536,
            queue_max_bytes: 64 << 20,
            overflow: Overflow::DropNewest,
            max_pes_bytes: 8 << 20,
            max_reassembly_bytes: 64 << 20,
        }
    }
}

/// Largest value accepted for [`Config::history_packets`] (about 2 GB).
pub const MAX_HISTORY_PACKETS: usize = 10_000_000;

impl Config {
    /// Check every field is in range.
    pub fn validate(&self) -> Result<(), Error> {
        let bad = |field, reason| Err(Error::InvalidConfig { field, reason });
        if !(1..=8).contains(&self.sync_confirmations) {
            return bad("sync_confirmations", "must be 1 to 8");
        }
        if !(1..=u16::MAX as usize).contains(&self.max_streams) {
            return bad("max_streams", "must be 1 to 65535");
        }
        if self.history_packets > MAX_HISTORY_PACKETS {
            return bad("history_packets", "must be at most 10,000,000");
        }
        if self.queue_max_items == 0 {
            return bad("queue_max_items", "must be at least 1");
        }
        if self.queue_max_bytes < crate::PACKET_SIZE {
            return bad("queue_max_bytes", "must hold at least one packet (188 bytes)");
        }
        if self.max_pes_bytes < crate::PACKET_SIZE {
            return bad("max_pes_bytes", "must be at least 188");
        }
        if self.max_reassembly_bytes < self.max_pes_bytes {
            return bad("max_reassembly_bytes", "must be at least max_pes_bytes");
        }
        Ok(())
    }
}

/// Errors from configuring a demuxer or creating a subscription. Feeding
/// data never fails; damaged input is skipped and counted instead.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Error {
    /// A [`Config`] field is out of range.
    InvalidConfig {
        /// The field name.
        field: &'static str,
        /// What is wrong with it.
        reason: &'static str,
    },
    /// A [`Subscription`](crate::Subscription) asks for something impossible.
    InvalidSubscription(&'static str),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::InvalidConfig { field, reason } => write!(f, "invalid config: {field} {reason}"),
            Error::InvalidSubscription(reason) => write!(f, "invalid subscription: {reason}"),
        }
    }
}

impl std::error::Error for Error {}
