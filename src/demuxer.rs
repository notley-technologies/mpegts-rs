//! Push-based transport stream analyser for one feed.

use crate::codec::{identify, sniff, Basis};
use crate::config::{Config, Error};
use crate::crc::crc32_mpeg2;
use crate::output::{
    Output, OutputStats, Outputs, Select, Subscription, SubscriptionId, Unit, FLAG_DUPLICATE,
    FLAG_RESET, MAX_SUBSCRIPTIONS,
};
use crate::psi::{parse_long, raw_descriptors, Descriptors, LongSection, SectionAssembler};
use crate::report::{
    CaSystem, FeedStats, FourCc, Language, PidInfo, Program, Report, Stream, PCR_HZ, PTS_HZ,
};
use std::collections::HashMap;
use std::fmt;
use std::mem::take;

/// Size of an MPEG-TS packet.
pub const PACKET_SIZE: usize = 188;
/// First byte of every TS packet.
pub const SYNC_BYTE: u8 = 0x47;
/// Strides tried when acquiring sync: plain TS, M2TS (4-byte prefix), and
/// TS with 16 bytes of Reed-Solomon parity.
const STRIDES: [usize; 3] = [188, 192, 204];
const PID_COUNT: usize = 0x2000;
const NULL_PID: u16 = 0x1FFF;
const CAT_PID: u16 = 0x0001;
const NO_CC: u8 = 0xFF;
/// PTS/DTS wrap at 2^33; PCR at 2^33 * 300.
const PTS_MODULUS: u64 = 1 << 33;
const PCR_MODULUS: u64 = (1 << 33) * 300;
/// PCR may jitter by a few microseconds but never runs backwards; a larger
/// backwards step, or a forward one beyond the gap below, is a new timeline.
const PCR_MAX_BACK: i64 = 27_000; // 1 ms
const PCR_MAX_GAP: i64 = 10 * 27_000_000; // 10 s
/// A stream's PTS keeps a near-constant offset from its program clock. If
/// that offset moves by more than this (5 s, in 27 MHz units), the stream
/// has jumped to another timeline.
const PTS_PCR_MAX_SHIFT: i64 = 5 * 27_000_000;
/// Without a program clock to compare against, PTS steps beyond this
/// (5 minutes) either way start a new timeline.
const PTS_MAX_JUMP: i64 = 300 * 90_000;
/// PES bytes worth collecting when a header is split across packets: the
/// longest header (9 + 255) plus a 9-byte look at the payload.
const MAX_PES_PEEK: usize = 9 + 255 + 9;

#[derive(Clone, Copy)]
enum Role {
    None,
    Pat,
    Cat,
    Pmt,
    /// The only stream on this PID.
    Es { program: u16, index: u16 },
    /// Elementary PID listed by more than one program; see `es_refs`.
    SharedEs,
}

#[derive(Clone, Copy)]
struct PidStat {
    packets: u64,
    /// Feed packet number of the latest packet on this PID.
    last_packet: u64,
    scrambled: u64,
    /// Sample of the last payload packet, so a true duplicate (identical
    /// payload) can be told apart from 16 lost packets wrapping the CC.
    fingerprint: u64,
    /// Saturating: a feed can run for months.
    cc_errors: u32,
    transport_errors: u32,
    last_cc: u8,
    /// The previous packet was a duplicate; only one is allowed.
    duplicate: bool,
    /// A packet was lost to a transport error: drop partial data.
    lost: bool,
    scrambled_now: bool,
    /// A PES header split across packets is being collected in `pes_peek`.
    peeking: bool,
    /// Some subscription follows this PID.
    output: bool,
}

impl PidStat {
    const BLANK: PidStat = PidStat {
        packets: 0,
        last_packet: 0,
        scrambled: 0,
        fingerprint: 0,
        cc_errors: 0,
        transport_errors: 0,
        last_cc: NO_CC,
        duplicate: false,
        lost: false,
        scrambled_now: false,
        peeking: false,
        output: false,
    };
}

/// `a` reduced modulo `modulus` into the range (-modulus/2, modulus/2].
fn signed_mod(a: i64, modulus: u64) -> i64 {
    let m = modulus as i64;
    let d = a.rem_euclid(m);
    if d > m / 2 {
        d - m
    } else {
        d
    }
}

/// Wraparound-aware tracker for a modular clock (PTS or PCR). The span is
/// accumulated across timeline discontinuities (splices, encoder restarts).
#[derive(Clone, Copy)]
struct Track {
    last_raw: u64,
    pos: i64,
    min: i64,
    max: i64,
    /// Span of completed timeline segments.
    done: u64,
}

impl Track {
    fn new(raw: u64) -> Self {
        let pos = raw as i64;
        Track { last_raw: raw, pos, min: pos, max: pos, done: 0 }
    }

    /// Close the current segment and start a new one at `raw`.
    fn restart(&mut self, raw: u64) {
        self.done += (self.max - self.min) as u64;
        *self = Track { done: self.done, ..Track::new(raw) };
    }

    /// Advance by the shortest signed distance modulo `modulus`, so both
    /// wraparound and out-of-order values (B-frames) are handled. Steps
    /// further than `max_back` backwards or `max_fwd` forwards start a new
    /// segment instead. Returns true when that happened.
    fn update(&mut self, raw: u64, modulus: u64, max_back: i64, max_fwd: i64) -> bool {
        let d = signed_mod(raw as i64 - self.last_raw as i64, modulus);
        if d < -max_back || d > max_fwd {
            self.restart(raw);
            return true;
        }
        self.last_raw = raw;
        self.pos += d;
        self.min = self.min.min(self.pos);
        self.max = self.max.max(self.pos);
        false
    }

    fn span(&self) -> u64 {
        self.done + (self.max - self.min) as u64
    }
}

/// Clock state of one PCR PID, shared by every program that uses it.
#[derive(Default)]
struct PcrClock {
    /// Programs whose PMT names this PID as PCR_PID.
    users: u32,
    track: Option<Track>,
    first: Option<u64>,
    last: Option<u64>,
    /// Bumped on every timeline discontinuity; stream PTS tracks follow it.
    epoch: u32,
    /// discontinuity_indicator seen: the next PCR starts a new timeline.
    restart: bool,
}

impl PcrClock {
    fn observe(&mut self, pcr: u64) {
        self.first.get_or_insert(pcr);
        self.last = Some(pcr);
        let restarted = match &mut self.track {
            None => {
                self.track = Some(Track::new(pcr));
                false
            }
            Some(t) if self.restart => {
                t.restart(pcr);
                true
            }
            Some(t) => t.update(pcr, PCR_MODULUS, PCR_MAX_BACK, PCR_MAX_GAP),
        };
        self.restart = false;
        if restarted {
            self.epoch = self.epoch.wrapping_add(1);
        }
    }
}

#[derive(Clone, Copy, Default)]
struct EsState {
    /// PES packets still to inspect for payload identification.
    probes_left: u32,
    pts: Option<Track>,
    /// Clock epoch the PTS track belongs to.
    epoch: u32,
    /// Last PTS minus the program clock, in 27 MHz units.
    pcr_offset: Option<i64>,
    /// PID counters when this stream replaced a different one on the same
    /// PID, so the report counts only this stream's packets.
    base: Counters,
}

#[derive(Clone, Copy, Default)]
struct Counters {
    packets: u64,
    cc_errors: u32,
    scrambled: u64,
}

struct ProgramState {
    info: Program,
    /// Last PMT section applied, to skip re-parsing identical repeats.
    pmt_raw: Vec<u8>,
    /// Parallel to `info.streams`.
    es: Vec<EsState>,
    /// PMT entries not tracked because of [`Config::max_streams`].
    dropped: usize,
}

/// Sections of one table version, collected until all are present.
#[derive(Default)]
struct SectionSet {
    sections: Vec<Vec<u8>>,
    version: Option<u8>,
}

impl SectionSet {
    /// Store a section. Returns true when every section of the current
    /// version is present and something changed.
    fn offer(&mut self, sec: &LongSection<'_>, raw: &[u8], crc_errors: &mut u64) -> bool {
        let n = usize::from(sec.number);
        if self.sections.get(n).is_some_and(|v| v == raw) {
            return false; // Identical repeat: the common case.
        }
        if crc32_mpeg2(raw) != 0 {
            *crc_errors += 1;
            return false;
        }
        let count = usize::from(sec.last) + 1;
        if self.version != Some(sec.version) || self.sections.len() != count {
            self.sections = vec![Vec::new(); count];
            self.version = Some(sec.version);
        }
        if n >= count {
            return false;
        }
        self.sections[n] = raw.to_vec();
        self.sections.iter().all(|v| !v.is_empty())
    }

    /// Bodies of all stored sections, in section_number order.
    fn bodies(&self) -> impl Iterator<Item = &[u8]> {
        self.sections.iter().filter_map(|raw| parse_long(raw).map(|s| s.body))
    }
}

enum Lock {
    Found(usize, usize),
    /// Keep bytes from this offset; need more data to decide.
    NeedMore(usize),
}

/// Incremental MPEG-TS analyser for a single feed.
///
/// The demuxer never performs I/O. Hand it data with [`push`](Self::push), in
/// chunks of any size and alignment, and call [`report`](Self::report)
/// whenever you want to know what the feed carries right now. For a complete
/// buffer, [`analyze`](crate::analyze) does both in one call.
///
/// Each independent feed needs its own `Demuxer`.
pub struct Demuxer {
    config: Config,
    // Framing
    stride: Option<usize>,
    /// Partial packet (when locked) or unresolved bytes (when hunting).
    carry: Vec<u8>,
    /// Trailer bytes of the current unit still to discard (M2TS / 204).
    skip: usize,
    /// Most recently locked stride, kept across `finish`.
    packet_size: Option<usize>,

    roles: Box<[Role]>,
    pids: Box<[PidStat]>,

    pat_asm: SectionAssembler,
    pat: SectionSet,
    transport_stream_id: Option<u16>,
    pat_version: Option<u8>,
    network_pid: Option<u16>,
    /// Programs in program_number order.
    programs: Vec<ProgramState>,
    /// PMT PIDs: number of programs using each, and its assembler.
    pmt_asms: HashMap<u16, (u32, SectionAssembler)>,
    /// Elementary PIDs: every (program number, stream index) listing them.
    es_refs: HashMap<u16, Vec<(u16, u16)>>,
    /// Clocks of the PCR PIDs that programs reference.
    clocks: HashMap<u16, PcrClock>,
    /// PES headers split across packets, by PID.
    pes_peek: HashMap<u16, Vec<u8>>,
    /// Stream entries tracked across all programs.
    stream_count: usize,
    cat_asm: SectionAssembler,
    cat: SectionSet,
    ca_systems: Vec<CaSystem>,

    out: Outputs,
    /// PIDs whose subscription matches need recomputing.
    dirty: Vec<u16>,

    revision: u64,
    packets: u64,
    bytes_skipped: u64,
    sync_losses: u64,
    crc_errors: u64,
}

impl Default for Demuxer {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for Demuxer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Demuxer")
            .field("stride", &self.stride)
            .field("revision", &self.revision)
            .field("packets", &self.packets)
            .field("programs", &self.programs.len())
            .finish_non_exhaustive()
    }
}

impl Demuxer {
    /// A demuxer for one new feed, with the default [`Config`].
    pub fn new() -> Self {
        Self::build(Config::default())
    }

    /// A demuxer for one new feed, with custom limits and buffer sizes.
    pub fn with_config(config: Config) -> Result<Self, Error> {
        config.validate()?;
        Ok(Self::build(config))
    }

    fn build(config: Config) -> Self {
        let mut roles = vec![Role::None; PID_COUNT].into_boxed_slice();
        roles[0] = Role::Pat;
        roles[usize::from(CAT_PID)] = Role::Cat;
        Demuxer {
            out: Outputs::new(&config),
            dirty: Vec::new(),
            config,
            stride: None,
            carry: Vec::new(),
            skip: 0,
            packet_size: None,
            roles,
            pids: vec![PidStat::BLANK; PID_COUNT].into_boxed_slice(),
            pat_asm: SectionAssembler::default(),
            pat: SectionSet::default(),
            transport_stream_id: None,
            pat_version: None,
            network_pid: None,
            programs: Vec::new(),
            pmt_asms: HashMap::new(),
            es_refs: HashMap::new(),
            clocks: HashMap::new(),
            pes_peek: HashMap::new(),
            stream_count: 0,
            cat_asm: SectionAssembler::default(),
            cat: SectionSet::default(),
            ca_systems: Vec::new(),
            revision: 0,
            packets: 0,
            bytes_skipped: 0,
            sync_losses: 0,
            crc_errors: 0,
        }
    }

    /// Feed bytes from the stream. Never fails: corrupt data is skipped and
    /// counted in the report's [`FeedStats`].
    pub fn push(&mut self, mut data: &[u8]) {
        if self.skip > 0 {
            let n = self.skip.min(data.len());
            data = &data[n..];
            self.skip -= n;
        }
        if data.is_empty() {
            return;
        }
        match self.stride {
            Some(stride) if !self.carry.is_empty() => {
                // Complete the partial packet left over from the last push.
                let need = PACKET_SIZE - self.carry.len();
                if data.len() < need {
                    self.carry.extend_from_slice(data);
                    return;
                }
                self.carry.extend_from_slice(&data[..need]);
                data = &data[need..];
                let mut pkt = [0u8; PACKET_SIZE];
                pkt.copy_from_slice(&self.carry);
                self.carry.clear();
                self.handle_packet(&pkt);
                let trailer = (stride - PACKET_SIZE).min(data.len());
                self.skip = stride - PACKET_SIZE - trailer;
                self.process(&data[trailer..], false);
            }
            Some(_) => self.process(data, false),
            None if !self.carry.is_empty() => {
                // Still hunting for sync across push boundaries (rare).
                let mut buf = take(&mut self.carry);
                buf.extend_from_slice(data);
                self.process(&buf, false);
            }
            None => self.process(data, false),
        }
    }

    /// Signal the end of input, e.g. after the last chunk of a file. Lets
    /// the final packets be accepted without waiting for later sync bytes
    /// and discards any truncated trailing packet. A later `push` starts a
    /// fresh sync search while keeping all discovered state.
    pub fn finish(&mut self) {
        if self.stride.is_none() && !self.carry.is_empty() {
            let buf = take(&mut self.carry);
            self.process(&buf, true);
        }
        self.bytes_skipped += self.carry.len() as u64;
        self.carry.clear();
        self.stride = None;
        self.skip = 0;
        self.out.finish();
    }

    /// The configuration this demuxer was built with.
    pub fn config(&self) -> &Config {
        &self.config
    }

    /// Start extracting data. Outputs are queued as packets are pushed;
    /// collect them with [`pop_output`](Self::pop_output) or
    /// [`outputs`](Self::outputs).
    ///
    /// ```
    /// use mpegts::{Codec, Demuxer, Select, Subscription, Unit};
    /// let mut demux = Demuxer::new();
    /// demux.subscribe(Subscription::new(Select::Codec(Codec::KlvSync), Unit::Pes)).unwrap();
    /// demux.subscribe(Subscription::new(Select::Codec(Codec::KlvAsync), Unit::Pes)).unwrap();
    /// # let bytes = [];
    /// demux.push(&bytes);
    /// for out in demux.outputs() {
    ///     let klv: &[u8] = out.payload(); // hand to a KLV parser, with out.pts
    /// #   let _ = klv;
    /// }
    /// ```
    pub fn subscribe(&mut self, sub: Subscription) -> Result<SubscriptionId, Error> {
        if let Select::Pid(pid) = sub.select {
            if pid > NULL_PID {
                return Err(Error::InvalidSubscription("PID must be at most 0x1FFF"));
            }
        }
        if self.out.subs.len() >= MAX_SUBSCRIPTIONS {
            return Err(Error::InvalidSubscription("too many subscriptions (limit 256)"));
        }
        let matched: Box<[Option<Unit>]> = (0..PID_COUNT).map(|pid| self.follows(sub, pid as u16)).collect();
        for (st, m) in self.pids.iter_mut().zip(matched.iter()) {
            st.output |= m.is_some();
        }
        Ok(self.out.add(sub, matched))
    }

    /// Stop a subscription. Its queued outputs stay queued; data still being
    /// assembled for it is dropped. Returns false for an unknown id.
    pub fn unsubscribe(&mut self, id: SubscriptionId) -> bool {
        if !self.out.remove(id) {
            return false;
        }
        for (pid, st) in self.pids.iter_mut().enumerate() {
            st.output = self.out.subs.iter().any(|s| s.matched[pid].is_some());
        }
        true
    }

    /// The oldest queued output, if any.
    pub fn pop_output(&mut self) -> Option<Output> {
        self.out.pop()
    }

    /// Drain the queued outputs, oldest first.
    pub fn outputs(&mut self) -> impl Iterator<Item = Output> + '_ {
        std::iter::from_fn(move || self.out.pop())
    }

    /// Optionally hand back an output's buffer so a later output can reuse
    /// it instead of allocating.
    pub fn recycle(&mut self, output: Output) {
        self.out.recycle(output);
    }

    /// Counters for extracted data.
    pub fn output_stats(&self) -> OutputStats {
        self.out.stats()
    }

    /// The packets held in the history (see [`Config::history_packets`]),
    /// oldest first, with their feed packet numbers.
    pub fn recent_packets(&self) -> impl Iterator<Item = (u64, &[u8; PACKET_SIZE])> + '_ {
        self.out.history()
    }

    // Indexed access for the C ABI.
    #[cfg(feature = "capi")]
    pub(crate) fn history_len(&self) -> usize {
        self.out.history_len()
    }

    #[cfg(feature = "capi")]
    pub(crate) fn history_get(&self, i: usize) -> Option<(u64, &[u8; PACKET_SIZE])> {
        self.out.history_get(i)
    }

    /// Increases whenever the program or stream structure changes (PAT, PMT
    /// or CAT update, or a stream identified from its payload). Cheap to poll.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Snapshot of the programs and streams currently signalled.
    pub fn report(&self) -> Report {
        let mut streams_dropped = 0;
        let programs = self
            .programs
            .iter()
            .map(|p| {
                streams_dropped += p.dropped as u64;
                let mut info = p.info.clone();
                if let Some(clock) = info.pcr_pid.and_then(|pid| self.clocks.get(&pid)) {
                    info.first_pcr = clock.first;
                    info.last_pcr = clock.last;
                    info.pcr_span = clock.track.map(|t| t.span());
                }
                for (s, es) in info.streams.iter_mut().zip(&p.es) {
                    let st = &self.pids[usize::from(s.pid)];
                    s.packets = st.packets - es.base.packets;
                    s.cc_errors = u64::from(st.cc_errors - es.base.cc_errors);
                    s.scrambled_packets = st.scrambled - es.base.scrambled;
                    s.scrambled = s.packets > 0 && st.scrambled_now;
                    s.last_packet = (s.packets > 0).then_some(st.last_packet);
                    s.pts_span = es.pts.map(|t| t.span());
                    let span = match s.pts_span {
                        Some(t) if t > 0 => Some((t, PTS_HZ)),
                        _ => info.pcr_span.filter(|&t| t > 0).map(|t| (t, PCR_HZ)),
                    };
                    s.bitrate = span.map(|(t, hz)| bits_per_second(s.packets, t, hz));
                }
                info
            })
            .collect();
        let pids = self
            .pids
            .iter()
            .enumerate()
            .filter(|(_, s)| s.packets > 0)
            .map(|(pid, s)| PidInfo {
                pid: pid as u16,
                packets: s.packets,
                cc_errors: u64::from(s.cc_errors),
                transport_errors: u64::from(s.transport_errors),
                scrambled_packets: s.scrambled,
                last_packet: s.last_packet,
            })
            .collect();
        Report {
            revision: self.revision,
            transport_stream_id: self.transport_stream_id,
            pat_version: self.pat_version,
            network_pid: self.network_pid,
            ca_systems: self.ca_systems.clone(),
            programs,
            pids,
            stats: FeedStats {
                packet_size: self.packet_size,
                packets: self.packets,
                bytes_skipped: self.bytes_skipped,
                sync_losses: self.sync_losses,
                crc_errors: self.crc_errors,
                streams_dropped,
            },
        }
    }

    // ---------------------------------------------------------------- framing

    /// Consume a contiguous slice that starts at a packet boundary (when
    /// locked) or anywhere (when hunting). Stores any unresolved tail.
    fn process(&mut self, data: &[u8], eof: bool) {
        let mut pos = 0;
        loop {
            if let Some(stride) = self.stride {
                match self.run(&data[pos..], stride) {
                    None => return,
                    Some(lost_at) => {
                        pos += lost_at;
                        self.stride = None;
                        self.sync_losses += 1;
                    }
                }
            }
            // A lone unconfirmed packet is only believable as the whole of a
            // tiny input, never as a tail after real packets.
            let allow_single = self.packet_size.is_none();
            match find_lock(&data[pos..], eof, allow_single, self.config.sync_confirmations) {
                Lock::Found(off, stride) => {
                    self.bytes_skipped += off as u64;
                    pos += off;
                    self.stride = Some(stride);
                    self.packet_size = Some(stride);
                }
                Lock::NeedMore(off) => {
                    self.bytes_skipped += off as u64;
                    self.carry.extend_from_slice(&data[pos + off..]);
                    return;
                }
            }
        }
    }

    /// Parse aligned packets. Returns the offset where sync was lost, or
    /// `None` after stashing a short aligned tail.
    #[inline]
    fn run(&mut self, mut data: &[u8], stride: usize) -> Option<usize> {
        let start = data.len();
        while data.len() >= PACKET_SIZE {
            if data[0] != SYNC_BYTE {
                return Some(start - data.len());
            }
            let pkt: &[u8; PACKET_SIZE] = data[..PACKET_SIZE].try_into().unwrap();
            self.handle_packet(pkt);
            if data.len() >= stride {
                data = &data[stride..];
            } else {
                self.skip = stride - data.len();
                return None;
            }
        }
        if let Some(&b) = data.first() {
            if b != SYNC_BYTE {
                return Some(start - data.len());
            }
            self.carry.extend_from_slice(data);
        }
        None
    }

    // ---------------------------------------------------------------- packets

    #[inline]
    fn handle_packet(&mut self, p: &[u8; PACKET_SIZE]) {
        self.packets += 1;
        let pid = (u16::from(p[1] & 0x1F) << 8) | u16::from(p[2]);
        let flags = self.analyse(pid, p);
        if !self.dirty.is_empty() {
            self.refresh_outputs();
        }
        if self.out.records_history() {
            self.out.record(self.packets, flags, p);
        }
        if self.pids[usize::from(pid)].output {
            self.out.deliver(pid, p, self.packets, flags);
        }
    }

    /// Update state from one packet. Returns the continuity flags that
    /// subscriptions need.
    #[inline]
    fn analyse(&mut self, pid: u16, p: &[u8; PACKET_SIZE]) -> u8 {
        let st = &mut self.pids[usize::from(pid)];
        st.packets += 1;
        st.last_packet = self.packets;
        if p[1] & 0x80 != 0 {
            // The header itself may be damaged. Count it, then resynchronise
            // continuity and drop partial data on this PID instead of also
            // reporting the gap as a CC error.
            st.transport_errors = st.transport_errors.saturating_add(1);
            st.last_cc = NO_CC;
            st.duplicate = false;
            st.lost = true;
            return 0;
        }
        let afc = (p[3] >> 4) & 0x3;
        let has_payload = afc & 1 != 0;
        let mut payload_start = 4;
        let mut discontinuity = false;
        let mut pcr = None;
        if afc & 2 != 0 {
            let len = usize::from(p[4]);
            payload_start = 5 + len;
            if payload_start > PACKET_SIZE {
                return 0;
            }
            if len > 0 {
                let flags = p[5];
                discontinuity = flags & 0x80 != 0;
                if flags & 0x10 != 0 && len >= 7 {
                    pcr = Some(read_pcr(&p[6..12]));
                }
            }
        }

        // Continuity (2.4.3.3): +1 per payload packet; one duplicate with an
        // identical payload is allowed; anything goes after a discontinuity.
        let scrambled = p[3] & 0xC0 != 0;
        let mut duplicate = false;
        let mut reset = false;
        if has_payload && pid != NULL_PID {
            if scrambled {
                st.scrambled += 1;
            }
            st.scrambled_now = scrambled;
            let fp = fingerprint(p, payload_start);
            let cc = p[3] & 0x0F;
            let last = st.last_cc;
            let mut cc_error = false;
            if last != NO_CC && !discontinuity {
                if cc == last && !st.duplicate && fp == st.fingerprint {
                    duplicate = true;
                } else if cc != (last + 1) & 0x0F {
                    cc_error = true;
                    st.cc_errors = st.cc_errors.saturating_add(1);
                }
            }
            st.duplicate = duplicate;
            st.last_cc = cc;
            st.fingerprint = fp;
            // A gap leaves a hole in anything being reassembled on this PID.
            reset = cc_error || take(&mut st.lost);
        } else if discontinuity && pid != NULL_PID {
            st.last_cc = NO_CC;
            st.duplicate = false;
        }

        if pcr.is_some() || discontinuity {
            if let Some(clock) = self.clocks.get_mut(&pid) {
                clock.restart |= discontinuity;
                if let Some(pcr) = pcr {
                    clock.observe(pcr);
                }
            }
        }

        let flags = if duplicate { FLAG_DUPLICATE } else { 0 } | if reset { FLAG_RESET } else { 0 };
        // Scrambled payloads can't be read; a duplicate repeats the last one.
        if !has_payload || payload_start >= PACKET_SIZE || scrambled || duplicate {
            return flags;
        }
        let pusi = p[1] & 0x40 != 0;
        let payload = &p[payload_start..];
        match self.roles[usize::from(pid)] {
            Role::None => {}
            Role::Es { .. } | Role::SharedEs => self.on_es_payload(pid, payload, pusi, reset),
            Role::Pat => {
                let mut asm = take(&mut self.pat_asm);
                if reset {
                    asm.reset();
                }
                asm.feed(payload, pusi, |s| self.on_pat_section(s));
                self.pat_asm = asm;
            }
            Role::Cat => {
                let mut asm = take(&mut self.cat_asm);
                if reset {
                    asm.reset();
                }
                asm.feed(payload, pusi, |s| self.on_cat_section(s));
                self.cat_asm = asm;
            }
            Role::Pmt => {
                let Some(entry) = self.pmt_asms.get_mut(&pid) else { return flags };
                let mut asm = take(&mut entry.1);
                if reset {
                    asm.reset();
                }
                asm.feed(payload, pusi, |s| self.on_pmt_section(pid, s));
                if let Some(entry) = self.pmt_asms.get_mut(&pid) {
                    entry.1 = asm;
                }
            }
        }
        flags
    }

    /// Elementary stream payload: look at PES headers, collecting one that
    /// is split across packets.
    fn on_es_payload(&mut self, pid: u16, payload: &[u8], pusi: bool, reset: bool) {
        let i = usize::from(pid);
        if self.pids[i].peeking {
            if !pusi && !reset {
                let buf = self.pes_peek.entry(pid).or_default();
                let room = MAX_PES_PEEK.saturating_sub(buf.len());
                buf.extend_from_slice(&payload[..payload.len().min(room)]);
                if pes_header_ready(buf) {
                    let buf = self.pes_peek.remove(&pid).unwrap_or_default();
                    self.pids[i].peeking = false;
                    self.dispatch_pes(pid, &buf);
                }
                return;
            }
            // The PES ended, or data was lost, before its header was whole.
            self.pids[i].peeking = false;
            let buf = self.pes_peek.remove(&pid).unwrap_or_default();
            if !reset {
                self.dispatch_pes(pid, &buf);
            }
        }
        if !pusi {
            return;
        }
        if pes_header_ready(payload) {
            self.dispatch_pes(pid, payload);
        } else {
            self.pids[i].peeking = true;
            self.pes_peek.insert(pid, payload.to_vec());
        }
    }

    /// Hand the start of a PES to every stream declared on `pid`.
    fn dispatch_pes(&mut self, pid: u16, pes: &[u8]) {
        match self.roles[usize::from(pid)] {
            Role::Es { program, index } => {
                if let Some(pi) = self.program_index(program) {
                    self.on_pes_start(pi, usize::from(index), pes);
                }
            }
            Role::SharedEs => {
                let refs = self.es_refs.remove(&pid).unwrap_or_default();
                for &(program, index) in &refs {
                    if let Some(pi) = self.program_index(program) {
                        self.on_pes_start(pi, usize::from(index), pes);
                    }
                }
                self.es_refs.insert(pid, refs);
            }
            _ => {}
        }
    }

    fn on_pes_start(&mut self, pi: usize, si: usize, pl: &[u8]) {
        if pl.len() < 6 || pl[..3] != [0, 0, 1] {
            return;
        }
        let sid = pl[3];
        let (epoch, pcr_now) = self.programs[pi]
            .info
            .pcr_pid
            .and_then(|pid| self.clocks.get(&pid))
            .map_or((0, None), |c| (c.epoch, c.last));
        let prog = &mut self.programs[pi];
        let (Some(s), Some(state)) = (prog.info.streams.get_mut(si), prog.es.get_mut(si)) else {
            return;
        };
        s.pes_stream_id = Some(sid);
        let mut es_start = 6;
        let mut pes_scrambled = false;
        if has_optional_header(sid) && pl.len() >= 9 {
            pes_scrambled = pl[6] & 0x30 != 0;
            let header_len = usize::from(pl[8]);
            es_start = 9 + header_len;
            let flags = pl[7] >> 6;
            if flags & 0b10 != 0 && header_len >= 5 && pl.len() >= 14 {
                let pts = read_timestamp(&pl[9..14]);
                s.first_pts.get_or_insert(pts);
                s.last_pts = Some(pts);
                let offset =
                    pcr_now.map(|pcr| signed_mod(pts as i64 * 300 - pcr as i64, PCR_MODULUS));
                match &mut state.pts {
                    None => state.pts = Some(Track::new(pts)),
                    Some(t) if state.epoch != epoch => t.restart(pts),
                    Some(t) => match (offset, state.pcr_offset) {
                        (Some(now), Some(before))
                            if signed_mod(now - before, PCR_MODULUS).abs() > PTS_PCR_MAX_SHIFT =>
                        {
                            t.restart(pts)
                        }
                        // Measured against the program clock, gaps of any
                        // length are real (sparse KLV, subtitles).
                        (Some(_), _) => {
                            t.update(pts, PTS_MODULUS, i64::MAX, i64::MAX);
                        }
                        (None, _) => {
                            t.update(pts, PTS_MODULUS, PTS_MAX_JUMP, PTS_MAX_JUMP);
                        }
                    },
                }
                state.epoch = epoch;
                state.pcr_offset = offset;
                if flags == 0b11 && header_len >= 10 && pl.len() >= 19 {
                    s.last_dts = Some(read_timestamp(&pl[14..19]));
                }
            }
        }
        if state.probes_left == 0 || pes_scrambled {
            return;
        }
        let Some(es) = pl.get(es_start..) else { return };
        if es.len() < 9 {
            return; // Not enough to judge; try the next PES.
        }
        if !s.codec.is_generic() {
            state.probes_left = 0;
            return;
        }
        state.probes_left -= 1;
        if let Some(codec) = sniff(sid, es) {
            s.codec = codec;
            s.basis = Basis::Payload;
            state.probes_left = 0;
            self.revision += 1;
            let pid = s.pid;
            self.mark_dirty(pid);
        }
    }

    // ---------------------------------------------------------------- PSI

    fn on_pat_section(&mut self, s: &[u8]) {
        let Some(sec) = parse_long(s) else { return };
        if sec.table_id != 0x00 || !sec.current {
            return;
        }
        if !self.pat.offer(&sec, s, &mut self.crc_errors) {
            return;
        }
        let mut network_pid = None;
        let mut entries: Vec<(u16, u16)> = Vec::new();
        for body in self.pat.bodies() {
            for e in body.chunks_exact(4) {
                let number = u16::from_be_bytes([e[0], e[1]]);
                let pid = u16::from_be_bytes([e[2] & 0x1F, e[3]]);
                if number == 0 {
                    network_pid = Some(pid);
                } else {
                    entries.push((number, pid));
                }
            }
        }
        // Stable sort: the first entry for a repeated program_number wins.
        entries.sort_by_key(|e| e.0);
        entries.dedup_by_key(|e| e.0);
        self.apply_pat(sec.id_ext, sec.version, network_pid, &entries);
    }

    /// Merge a new program list (sorted by number) into the current one.
    fn apply_pat(&mut self, tsid: u16, version: u8, network_pid: Option<u16>, entries: &[(u16, u16)]) {
        let mut changed = self.transport_stream_id != Some(tsid)
            || self.pat_version != Some(version)
            || self.network_pid != network_pid;
        if self.network_pid != network_pid {
            for pid in [self.network_pid, network_pid].into_iter().flatten() {
                self.mark_dirty(pid);
            }
        }
        self.transport_stream_id = Some(tsid);
        self.pat_version = Some(version);
        self.network_pid = network_pid;

        let mut old = take(&mut self.programs).into_iter().peekable();
        let mut programs = Vec::with_capacity(entries.len());
        for &(number, pmt_pid) in entries {
            while let Some(gone) = old.next_if(|p| p.info.number < number) {
                self.remove_program(gone);
                changed = true;
            }
            match old.next_if(|p| p.info.number == number) {
                Some(p) if p.info.pmt_pid == pmt_pid => programs.push(p),
                moved => {
                    if let Some(p) = moved {
                        self.remove_program(p);
                    }
                    programs.push(self.add_program(number, pmt_pid));
                    changed = true;
                }
            }
        }
        for gone in old {
            self.remove_program(gone);
            changed = true;
        }
        self.programs = programs;
        if changed {
            self.revision += 1;
        }
    }

    fn add_program(&mut self, number: u16, pmt_pid: u16) -> ProgramState {
        self.pmt_asms.entry(pmt_pid).or_default().0 += 1;
        self.refresh_role(pmt_pid);
        ProgramState {
            info: Program {
                number,
                pmt_pid,
                version: None,
                pcr_pid: None,
                registration: None,
                descriptors: Vec::new(),
                first_pcr: None,
                last_pcr: None,
                pcr_span: None,
                streams: Vec::new(),
            },
            pmt_raw: Vec::new(),
            es: Vec::new(),
            dropped: 0,
        }
    }

    fn remove_program(&mut self, p: ProgramState) {
        let number = p.info.number;
        for s in &p.info.streams {
            self.unref_es(s.pid, number);
        }
        self.stream_count -= p.info.streams.len();
        if let Some(pid) = p.info.pcr_pid {
            self.release_clock(pid);
            self.mark_dirty(pid);
        }
        let pmt_pid = p.info.pmt_pid;
        if let Some(entry) = self.pmt_asms.get_mut(&pmt_pid) {
            entry.0 -= 1;
            if entry.0 == 0 {
                self.pmt_asms.remove(&pmt_pid);
            }
        }
        self.refresh_role(pmt_pid);
    }

    fn on_pmt_section(&mut self, pid: u16, s: &[u8]) {
        let Some(sec) = parse_long(s) else { return };
        if sec.table_id != 0x02 || !sec.current {
            return;
        }
        let number = sec.id_ext;
        let Some(pi) = self.program_index(number) else { return };
        if self.programs[pi].info.pmt_pid != pid || self.programs[pi].pmt_raw == s {
            return; // Not this program's PMT PID, or an identical repeat.
        }
        if crc32_mpeg2(s) != 0 {
            self.crc_errors += 1;
            return;
        }

        let b = sec.body;
        if b.len() < 4 {
            return;
        }
        let pcr_pid = u16::from_be_bytes([b[0] & 0x1F, b[1]]);
        let pcr_pid = (pcr_pid != NULL_PID).then_some(pcr_pid);
        let info_len = (usize::from(b[2] & 0x0F) << 8) | usize::from(b[3]);
        let Some(pdesc) = b.get(4..4 + info_len) else { return };
        let prog_desc = Descriptors::parse(pdesc);

        let prog = &mut self.programs[pi];
        let had_pmt = prog.info.version.is_some();
        let old_pcr = prog.info.pcr_pid;
        let old_streams = take(&mut prog.info.streams);
        let old_es = take(&mut prog.es);
        self.stream_count -= old_streams.len();
        let budget = self.config.max_streams.saturating_sub(self.stream_count);

        let mut streams: Vec<Stream> = Vec::new();
        let mut es_states = Vec::new();
        let mut dropped = 0;
        let mut rest = &b[4 + info_len..];
        while rest.len() >= 5 {
            let stream_type = rest[0];
            let es_pid = u16::from_be_bytes([rest[1] & 0x1F, rest[2]]);
            let es_len = (usize::from(rest[3] & 0x0F) << 8) | usize::from(rest[4]);
            let Some(d) = rest.get(5..5 + es_len) else { break };
            rest = &rest[5 + es_len..];
            if streams.iter().any(|s| s.pid == es_pid) {
                continue;
            }
            if streams.len() == budget {
                dropped += 1;
                continue;
            }
            let es_desc = Descriptors::parse(d);
            let (codec, basis) = identify(stream_type, &es_desc, &prog_desc);
            let mut stream = Stream {
                program: number,
                pid: es_pid,
                stream_type,
                codec,
                basis,
                language: es_desc.language.map(Language),
                registration: es_desc.registration.map(FourCc),
                metadata_format: es_desc.metadata_format.map(FourCc),
                descriptors: raw_descriptors(d),
                pes_stream_id: None,
                first_pts: None,
                last_pts: None,
                last_dts: None,
                pts_span: None,
                packets: 0,
                cc_errors: 0,
                scrambled_packets: 0,
                scrambled: false,
                last_packet: None,
                bitrate: None,
            };
            let mut state = EsState { probes_left: self.config.probe_pes, ..EsState::default() };
            if had_pmt {
                let st = &self.pids[usize::from(es_pid)];
                state.base = Counters {
                    packets: st.packets,
                    cc_errors: st.cc_errors,
                    scrambled: st.scrambled,
                };
            }
            // Same PID and type as before: keep timing and payload findings.
            if let Some(i) = old_streams
                .iter()
                .position(|o| o.pid == es_pid && o.stream_type == stream_type)
            {
                let o = &old_streams[i];
                stream.pes_stream_id = o.pes_stream_id;
                stream.first_pts = o.first_pts;
                stream.last_pts = o.last_pts;
                stream.last_dts = o.last_dts;
                state = EsState { probes_left: self.config.probe_pes, ..old_es[i] };
                if codec.is_generic() {
                    state.probes_left = old_es[i].probes_left;
                    if o.basis == Basis::Payload {
                        stream.codec = o.codec;
                        stream.basis = Basis::Payload;
                    }
                }
            }
            streams.push(stream);
            es_states.push(state);
        }

        for s in &old_streams {
            self.unref_es(s.pid, number);
        }
        for (si, s) in streams.iter().enumerate() {
            self.ref_es(s.pid, number, si as u16);
        }
        if old_pcr != pcr_pid {
            if let Some(pid) = old_pcr {
                self.release_clock(pid);
                self.mark_dirty(pid);
            }
            if let Some(pid) = pcr_pid {
                self.clocks.entry(pid).or_default().users += 1;
                self.mark_dirty(pid);
            }
        }
        self.stream_count += streams.len();

        let prog = &mut self.programs[pi];
        prog.info.version = Some(sec.version);
        prog.info.pcr_pid = pcr_pid;
        prog.info.registration = prog_desc.registration.map(FourCc);
        prog.info.descriptors = raw_descriptors(pdesc);
        prog.info.streams = streams;
        prog.es = es_states;
        prog.dropped = dropped;
        prog.pmt_raw.clear();
        prog.pmt_raw.extend_from_slice(s);
        self.revision += 1;
    }

    fn on_cat_section(&mut self, s: &[u8]) {
        let Some(sec) = parse_long(s) else { return };
        if sec.table_id != 0x01 || !sec.current {
            return;
        }
        if !self.cat.offer(&sec, s, &mut self.crc_errors) {
            return;
        }
        let ca_systems: Vec<CaSystem> = self
            .cat
            .bodies()
            .flat_map(raw_descriptors)
            .filter(|d| d.tag == 0x09 && d.data.len() >= 4)
            .map(|d| CaSystem {
                system_id: u16::from_be_bytes([d.data[0], d.data[1]]),
                pid: u16::from_be_bytes([d.data[2] & 0x1F, d.data[3]]),
            })
            .collect();
        if ca_systems != self.ca_systems {
            self.ca_systems = ca_systems;
            self.revision += 1;
        }
    }

    // ---------------------------------------------------------------- indexes

    fn program_index(&self, number: u16) -> Option<usize> {
        self.programs.binary_search_by_key(&number, |p| p.info.number).ok()
    }

    fn ref_es(&mut self, pid: u16, program: u16, index: u16) {
        self.es_refs.entry(pid).or_default().push((program, index));
        self.refresh_role(pid);
    }

    fn unref_es(&mut self, pid: u16, program: u16) {
        if let Some(refs) = self.es_refs.get_mut(&pid) {
            refs.retain(|&(p, _)| p != program);
            if refs.is_empty() {
                self.es_refs.remove(&pid);
            }
        }
        self.refresh_role(pid);
    }

    fn release_clock(&mut self, pid: u16) {
        if let Some(clock) = self.clocks.get_mut(&pid) {
            clock.users -= 1;
            if clock.users == 0 {
                self.clocks.remove(&pid);
            }
        }
    }

    // ---------------------------------------------------------------- outputs

    fn mark_dirty(&mut self, pid: u16) {
        if !self.out.subs.is_empty() {
            self.dirty.push(pid);
        }
    }

    /// Recompute which subscriptions follow the PIDs whose tables changed.
    fn refresh_outputs(&mut self) {
        let mut dirty = take(&mut self.dirty);
        dirty.sort_unstable();
        dirty.dedup();
        for &pid in &dirty {
            let mut any = false;
            for si in 0..self.out.subs.len() {
                let unit = self.follows(self.out.subs[si].spec, pid);
                self.out.set_match(si, pid, unit);
                any |= unit.is_some();
            }
            self.pids[usize::from(pid)].output = any;
        }
        dirty.clear();
        self.dirty = dirty;
    }

    /// The unit `sub` takes from `pid` given the current tables, if any.
    fn follows(&self, sub: Subscription, pid: u16) -> Option<Unit> {
        let table = matches!(self.roles[usize::from(pid)], Role::Pat | Role::Cat | Role::Pmt)
            || (2..=0x1F).contains(&pid)
            || pid == 0x1FFB
            || self.network_pid == Some(pid);
        let followed = match sub.select {
            Select::Pid(p) => return (p == pid).then_some(sub.unit),
            Select::All => true,
            Select::Psi => table,
            Select::Program(n) => self.program_index(n).is_some_and(|i| {
                let p = &self.programs[i].info;
                p.pmt_pid == pid
                    || p.pcr_pid == Some(pid)
                    || self.es_refs.get(&pid).is_some_and(|r| r.iter().any(|&(m, _)| m == n))
            }),
            Select::Codec(c) => self.streams_on(pid).any(|s| s.codec == c),
            Select::Kind(k) => self.streams_on(pid).any(|s| s.kind() == k),
        };
        let fits = match sub.unit {
            Unit::Packets => true,
            Unit::Sections => table || self.streams_on(pid).any(carries_sections),
            Unit::Pes => !table && self.streams_on(pid).any(|s| !carries_sections(s)),
        };
        (followed && fits).then_some(sub.unit)
    }

    /// Streams declared on `pid`, in any program.
    fn streams_on(&self, pid: u16) -> impl Iterator<Item = &Stream> + '_ {
        self.es_refs.get(&pid).into_iter().flatten().filter_map(move |&(n, i)| {
            let pi = self.program_index(n)?;
            self.programs[pi].info.streams.get(usize::from(i))
        })
    }

    /// Recompute what `pid` carries. PSI roles win over elementary streams.
    fn refresh_role(&mut self, pid: u16) {
        self.mark_dirty(pid);
        self.roles[usize::from(pid)] = if pid == 0 {
            Role::Pat
        } else if pid == CAT_PID {
            Role::Cat
        } else if self.pmt_asms.contains_key(&pid) {
            Role::Pmt
        } else {
            match self.es_refs.get(&pid).map(Vec::as_slice) {
                Some(&[(program, index)]) => Role::Es { program, index },
                Some([_, _, ..]) => Role::SharedEs,
                _ => Role::None,
            }
        };
    }
}

/// Whether a stream carries sections rather than PES.
fn carries_sections(s: &Stream) -> bool {
    matches!(s.stream_type, 0x05 | 0x0A..=0x0D | 0x16) || s.codec == crate::Codec::Scte35
}

/// Cheap sample of a packet's payload: three words from its start, middle
/// and end. Enough to tell a repeated packet from a different one.
#[inline]
fn fingerprint(p: &[u8; PACKET_SIZE], payload_start: usize) -> u64 {
    let word = |i: usize| u64::from_le_bytes(p[i..i + 8].try_into().unwrap());
    let start = payload_start.min(PACKET_SIZE - 8);
    let middle = ((payload_start + PACKET_SIZE) / 2).min(PACKET_SIZE - 8);
    word(start) ^ word(middle).rotate_left(21) ^ word(PACKET_SIZE - 8).rotate_left(42)
}

/// Whether `pl` (the start of a PES) holds the header and a first look at
/// the payload, or as much of them as this PES will ever have.
fn pes_header_ready(pl: &[u8]) -> bool {
    if pl.len() >= MAX_PES_PEEK {
        return true;
    }
    if pl.len() < 6 {
        return false;
    }
    if pl[..3] != [0, 0, 1] {
        return true; // Not a PES start: nothing worth waiting for.
    }
    let declared = usize::from(u16::from_be_bytes([pl[4], pl[5]]));
    let end = if declared == 0 { usize::MAX } else { 6 + declared };
    let wanted = if !has_optional_header(pl[3]) {
        6 + 9
    } else if pl.len() < 9 {
        9
    } else {
        9 + usize::from(pl[8]) + 9
    };
    pl.len() >= wanted.min(end)
}

/// Average bitrate of `packets` TS packets over `ticks` of a `hz` clock.
fn bits_per_second(packets: u64, ticks: u64, hz: u64) -> u64 {
    let bits = u128::from(packets) * (PACKET_SIZE as u128 * 8);
    (bits * u128::from(hz) / u128::from(ticks)) as u64
}

/// Find the first offset where sync bytes line up at one of [`STRIDES`].
fn find_lock(buf: &[u8], eof: bool, allow_single: bool, confirmations: usize) -> Lock {
    let mut i = 0;
    while let Some(off) = buf[i..].iter().position(|&b| b == SYNC_BYTE) {
        let c = i + off;
        let mut pending = false;
        for stride in STRIDES {
            let mut matched = 0;
            let mut ok = true;
            let mut short = false;
            for k in 1..=confirmations {
                match buf.get(c + k * stride) {
                    Some(&SYNC_BYTE) => matched += 1,
                    Some(_) => {
                        ok = false;
                        break;
                    }
                    None => {
                        short = true;
                        break;
                    }
                }
            }
            if ok && !short {
                return Lock::Found(c, stride);
            }
            if ok && short {
                // At end of input, take what is consistent so far.
                if eof && c + PACKET_SIZE <= buf.len() && (matched > 0 || allow_single) {
                    return Lock::Found(c, stride);
                }
                pending = true;
            }
        }
        if pending {
            return Lock::NeedMore(c);
        }
        i = c + 1;
    }
    Lock::NeedMore(buf.len())
}

pub(crate) fn has_optional_header(stream_id: u8) -> bool {
    !matches!(stream_id, 0xBC | 0xBE | 0xBF | 0xF0 | 0xF1 | 0xF2 | 0xF8 | 0xFF)
}

/// 33-bit PTS/DTS from its 5-byte encoding.
pub(crate) fn read_timestamp(b: &[u8]) -> u64 {
    (u64::from(b[0] >> 1) & 0x07) << 30
        | u64::from(b[1]) << 22
        | u64::from(b[2] >> 1) << 15
        | u64::from(b[3]) << 7
        | u64::from(b[4] >> 1)
}

/// 27 MHz PCR from its 6-byte encoding.
pub(crate) fn read_pcr(b: &[u8]) -> u64 {
    let base = u64::from(b[0]) << 25
        | u64::from(b[1]) << 17
        | u64::from(b[2]) << 9
        | u64::from(b[3]) << 1
        | u64::from(b[4] >> 7);
    let ext = (u64::from(b[4]) & 1) << 8 | u64::from(b[5]);
    base * 300 + ext
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn track_handles_wrap_and_reorder() {
        let j = PTS_MAX_JUMP;
        let mut t = Track::new(PTS_MODULUS - 100);
        assert!(!t.update(50, PTS_MODULUS, j, j));
        assert_eq!(t.span(), 150);
        t.update(PTS_MODULUS - 200, PTS_MODULUS, j, j); // backwards across the wrap
        assert_eq!(t.span(), 250);
        t.update(10, PTS_MODULUS, j, j);
        assert_eq!(t.span(), 250);
    }

    #[test]
    fn track_handles_non_power_of_two_modulus() {
        let mut t = Track::new(PCR_MODULUS - 300);
        t.update(600, PCR_MODULUS, PCR_MAX_BACK, PCR_MAX_GAP);
        assert_eq!(t.span(), 900);
    }

    #[test]
    fn track_accumulates_segments() {
        let mut t = Track::new(1_000);
        t.update(28_001_000, PCR_MODULUS, PCR_MAX_BACK, PCR_MAX_GAP);
        assert_eq!(t.span(), 28_000_000);
        // Backwards jump: new segment.
        assert!(t.update(500, PCR_MODULUS, PCR_MAX_BACK, PCR_MAX_GAP));
        t.update(10_500, PCR_MODULUS, PCR_MAX_BACK, PCR_MAX_GAP);
        assert_eq!(t.span(), 28_010_000);
        // Forward jump beyond the gap: new segment.
        assert!(t.update(10_500 + 11 * 27_000_000, PCR_MODULUS, PCR_MAX_BACK, PCR_MAX_GAP));
        assert_eq!(t.span(), 28_010_000);
        // Small jitter backwards is tolerated.
        assert!(!t.update(10_500 + 11 * 27_000_000 - 100, PCR_MODULUS, PCR_MAX_BACK, PCR_MAX_GAP));
    }

    #[test]
    fn error_counters_saturate() {
        let mut d = Demuxer::new();
        d.pids[0x100].cc_errors = u32::MAX;
        d.pids[0x100].last_cc = 0;
        let mut p = [0xFFu8; PACKET_SIZE];
        p[..4].copy_from_slice(&[SYNC_BYTE, 0x01, 0x00, 0x15]); // CC 5: a jump
        d.handle_packet(&p);
        p[1] |= 0x80; // TEI
        d.pids[0x100].transport_errors = u32::MAX;
        d.handle_packet(&p);
        assert_eq!(d.pids[0x100].cc_errors, u32::MAX);
        assert_eq!(d.pids[0x100].transport_errors, u32::MAX);
    }

    #[test]
    fn timestamp_encodings() {
        // PTS 0x1_2345_6789 with marker bits.
        let t: u64 = 0x1_2345_6789;
        let b = [
            0x21 | ((t >> 29) & 0x0E) as u8,
            (t >> 22) as u8,
            0x01 | ((t >> 14) & 0xFE) as u8,
            (t >> 7) as u8,
            0x01 | ((t << 1) & 0xFE) as u8,
        ];
        assert_eq!(read_timestamp(&b), t);
        // PCR base 2^33 - 1, extension 299.
        let b = [0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x2B];
        assert_eq!(read_pcr(&b), ((1u64 << 33) - 1) * 300 + 299);
    }

    #[test]
    fn find_lock_prefers_188_and_waits_for_confirmation() {
        let mut buf = vec![0u8; 188 * 3 + 1];
        for i in 0..3 {
            buf[1 + i * 188] = SYNC_BYTE;
        }
        assert!(matches!(find_lock(&buf, false, false, 2), Lock::Found(1, 188)));
        assert!(matches!(find_lock(&buf[..300], false, false, 2), Lock::NeedMore(1)));
        assert!(matches!(find_lock(&buf[..300], true, false, 2), Lock::Found(1, 188)));
        assert!(matches!(find_lock(&[0u8; 50], false, false, 2), Lock::NeedMore(50)));
        // A lone packet at end of input needs `allow_single`.
        assert!(matches!(find_lock(&buf[1..189], true, false, 2), Lock::NeedMore(0)));
        assert!(matches!(find_lock(&buf[1..189], true, true, 2), Lock::Found(0, 188)));
    }

    #[test]
    fn pes_header_readiness() {
        // Optional header with 5 header bytes: wants 9 + 5 + 9 = 23 bytes.
        let mut pes = vec![0, 0, 1, 0xE0, 0, 0, 0x80, 0x80, 5];
        pes.extend([0u8; 13]);
        assert!(!pes_header_ready(&pes));
        pes.push(0);
        assert!(pes_header_ready(&pes));
        // A PES declared 8 bytes long (14 in all) is complete at 14 bytes.
        let short = [0, 0, 1, 0xE0, 0, 8, 0x80, 0x80, 5, 0x21, 0, 1, 0, 1];
        assert!(!pes_header_ready(&short[..13]));
        assert!(pes_header_ready(&short));
        assert!(pes_header_ready(&[0xAA; 6]), "not a PES start");
    }
}
