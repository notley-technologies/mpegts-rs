//! The injector's per-packet engine: what each input packet becomes, when
//! PSI copies and KLV units go out, and the T-STD bookkeeping behind those
//! decisions.

use super::plan::{plan, rewrite_pmt, Plan, PlanInput};
use super::tables::{Applied, Pids, Sections, Tables, PAT_PID};
use super::{
    DropReason, Frame, InjectConfig, InjectError, InjectEvent, InjectStats, InjectWarning,
    InsertUnit, KlvCarriage, OnOverflow, RateMode, Target, Units,
};
use crate::mux::{
    Carriage, Packet, PacketSink, PesUnit, PesWriter, TsWriter, MAX_CELL_DATA, NULL_PACKET,
};
use crate::packet::{pcr_delta, pts_delta, PacketHeader, PesHeader, NULL_PID, PCR_WRAP, PTS_WRAP};
use crate::{PACKET_SIZE, PCR_HZ};
use std::collections::VecDeque;

/// 27 MHz ticks per millisecond.
const MS: u64 = PCR_HZ / 1000;
/// A PCR step back of more than this, or forward of more than a second,
/// is a time base discontinuity even without a discontinuity_indicator
/// (the limits [`PcrTimeline`](crate::PcrTimeline) uses).
const PCR_MAX_BACK: i64 = MS as i64;
const PCR_MAX_GAP: i64 = PCR_HZ as i64;
/// TB_sys: 512 bytes drained at Rx_sys = 1 Mbit/s (R-ISO13818-147,
/// R-ISO13818-151), modelled in 27 MHz ticks: a byte takes 216 ticks to
/// leave.
const TB_SYS_TICKS_PER_BYTE: u64 = PCR_HZ / 125_000;
const TB_SYS_CAPACITY: u64 = 512 * TB_SYS_TICKS_PER_BYTE;
/// Most events kept for [`Injector::pop_event`](super::Injector::pop_event).
const MAX_EVENTS: usize = 256;
/// Most entries in the metadata buffer model.
const MAX_BN_ENTRIES: usize = 65_536;
/// Most PSI packets waiting for TB_sys room or a CBR slot.
const MAX_PSI_QUEUE: usize = 256;
/// Most packets of the merged PID waiting for CBR slots.
const MAX_FIFO: usize = 4096;

// Per-PID roles, recomputed when the tables or the plan change.
const R_PAT: u16 = 1;
const R_PMT_OWNED: u16 = 2;
const R_PSI: u16 = 4;
const R_STRIP: u16 = 8;
const R_MERGED: u16 = 16;
const R_VIDEO: u16 = 32;
const R_PCR: u16 = 64;
const R_TARGET: u16 = 128;
const R_OURS: u16 = 256;

// What a queued output packet is.
const O_PAT_END: u8 = 1;
const O_PMT_END: u8 = 2;
const O_EXTRA: u8 = 4;
const O_UNIT_FIRST: u8 = 8;
const O_UNIT_LAST: u8 = 16;
const O_INPUT: u8 = 32;

/// A packet waiting for its slot.
#[derive(Debug, Clone)]
struct Out {
    p: Packet,
    flags: u8,
    /// PTS of the unit it ends (O_UNIT_LAST), or the section length
    /// (O_PMT_END).
    value: u64,
}

/// A queued unit.
#[derive(Debug)]
struct Queued {
    pts: Option<u64>,
    data: Vec<u8>,
    epoch: u32,
}

/// Units waiting to be sent, in PTS order (synchronous) or insertion order
/// (asynchronous).
#[derive(Debug)]
pub(super) struct UnitQueue {
    items: VecDeque<Queued>,
    bytes: usize,
    max_units: usize,
    max_bytes: usize,
    pub carriage: KlvCarriage,
    /// Largest unit, by its PES bytes in B_n (synchronous).
    max_pes_bytes: usize,
    /// PTS of the last synchronous unit sent in this time base.
    last_sent: Option<u64>,
    epoch: u32,
}

/// PES bytes of a synchronous access unit of `len` bytes: a 14-byte PES
/// header and a 5-byte cell header per fragment (iso13818.md 3.12).
fn pes_bytes(len: usize) -> usize {
    len + (len + MAX_CELL_DATA - 1) / MAX_CELL_DATA * 19
}

impl UnitQueue {
    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn insert(&mut self, unit: InsertUnit) -> Result<(), InjectError> {
        if unit.data.is_empty() {
            return Err(InjectError::InvalidUnit(
                "a KLV unit needs at least one byte",
            ));
        }
        match (self.carriage, unit.pts) {
            (KlvCarriage::Sync, None) => {
                return Err(InjectError::InvalidUnit(
                    "synchronous KLV needs a PTS (R-ST1402TIME-17)",
                ))
            }
            (KlvCarriage::Async, Some(_)) => {
                return Err(InjectError::InvalidUnit(
                    "asynchronous KLV has no PTS (R-ST1402TIME-42)",
                ))
            }
            _ => {}
        }
        if unit.data.len() > self.max_bytes {
            return Err(InjectError::InvalidUnit("larger than max_queued_bytes"));
        }
        if self.carriage == KlvCarriage::Sync && pes_bytes(unit.data.len()) > self.max_pes_bytes {
            return Err(InjectError::InvalidUnit(
                "larger than the metadata buffer the metadata_STD_descriptor declares (R-ISO13818-162)",
            ));
        }
        let pts = unit.pts.map(|p| p % PTS_WRAP);
        if let (Some(p), Some(last)) = (pts, self.last_sent) {
            if pts_delta(p, last) < 0 {
                return Err(InjectError::OutOfOrder { pts: p, last });
            }
        }
        if self.items.len() >= self.max_units || self.bytes + unit.data.len() > self.max_bytes {
            return Err(InjectError::QueueFull {
                units: self.items.len(),
                bytes: self.bytes,
            });
        }
        self.bytes += unit.data.len();
        let q = Queued {
            pts,
            data: unit.data,
            epoch: self.epoch,
        };
        match pts {
            // PTS order, modulo 2^33 (D-ISO13818-17); equal PTS keep
            // insertion order.
            Some(p) => {
                let at = self
                    .items
                    .iter()
                    .rposition(|u| u.pts.map_or(true, |o| pts_delta(o, p) <= 0))
                    .map_or(0, |i| i + 1);
                self.items.insert(at, q);
            }
            None => self.items.push_back(q),
        }
        Ok(())
    }

    fn pop(&mut self) -> Option<Queued> {
        let u = self.items.pop_front()?;
        self.bytes -= u.data.len();
        Some(u)
    }
}

/// The program clock and the stream position derived from it.
#[derive(Debug, Default)]
struct Clock {
    now: Option<u64>,
    prev: Option<u64>,
    /// Continuous 27 MHz position, never backwards.
    pos: u64,
    /// A clock has been seen, so `pos` means something.
    timed: bool,
    /// 27 MHz ticks per input packet, from the last two clocks.
    tpp: u64,
    /// Total time span covered by valid steps, for rates.
    span: u64,
}

impl Clock {
    fn advance(&mut self, clock: Option<u64>) {
        self.prev = self.now;
        let Some(c) = clock.map(|c| c % PCR_WRAP) else {
            self.now = None;
            return;
        };
        if let Some(p) = self.prev {
            let d = pcr_delta(c, p);
            if (0..PCR_MAX_GAP).contains(&d) {
                self.pos += d as u64;
                self.span += d as u64;
                if d > 0 {
                    self.tpp = d as u64;
                }
            } else {
                self.pos += self.tpp;
            }
        }
        self.now = Some(c);
        self.timed = true;
    }

    /// PTS units (90 kHz) of the clock.
    fn base(&self) -> Option<u64> {
        self.now.map(|c| (c / 300) % PTS_WRAP)
    }
}

/// TB_sys occupancy from the PSI the injector sends (D-ISO13818-21).
#[derive(Debug, Default)]
struct TbSys {
    fullness: u64,
    at: u64,
}

impl TbSys {
    fn drain(&mut self, pos: u64) {
        self.fullness = self.fullness.saturating_sub(pos.saturating_sub(self.at));
        self.at = pos;
    }

    fn room(&mut self, clock: &Clock) -> bool {
        if !clock.timed {
            return true;
        }
        self.drain(clock.pos);
        self.fullness + PACKET_SIZE as u64 * TB_SYS_TICKS_PER_BYTE <= TB_SYS_CAPACITY
    }

    fn add(&mut self, clock: &Clock) {
        if clock.timed {
            self.drain(clock.pos);
            self.fullness += PACKET_SIZE as u64 * TB_SYS_TICKS_PER_BYTE;
        }
    }
}

/// The metadata buffer B_n of the KLV stream: PES bytes from arrival until
/// their PTS (R-ISO13818-160, D-ISO13818-05).
#[derive(Debug, Default)]
struct Bn {
    entries: VecDeque<(u64, u64)>,
    total: u64,
    capacity: u64,
    peak: u64,
}

impl Bn {
    fn drain(&mut self, now90: u64) {
        while let Some(&(pts, bytes)) = self.entries.front() {
            if pts_delta(pts, now90) > 0 {
                break;
            }
            self.entries.pop_front();
            self.total -= bytes;
        }
    }

    fn fits(&self, bytes: u64) -> bool {
        self.total + bytes <= self.capacity
    }

    fn add(&mut self, pts: u64, bytes: u64) {
        if self.entries.len() >= MAX_BN_ENTRIES {
            if let Some((_, b)) = self.entries.pop_front() {
                self.total -= b;
            }
        }
        let at = self
            .entries
            .iter()
            .rposition(|&(p, _)| pts_delta(p, pts) <= 0)
            .map_or(0, |i| i + 1);
        self.entries.insert(at, (pts, bytes));
        self.total += bytes;
        self.peak = self.peak.max(self.total);
    }
}

/// Where an input PES on the merged PID stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PesState {
    /// Between PES packets: ours may go here.
    Idle,
    /// Inside a PES whose start was not seen.
    Unknown,
    /// Inside a PES; `end` is its total length when bounded.
    In { at: usize, end: Option<usize> },
}

/// Joining an existing synchronous KLV stream (st1402-time.md D-5): its
/// continuity counters and cell sequence numbers are renumbered so that
/// ours fit in; access unit bytes are never changed.
#[derive(Debug)]
struct Merge {
    pid: u16,
    writer: PesWriter,
    state: PesState,
    /// The next cell's header starts at this PES offset.
    cell: usize,
    len_hi: u8,
    /// PES header bytes seen (for the PTS).
    head: [u8; 14],
    /// Next cell sequence_number of the stream (R-ISO13818-140).
    seq: Option<u8>,
    /// Continuity counter of the last packet written, and the input's
    /// counter of the last input packet written.
    out_cc: Option<u8>,
    in_cc: Option<u8>,
    /// Packets waiting for a free slot (CBR), in output order.
    fifo: VecDeque<Out>,
    /// metadata_service_ids seen in the input's cells.
    services: [bool; 256],
    our_service: u8,
    /// The input access unit being received, and the last one complete,
    /// for B_n: (PTS, PES bytes).
    pending_au: Option<(u64, u64)>,
    input_au: Option<(u64, u64)>,
    /// The input uses our service id; reported once.
    saw_ours: bool,
    warned: bool,
}

impl Merge {
    fn new(pid: u16, service_id: u8, last_cc: Option<u8>) -> Result<Self, InjectError> {
        Ok(Merge {
            pid,
            writer: PesWriter::with_carriage(pid, 0xFC, Carriage::MetadataCells { service_id })?,
            state: PesState::Unknown,
            cell: 0,
            len_hi: 0,
            head: [0; 14],
            seq: None,
            out_cc: last_cc,
            in_cc: last_cc,
            fifo: VecDeque::new(),
            services: [false; 256],
            our_service: service_id,
            pending_au: None,
            input_au: None,
            saw_ours: false,
            warned: false,
        })
    }

    fn idle(&self) -> bool {
        self.state == PesState::Idle
    }

    /// Track an input packet of the merged PID, in output order, and
    /// renumber the sequence_number of every cell it holds.
    fn take_input(&mut self, p: &mut Packet) {
        if p[1] & 0x80 != 0 {
            self.state = PesState::Unknown;
            return;
        }
        let afc = (p[3] >> 4) & 3;
        if afc & 1 == 0 {
            return;
        }
        let start = if afc & 2 != 0 {
            5 + usize::from(p[4])
        } else {
            4
        };
        if start >= PACKET_SIZE {
            return;
        }
        if p[1] & 0x40 != 0 {
            self.state = PesState::In { at: 0, end: None };
            self.cell = usize::MAX;
            self.pending_au = None;
        } else if self.state == PesState::Idle {
            self.state = PesState::Unknown;
        }
        let PesState::In { mut at, mut end } = self.state else {
            return;
        };
        let mut i = start;
        while i < PACKET_SIZE {
            if end.is_some_and(|e| at >= e) {
                break;
            }
            let b = p[i];
            if at < self.head.len() {
                self.head[at] = b;
            }
            if at <= 8 {
                if at == 5 {
                    let len = usize::from(u16::from_be_bytes([self.head[4], self.head[5]]));
                    end = (len != 0).then_some(6 + len);
                }
                if at == 8 {
                    // A metadata PES: cells start after the header
                    // (R-ISO13818-133); anything else is left alone.
                    self.cell = if self.head[..4] == [0, 0, 1, 0xFC] {
                        9 + usize::from(b)
                    } else {
                        usize::MAX
                    };
                }
                at += 1;
                i += 1;
                continue;
            }
            if self.cell == usize::MAX || at < self.cell {
                // Up to the next cell header, byte by byte while the PTS
                // is still being collected.
                let n = if at < self.head.len() {
                    1
                } else {
                    (self.cell - at).min(PACKET_SIZE - i)
                };
                at += n;
                i += n;
                continue;
            }
            match at - self.cell {
                0 => {
                    self.services[usize::from(b)] = true;
                    self.saw_ours |= b == self.our_service;
                }
                1 => {
                    // One sequence_number counter per stream, whatever the
                    // service (R-ISO13818-140, D-ISO13818-07): continue the
                    // input's numbering, counting our cells too.
                    let seq = *self.seq.get_or_insert(b);
                    p[i] = seq;
                    self.seq = Some(seq.wrapping_add(1));
                }
                3 => self.len_hi = b,
                4 => self.cell += 5 + usize::from(u16::from_be_bytes([self.len_hi, b])),
                _ => {}
            }
            at += 1;
            i += 1;
        }
        if at >= self.head.len()
            && self.pending_au.is_none()
            && self.head[3] == 0xFC
            && self.head[7] & 0x80 != 0
        {
            if let Some(e) = end {
                self.pending_au =
                    Some((crate::demuxer::read_timestamp(&self.head[9..14]), e as u64));
            }
        }
        self.state = match end {
            Some(e) if at >= e => {
                self.input_au = self.pending_au.take();
                PesState::Idle
            }
            _ => PesState::In { at, end },
        };
    }

    /// Set the continuity counter of a packet as it is written.
    fn stamp(&mut self, p: &mut Packet, input: bool) {
        let afc = (p[3] >> 4) & 3;
        let cc = p[3] & 0x0F;
        let out = if input {
            let discontinuity = afc & 2 != 0 && p[4] > 0 && p[5] & 0x80 != 0;
            let next = match (self.out_cc, self.in_cc) {
                // A packet without payload repeats the counter
                // (R-ISO13818-010); input gaps and duplicates are kept as
                // they were (R-ISO13818-011, R-ISO13818-013).
                (Some(o), _) if afc & 1 == 0 => o,
                (Some(o), _) if discontinuity => (o + 1) & 0x0F,
                (Some(o), Some(i)) => (o + ((cc + 16 - i) & 0x0F)) & 0x0F,
                (Some(o), None) => (o + 1) & 0x0F,
                (None, _) => cc,
            };
            if afc & 1 != 0 {
                self.in_cc = Some(cc);
            }
            next
        } else {
            self.out_cc.map_or(0, |o| (o + 1) & 0x0F)
        };
        p[3] = (p[3] & 0xF0) | out;
        self.out_cc = Some(out);
    }
}

/// When the KLV stream may start: after the first PMT that lists it has
/// left B_sys, plus 4 ms (D-ISO13818-13).
#[derive(Debug, Default)]
struct Gate {
    listed: bool,
    open_at: Option<u64>,
}

/// Free slot measurement for CBR.
#[derive(Debug, Default)]
struct Slots {
    in_interval: u64,
    /// Free slots PSI left to KLV, per 27 MHz tick.
    rate: f64,
    measured: bool,
}

/// Packetize a section into a queue (R-ISO13818-006, R-ISO13818-073,
/// R-ISO13818-075); `flags` mark its last packet.
fn push_section(
    w: &mut TsWriter,
    q: &mut VecDeque<Out>,
    s: &[u8],
    flags: u8,
    value: u64,
) -> Result<(), InjectError> {
    let start = q.len();
    w.write_section(s, &mut |pk: &Packet| {
        q.push_back(Out {
            p: *pk,
            flags: 0,
            value,
        })
    })?;
    if q.len() > start {
        if let Some(last) = q.back_mut() {
            last.flags = flags;
        }
    }
    Ok(())
}

/// What kind of packet the injector writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Psi,
    Klv,
    Merged,
}

/// What an input packet becomes.
enum Slot {
    /// Written unchanged.
    Keep,
    /// The injector consumed it, or it is a null packet: a free slot.
    Free { null: bool },
    /// Replaced by this packet in place.
    Replace(Packet),
    /// A packet of the merged PID.
    Merged,
}

#[derive(Debug)]
pub(super) struct Core {
    pub cfg: InjectConfig,
    max_lead: i64,
    min_lead: i64,
    fallback: i64,
    psi_interval: u64,
    psi_margin: u64,
    cbr: Option<OnOverflow>,
    tables: Tables,
    sections: Sections,
    roles: Box<[u16]>,
    observed: Pids,
    in_cc: Box<[u8]>,
    plan: Option<Plan>,
    own_changes: u8,
    warned_multi: bool,
    clock: Clock,
    last_pcr: Option<u64>,
    epoch: u32,
    frame_discontinuity: bool,
    inserted_in_interval: u64,
    pat_writer: TsWriter,
    pmt_writer: Option<TsWriter>,
    psi_q: VecDeque<Out>,
    last_pat: Option<u64>,
    last_pmt: Option<u64>,
    /// Continuity counters of the last PAT and target PMT packets written.
    pat_last_cc: Option<u8>,
    pmt_last_cc: Option<u8>,
    /// A PAT (or target PMT) waits in the PSI queue.
    pat_copy_queued: bool,
    pmt_copy_queued: bool,
    tb_sys: TbSys,
    pub queue: UnitQueue,
    klv_writer: Option<PesWriter>,
    klv_q: VecDeque<Out>,
    merge: Option<Merge>,
    gate: Gate,
    bn: Bn,
    max_video_pts: Option<u64>,
    slots: Slots,
    /// Per-stripped-PID continuity counter of the adaptation-field-only
    /// packets that keep its PCR.
    af_cc: Vec<(u16, u8)>,
    async_head_since: Option<u64>,
    pub stats: InjectStats,
    pub events: VecDeque<InjectEvent>,
}

impl Core {
    pub fn new(cfg: InjectConfig) -> Self {
        let ticks = |ms: u32| i64::from(ms) * MS as i64;
        let interval = u64::from(cfg.psi_interval_ms) * MS;
        Core {
            max_lead: ticks(cfg.max_lead_ms),
            min_lead: ticks(cfg.min_lead_ms),
            fallback: ticks(cfg.fallback_lead_ms),
            psi_interval: interval,
            psi_margin: (10 * MS).min(interval / 2),
            cbr: match cfg.rate {
                RateMode::Cbr { on_overflow } => Some(on_overflow),
                _ => None,
            },
            tables: Tables::new(),
            sections: Sections::default(),
            roles: vec![0; 0x2000].into_boxed_slice(),
            observed: Pids::default(),
            in_cc: vec![0xFF; 0x2000].into_boxed_slice(),
            plan: None,
            own_changes: 0,
            warned_multi: false,
            clock: Clock::default(),
            last_pcr: None,
            epoch: 0,
            frame_discontinuity: false,
            inserted_in_interval: 0,
            pat_writer: TsWriter::new(PAT_PID).expect("PID 0 is valid"),
            pmt_writer: None,
            psi_q: VecDeque::new(),
            last_pat: None,
            last_pmt: None,
            pat_last_cc: None,
            pmt_last_cc: None,
            pat_copy_queued: false,
            pmt_copy_queued: false,
            tb_sys: TbSys::default(),
            queue: UnitQueue {
                items: VecDeque::new(),
                bytes: 0,
                max_units: cfg.max_queued_units,
                max_bytes: cfg.max_queued_bytes,
                carriage: cfg.carriage,
                max_pes_bytes: (cfg.sync.buffer_size as usize).saturating_mul(1024),
                last_sent: None,
                epoch: 0,
            },
            klv_writer: None,
            klv_q: VecDeque::new(),
            merge: None,
            gate: Gate::default(),
            bn: Bn {
                capacity: u64::from(cfg.sync.buffer_size) * 1024,
                ..Bn::default()
            },
            max_video_pts: None,
            slots: Slots::default(),
            af_cc: Vec::new(),
            async_head_since: None,
            stats: InjectStats::default(),
            events: VecDeque::new(),
            cfg,
        }
    }

    pub fn target(&self) -> Option<&Target> {
        self.plan.as_ref().map(|p| &p.target)
    }

    pub fn clock(&self) -> Option<u64> {
        self.clock.now
    }

    fn event(&mut self, e: InjectEvent) {
        if self.events.len() >= MAX_EVENTS {
            self.events.pop_front();
            self.stats.events_dropped += 1;
        }
        self.events.push_back(e);
    }

    fn dropped(&mut self, reason: DropReason, u: &Queued) {
        match reason {
            DropReason::Late => self.stats.dropped_late += 1,
            DropReason::Overflow => self.stats.dropped_overflow += 1,
            DropReason::Discontinuity => self.stats.dropped_discontinuity += 1,
            DropReason::Unplaced => self.stats.dropped_unplaced += 1,
        }
        self.event(InjectEvent::Dropped {
            reason,
            pts: u.pts,
            bytes: u.data.len(),
        });
    }

    pub fn snapshot(&self) -> InjectStats {
        let mut s = self.stats;
        s.units_queued = self.queue.len() as u64;
        s.bytes_queued = self.queue.bytes as u64;
        s.metadata_buffer_peak = self.bn.peak;
        let rate = |packets: u64| -> u64 {
            if self.clock.span == 0 {
                0
            } else {
                (u128::from(packets) * (PACKET_SIZE as u128 * 8) * u128::from(PCR_HZ)
                    / u128::from(self.clock.span)) as u64
            }
        };
        s.null_bitrate = rate(s.null_packets);
        s.free_bitrate = if self.slots.measured {
            (self.slots.rate * PCR_HZ as f64 * (PACKET_SIZE * 8) as f64) as u64
        } else {
            0
        };
        let added = rate(s.packets_out.abs_diff(s.packets_in)) as i64;
        s.added_bitrate = if s.packets_out >= s.packets_in {
            added
        } else {
            -added
        };
        s
    }

    /// Take over from the hold: the tables seen so far decide the plan,
    /// which applies from the first held packet.
    pub fn start(&mut self, tables: Tables, observed: Pids) -> Result<(), InjectError> {
        self.tables = tables;
        self.observed = observed;
        self.replan()
    }

    // ------------------------------------------------------------ planning

    fn replan(&mut self) -> Result<(), InjectError> {
        let cell_services = self.merge.as_ref().map_or([false; 256], |m| {
            let mut s = m.services;
            s[usize::from(m.our_service)] = false;
            s
        });
        let new = plan(&PlanInput {
            cfg: &self.cfg,
            tables: &self.tables,
            observed: &self.observed,
            previous: self.plan.as_ref(),
            own_changes: self.own_changes,
            cell_services: &cell_services,
        })?;
        let old = self.plan.take();
        match (&old, &new) {
            (Some(o), None) => self.event(InjectEvent::TargetLost {
                program: o.target.program,
            }),
            (None, Some(n)) => self.event(InjectEvent::TargetFound(n.target.clone())),
            (Some(o), Some(n)) if o.target != n.target => {
                self.event(InjectEvent::TargetChanged(n.target.clone()))
            }
            _ => {}
        }
        if let Some(n) = &new {
            if let Some(o) = &old {
                if o.target.program == n.target.program
                    && o.input_version == n.input_version
                    && o.pmt_in == n.pmt_in
                {
                    if o.rewrite != n.rewrite {
                        self.own_changes = self.own_changes.wrapping_add(1);
                    }
                    if let (Some(a), Some(b)) = (o.target.service_id, n.target.service_id) {
                        if a != b {
                            self.event(InjectEvent::Warning(InjectWarning::ServiceIdChanged {
                                from: a,
                                to: b,
                            }));
                        }
                    }
                }
                if o.target.program != n.target.program {
                    self.own_changes = 0;
                }
            }
            if n.programs > 1 && self.cbr.is_none() && !self.warned_multi {
                self.warned_multi = true;
                self.event(InjectEvent::Warning(InjectWarning::MultiProgramVbr {
                    programs: n.programs,
                }));
            }
            self.apply_plan(old.as_ref(), n)?;
        }
        self.plan = new;
        self.set_roles();
        Ok(())
    }

    /// Writers and state for a new plan.
    fn apply_plan(&mut self, old: Option<&Plan>, n: &Plan) -> Result<(), InjectError> {
        let t = &n.target;
        self.bn.capacity = n.bn_capacity;
        let same_stream = old.is_some_and(|o| {
            o.target.klv_pid == t.klv_pid
                && o.target.merged == t.merged
                && o.target.service_id == t.service_id
        });
        if !same_stream {
            // A new entry or service: the KLV waits for the first PMT that
            // lists it (D-ISO13818-13).
            self.gate = Gate::default();
            let last_cc = (self.in_cc[usize::from(t.klv_pid)] != 0xFF)
                .then_some(self.in_cc[usize::from(t.klv_pid)]);
            if t.merged {
                let keep = self
                    .merge
                    .as_ref()
                    .filter(|m| m.pid == t.klv_pid)
                    .map(|m| (m.seq, m.out_cc, m.in_cc));
                let mut m = Merge::new(t.klv_pid, t.service_id.unwrap_or(0), last_cc)?;
                if let Some((seq, out_cc, in_cc)) = keep {
                    m.seq = seq;
                    m.out_cc = out_cc;
                    m.in_cc = in_cc;
                }
                self.merge = Some(m);
                self.klv_writer = None;
            } else {
                let mut w = match t.carriage {
                    KlvCarriage::Sync => PesWriter::klv_sync(t.klv_pid, t.service_id.unwrap_or(0))?,
                    KlvCarriage::Async => PesWriter::klv_async(t.klv_pid)?,
                };
                match self.klv_writer.as_ref().filter(|w| w.pid() == t.klv_pid) {
                    // Same PID, new service id: the stream continues.
                    Some(old) => {
                        w.ts_mut()
                            .set_continuity_counter(old.ts().continuity_counter());
                        w.set_sequence_number(old.sequence_number());
                    }
                    // Taking over an input PID continues its counter
                    // (R-ISO13818-013).
                    None => {
                        if let Some(cc) = last_cc {
                            w.ts_mut().set_continuity_counter(cc + 1);
                        }
                    }
                }
                self.klv_writer = Some(w);
                self.merge = None;
            }
        }
        // Taking over a PMT PID (at the start, after the target moved or
        // came back) continues the counter of the input's packets that went
        // out on it (R-ISO13818-013).
        if old.map(|o| o.target.pmt_pid) != Some(t.pmt_pid) {
            let mut w = TsWriter::new(t.pmt_pid)?;
            if let Some(cc) = (self.in_cc[usize::from(t.pmt_pid)] != 0xFF)
                .then_some(self.in_cc[usize::from(t.pmt_pid)])
            {
                w.set_continuity_counter(cc + 1);
            }
            self.pmt_writer = Some(w);
            self.last_pmt = None;
            self.pmt_last_cc = None;
        }
        Ok(())
    }

    fn set_roles(&mut self) {
        self.roles.fill(0);
        self.roles[usize::from(PAT_PID)] = R_PAT;
        self.roles[1] |= R_PSI;
        for p in &self.tables.programs {
            self.roles[usize::from(p.pmt_pid)] |= R_PSI;
        }
        let Some(plan) = &self.plan else { return };
        let t = &plan.target;
        for &pid in &plan.es_pids {
            self.roles[usize::from(pid)] |= R_TARGET;
        }
        for &pid in &plan.strip {
            self.roles[usize::from(pid)] = (self.roles[usize::from(pid)] & !R_TARGET) | R_STRIP;
        }
        if let Some(pid) = t.video_pid {
            self.roles[usize::from(pid)] |= R_VIDEO;
        }
        if let Some(pid) = t.pcr_pid {
            self.roles[usize::from(pid)] |= R_PCR;
        }
        let k = usize::from(t.klv_pid);
        if t.merged {
            self.roles[k] |= R_MERGED;
        } else if self.roles[k] & R_STRIP == 0 {
            self.roles[k] |= R_OURS;
        }
        let m = usize::from(t.pmt_pid);
        self.roles[m] = (self.roles[m] & (R_PCR | R_PMT_OWNED)) | R_PMT_OWNED;
        // A PCR_PID may be PID 0 (R-ISO13818-016).
        self.roles[usize::from(PAT_PID)] = R_PAT | (self.roles[usize::from(PAT_PID)] & R_PCR);
    }

    // ------------------------------------------------------------ packets

    /// One input packet with its program clock.
    pub fn process<S, F>(
        &mut self,
        p: &Packet,
        clock: Option<u64>,
        sink: &mut S,
        on_frame: &mut F,
    ) -> Result<(), InjectError>
    where
        S: PacketSink + ?Sized,
        F: FnMut(&Frame, &mut Units<'_>) + ?Sized,
    {
        self.stats.packets_in += 1;
        self.clock.advance(clock);
        let Ok(h) = PacketHeader::parse(p) else {
            // Not a valid packet (push_packet input): pass it on.
            return self.write_slot(Slot::Keep, p, sink);
        };
        let pid = h.pid;
        let roles = self.roles[usize::from(pid)];
        let prev_cc = self.in_cc[usize::from(pid)];
        if !h.transport_error && h.has_payload() {
            self.in_cc[usize::from(pid)] = h.continuity_counter;
        }
        self.observed.set(pid);

        let mut new_base = false;
        if roles & R_PCR != 0 && !h.transport_error {
            if let Some(pcr) = h.pcr() {
                new_base = self.on_pcr(pcr, h.discontinuity())?;
            }
        }
        if roles & R_TARGET != 0 && h.scrambling != 0 && h.has_payload() {
            let program = self.plan.as_ref().map_or(0, |pl| pl.target.program);
            return Err(InjectError::Scrambled { program, pid });
        }
        // A packet whose header may be damaged (transport_error_indicator)
        // proves nothing about the PID; receivers discard it anyway.
        if roles & R_OURS != 0 && !h.transport_error {
            return Err(InjectError::PidInUse {
                pid,
                reason: "the input started using the KLV PID",
            });
        }
        if (1..=3).contains(&pid) {
            // CAT and the other system PIDs enter TB_sys too
            // (R-ISO13818-153).
            self.tb_sys.add(&self.clock);
        }

        let mut frame = None;
        let slot = if pid == NULL_PID {
            self.stats.null_packets += 1;
            Slot::Free { null: true }
        } else if roles & (R_PAT | R_PMT_OWNED) != 0 {
            self.owned_psi(p, pid, roles)?;
            match h.pcr() {
                // A PCR carried on a PSI PID stays where it is, in an
                // adaptation-field-only packet (R-ISO13818-016,
                // R-ISO13818-040).
                Some(_) if !h.transport_error => Slot::Replace(self.psi_pcr_only(p, pid)),
                _ => Slot::Free { null: false },
            }
        } else if roles & R_STRIP != 0 {
            self.stats.stripped_packets += 1;
            match h.pcr() {
                Some(_) if !h.transport_error => Slot::Replace(self.pcr_only(p, &h, prev_cc)),
                _ => Slot::Free { null: false },
            }
        } else {
            if roles & R_PSI != 0 {
                self.other_psi(p, pid)?;
            }
            if roles & R_VIDEO != 0 && h.payload_unit_start && !h.transport_error {
                if let Ok(pes) = PesHeader::parse(h.payload(p)) {
                    if pes.scrambling != 0 {
                        let program = self.plan.as_ref().map_or(0, |pl| pl.target.program);
                        return Err(InjectError::Scrambled { program, pid });
                    }
                    if let Some(pts) = pes.pts {
                        frame = Some(Frame {
                            pid,
                            pts,
                            dts: pes.dts,
                            clock: self.clock.now,
                            random_access: h.random_access(),
                            discontinuity: std::mem::take(&mut self.frame_discontinuity),
                        });
                    }
                }
            }
            if roles & R_MERGED != 0 {
                Slot::Merged
            } else {
                Slot::Keep
            }
        };

        self.extra_psi()?;
        let mut frame_pts = None;
        if let Some(f) = frame {
            self.stats.frames += 1;
            if self.max_video_pts.map_or(true, |m| pts_delta(f.pts, m) > 0) {
                self.max_video_pts = Some(f.pts);
            }
            frame_pts = Some(f.pts);
            let mut units = Units {
                queue: &mut self.queue,
                clock: self.clock.now,
                target: self.plan.as_ref().map(|pl| &pl.target),
            };
            on_frame(&f, &mut units);
        }
        let merged_start = roles & R_MERGED != 0 && h.payload_unit_start;
        self.schedule(frame_pts, merged_start, new_base)?;
        self.write_slot(slot, p, sink)
    }

    /// An adaptation-field-only packet keeping the PCR of a removed stream
    /// (R-ISO13818-010: its counter repeats the previous packet's).
    fn pcr_only(&mut self, p: &Packet, h: &PacketHeader, prev_cc: u8) -> Packet {
        let cc = match self.af_cc.iter().find(|(pid, _)| *pid == h.pid) {
            Some(&(_, cc)) => cc,
            None => {
                let cc = if prev_cc != 0xFF {
                    prev_cc
                } else {
                    h.continuity_counter
                };
                self.af_cc.push((h.pid, cc));
                cc
            }
        };
        let mut q = [0xFF; PACKET_SIZE];
        q[0] = p[0];
        q[1] = p[1] & 0x1F;
        q[2] = p[2];
        q[3] = 0x20 | cc;
        q[4] = 183;
        let af = usize::from(p[4]);
        q[5..5 + af].copy_from_slice(&p[5..5 + af]);
        self.stats.pcr_only_packets += 1;
        q
    }

    /// The adaptation field of an owned PSI packet that carries a PCR, in a
    /// packet without payload; its counter repeats the last one written on
    /// the PID (R-ISO13818-010).
    fn psi_pcr_only(&mut self, p: &Packet, pid: u16) -> Packet {
        let (last, writer) = if pid == PAT_PID {
            (self.pat_last_cc, Some(&self.pat_writer))
        } else {
            (self.pmt_last_cc, self.pmt_writer.as_ref())
        };
        let cc = last
            .unwrap_or_else(|| writer.map_or(0, |w| w.continuity_counter().wrapping_sub(1) & 0x0F));
        let mut q = [0xFF; PACKET_SIZE];
        q[0] = p[0];
        q[1] = p[1] & 0x1F;
        q[2] = p[2];
        q[3] = 0x20 | cc;
        q[4] = 183;
        let af = usize::from(p[4]);
        q[5..5 + af].copy_from_slice(&p[5..5 + af]);
        self.stats.pcr_only_packets += 1;
        self.tb_sys.add(&self.clock);
        q
    }

    /// PCR on the target's PCR_PID: time base discontinuities
    /// (R-ISO13818-029, R-ISO13818-031) and per-interval measurements.
    /// Returns whether this PCR starts a new time base.
    fn on_pcr(&mut self, pcr: u64, flagged: bool) -> Result<bool, InjectError> {
        let jump = match self.last_pcr {
            Some(last) => {
                let d = pcr_delta(pcr, last);
                flagged || !(-PCR_MAX_BACK..PCR_MAX_GAP).contains(&d)
            }
            None => false,
        };
        if let (Some(last), false) = (self.last_pcr, jump) {
            let d = pcr_delta(pcr, last);
            if d > 0 {
                let sample = self.slots.in_interval as f64 / d as f64;
                self.slots.rate = if self.slots.measured {
                    0.75 * self.slots.rate + 0.25 * sample
                } else {
                    sample
                };
                self.slots.measured = true;
            }
        }
        self.slots.in_interval = 0;
        self.inserted_in_interval = 0;
        self.last_pcr = Some(pcr);
        if jump {
            self.discontinuity(pcr)?;
        }
        Ok(jump)
    }

    /// A new time base: units stamped in the old one go out before this
    /// packet if their window allows, or are dropped; none crosses
    /// (R-ISO13818-031, D-ISO13818-14).
    fn discontinuity(&mut self, pcr: u64) -> Result<(), InjectError> {
        let old = self.clock.prev.map(|c| (c / 300) % PTS_WRAP);
        let mut keep = VecDeque::new();
        while let Some(u) = self.queue.pop() {
            let Some(pts) = u.pts else {
                keep.push_back(u);
                continue;
            };
            let lead = old.map(|b| pts_delta(pts, b) * 300);
            let in_window = lead.is_some_and(|l| l >= self.min_lead && l <= self.max_lead);
            let merge_idle = self.merge.as_ref().map_or(true, |m| m.idle());
            if in_window && self.cbr.is_none() && self.gate_open() && merge_idle {
                self.emit_unit(u)?;
            } else {
                self.dropped(DropReason::Discontinuity, &u);
            }
        }
        for u in keep {
            self.queue.bytes += u.data.len();
            self.queue.items.push_back(u);
        }
        if self.cbr.is_some() {
            // Whole units not yet started are dropped; one partly sent
            // finishes.
            if let Some(i) = self.klv_q.iter().position(|o| o.flags & O_UNIT_FIRST != 0) {
                let dropped = self
                    .klv_q
                    .drain(i..)
                    .filter(|o| o.flags & O_UNIT_LAST != 0)
                    .count();
                self.stats.dropped_discontinuity += dropped as u64;
            }
        }
        self.epoch = self.epoch.wrapping_add(1);
        self.queue.epoch = self.epoch;
        self.queue.last_sent = None;
        self.max_video_pts = None;
        self.frame_discontinuity = true;
        self.event(InjectEvent::Discontinuity { pcr });
        Ok(())
    }

    // ------------------------------------------------------------ PSI

    /// PID 0 and the target's PMT PID: every section is sent on in its own
    /// packets, with the injector's continuity counter, at the position of
    /// the packet that completes it (DECISIONS X-3).
    fn owned_psi(&mut self, p: &Packet, pid: u16, roles: u16) -> Result<(), InjectError> {
        let mut sections = std::mem::take(&mut self.sections);
        sections.clear();
        self.tables.assemble(p, &mut sections);
        let mut r = Ok(());
        for i in 0..sections.len() {
            r = self.owned_section(pid, roles, sections.get(i));
            if r.is_err() {
                break;
            }
        }
        self.sections = sections;
        r
    }

    fn owned_section(&mut self, pid: u16, roles: u16, s: &[u8]) -> Result<(), InjectError> {
        let Some(sec) = crate::psi::parse_long(s) else {
            // Not a long-form section: copied on the PMT PID, dropped on
            // PID 0 (only the PAT goes there).
            if roles & R_PMT_OWNED != 0 {
                self.queue_section(pid, s, 0, 0)?;
            }
            return Ok(());
        };
        if roles & R_PAT != 0 {
            if sec.table_id != 0x00 {
                return Ok(());
            }
            match self.tables.apply(pid, s) {
                Applied::CrcError => {
                    self.stats.psi_crc_errors += 1;
                    return Ok(());
                }
                Applied::Changed => self.replan()?,
                Applied::Same => {}
            }
            // The input's PAT, unchanged (R-ISO13818-081: the injector
            // never alters it).
            self.pat_copy_queued = true;
            return self.queue_section(PAT_PID, s, O_PAT_END, 0);
        }
        let target = self.plan.as_ref().map(|pl| pl.target.program);
        if sec.table_id == 0x02 && Some(sec.id_ext) == target {
            match self.tables.apply(pid, s) {
                Applied::CrcError => {
                    self.stats.psi_crc_errors += 1;
                    return Ok(());
                }
                Applied::Changed => self.replan()?,
                Applied::Same => {}
            }
            let Some(plan) = &self.plan else {
                return Ok(());
            };
            if sec.current && sec.number == 0 && sec.last == 0 && plan.pmt_in == s {
                if let (Some(out), Some(w)) = (&plan.pmt_out, &mut self.pmt_writer) {
                    if w.pid() == pid {
                        push_section(w, &mut self.psi_q, out, O_PMT_END, out.len() as u64)?;
                        self.pmt_copy_queued = true;
                    }
                }
            } else if !sec.current {
                // A "next" section becomes current with the same mapping
                // (R-ISO13818-094, D-ISO13818-12).
                let version = (sec.version + 1).wrapping_add(self.own_changes) & 0x1F;
                if let Ok(out) = rewrite_pmt(s, &plan.rewrite, version) {
                    self.queue_section(pid, &out, 0, 0)?;
                }
            }
            return Ok(());
        }
        // Another program's section on the shared PMT PID: copied.
        if self.tables.apply(pid, s) == Applied::Changed {
            self.replan()?;
        }
        self.queue_section(pid, s, 0, 0)
    }

    /// CAT and other programs' PMT PIDs: parsed, passed through.
    fn other_psi(&mut self, p: &Packet, pid: u16) -> Result<(), InjectError> {
        let mut sections = std::mem::take(&mut self.sections);
        sections.clear();
        self.tables.assemble(p, &mut sections);
        let mut changed = false;
        for i in 0..sections.len() {
            match self.tables.apply(pid, sections.get(i)) {
                Applied::Changed => changed = true,
                Applied::CrcError => self.stats.psi_crc_errors += 1,
                Applied::Same => {}
            }
        }
        self.sections = sections;
        if changed {
            self.replan()?;
        }
        Ok(())
    }

    /// Packetize a section into the PSI queue (R-ISO13818-006,
    /// R-ISO13818-073, R-ISO13818-075).
    fn queue_section(
        &mut self,
        pid: u16,
        s: &[u8],
        flags: u8,
        value: u64,
    ) -> Result<(), InjectError> {
        let writer = if pid == PAT_PID {
            &mut self.pat_writer
        } else {
            match self.pmt_writer.as_mut().filter(|w| w.pid() == pid) {
                Some(w) => w,
                None => return Ok(()),
            }
        };
        push_section(writer, &mut self.psi_q, s, flags, value)
    }

    /// Copies so that neither the PAT nor the target PMT is ever more than
    /// the PSI interval apart (DECISIONS X-3, R-ST1402TIME-03).
    fn extra_psi(&mut self) -> Result<(), InjectError> {
        if !self.clock.timed {
            return Ok(());
        }
        let due = |last: Option<u64>, pos: u64, interval: u64, margin: u64| {
            last.map_or(true, |t| pos.saturating_sub(t) + margin >= interval)
        };
        let pos = self.clock.pos;
        if !self.pat_copy_queued && due(self.last_pat, pos, self.psi_interval, self.psi_margin) {
            if let Some(pat) = &self.tables.pat {
                let n = pat.sections.len();
                for (i, s) in pat.sections.iter().enumerate() {
                    let flags = O_EXTRA | if i + 1 == n { O_PAT_END } else { 0 };
                    push_section(&mut self.pat_writer, &mut self.psi_q, s, flags, 0)?;
                }
                self.pat_copy_queued = n > 0;
            }
        }
        if !self.pmt_copy_queued && due(self.last_pmt, pos, self.psi_interval, self.psi_margin) {
            if let (Some(plan), Some(w)) = (&self.plan, &mut self.pmt_writer) {
                if let (Some(out), true) = (&plan.pmt_out, w.pid() == plan.target.pmt_pid) {
                    push_section(
                        w,
                        &mut self.psi_q,
                        out,
                        O_EXTRA | O_PMT_END,
                        out.len() as u64,
                    )?;
                    self.pmt_copy_queued = true;
                }
            }
        }
        Ok(())
    }

    // ------------------------------------------------------------ KLV

    fn gate_open(&self) -> bool {
        self.gate.listed && self.gate.open_at.map_or(true, |t| self.clock.pos >= t)
    }

    /// Packets a unit takes.
    fn packets_for(&self, u: &Queued) -> usize {
        let unit = match u.pts {
            Some(pts) => PesUnit::new(&u.data).with_pts(pts),
            None => PesUnit::new(&u.data),
        };
        let w = match (&self.merge, &self.klv_writer) {
            (Some(m), _) => &m.writer,
            (None, Some(w)) => w,
            _ => return 1,
        };
        w.packets_needed(&unit).unwrap_or(1)
    }

    /// Packets waiting for free slots.
    fn backlog(&self) -> usize {
        self.klv_q.len() + self.merge.as_ref().map_or(0, |m| m.fifo.len())
    }

    /// Whether a unit of `packets` packets can be placed in CBR free slots
    /// with its last byte at least `min_lead` before its deadline; `None`
    /// until the first PCR interval measured the free slot rate (MUXING_PLAN
    /// 6.9).
    fn cbr_fits(&self, lead: i64, packets: usize) -> Option<bool> {
        if !self.slots.measured {
            return None;
        }
        if self.slots.rate <= 0.0 {
            return Some(false);
        }
        let ticks = (self.backlog() + packets) as f64 / self.slots.rate;
        Some((lead as f64) - ticks >= self.min_lead as f64)
    }

    fn overflow(&mut self, u: Queued, packets: usize) -> Result<(), InjectError> {
        match self.cbr {
            Some(OnOverflow::DropOldest) => {
                self.dropped(DropReason::Overflow, &u);
                Ok(())
            }
            _ => Err(InjectError::CbrOverflow {
                pts: u.pts,
                packets,
                free_per_second: (self.slots.rate * PCR_HZ as f64) as u64,
            }),
        }
    }

    /// Move the units that are due into the output queues. `new_base`: the
    /// current packet carries the first PCR of a new time base, so no unit
    /// with a PTS may go before it (R-ISO13818-031).
    fn schedule(
        &mut self,
        frame_pts: Option<u64>,
        merged_start: bool,
        new_base: bool,
    ) -> Result<(), InjectError> {
        if self.plan.is_none() || self.queue.items.is_empty() || !self.gate_open() {
            return Ok(());
        }
        if let (Some(now), true) = (self.clock.base(), self.cfg.carriage == KlvCarriage::Sync) {
            self.bn.drain(now);
        }
        loop {
            let Some(head) = self.queue.items.front() else {
                return Ok(());
            };
            if let Some(m) = &self.merge {
                // Our PES goes only between complete input PES packets
                // (st1402-time.md D-5).
                if !m.idle() && !merged_start {
                    return Ok(());
                }
            }
            let Some(pts) = head.pts else {
                // Asynchronous: at the next opportunity (R-ST1402TIME-47).
                if self.cbr.is_some() && !self.klv_q.is_empty() {
                    // One unit at a time; a unit that waits longer than the
                    // window for room overflows.
                    let since = *self.async_head_since.get_or_insert(self.clock.pos);
                    if self.clock.timed
                        && self.clock.pos.saturating_sub(since) > self.max_lead as u64
                    {
                        self.async_head_since = None;
                        let u = self.queue.pop().expect("head exists");
                        let n = self.packets_for(&u);
                        self.overflow(u, n)?;
                        continue;
                    }
                    return Ok(());
                }
                self.async_head_since = None;
                let u = self.queue.pop().expect("head exists");
                self.emit_unit(u)?;
                continue;
            };
            if head.epoch != self.epoch {
                let u = self.queue.pop().expect("head exists");
                self.dropped(DropReason::Discontinuity, &u);
                continue;
            }
            if new_base {
                return Ok(());
            }
            let packets = self.packets_for(head);
            let aligned_by_video = |core: &Core| {
                !core.cfg.align_to_frames
                    || core
                        .plan
                        .as_ref()
                        .map_or(true, |p| p.target.video_pid.is_none())
                    || frame_pts.is_some_and(|f| pts_delta(f, pts) >= 0)
                    || core.max_video_pts.is_some_and(|m| pts_delta(m, pts) >= 0)
            };
            let Some(now) = self.clock.base() else {
                // No program clock: frame alignment alone decides.
                if !aligned_by_video(self) {
                    return Ok(());
                }
                let u = self.queue.pop().expect("head exists");
                self.emit_unit(u)?;
                continue;
            };
            let lead = pts_delta(pts, now) * 300;
            let tpp = self.clock.tpp as i64;
            // In VBR the packets added in a PCR interval spread its T-STD
            // times: the last byte of this unit arrives up to one packet
            // time per added packet later, its first byte up to one packet
            // time per packet of the unit earlier (R-ISO13818-043).
            let (last_lead, first_lead) = if self.cbr.is_none() {
                let added = self.inserted_in_interval as i64 + packets as i64;
                (lead - added * tpp, lead + packets as i64 * tpp)
            } else {
                (lead, lead)
            };
            if last_lead < self.min_lead {
                let u = self.queue.pop().expect("head exists");
                self.dropped(DropReason::Late, &u);
                continue;
            }
            if first_lead > self.max_lead {
                return Ok(());
            }
            if !(aligned_by_video(self) || lead <= self.fallback) {
                return Ok(());
            }
            let bytes = pes_bytes(head.data.len()) as u64;
            if !self.bn.fits(bytes) {
                // B_n would overflow (R-ISO13818-162): wait for older
                // access units to leave it.
                return Ok(());
            }
            if self.cbr.is_some() {
                match self.cbr_fits(lead, packets) {
                    None => return Ok(()),
                    Some(true) => {}
                    Some(false) => {
                        let u = self.queue.pop().expect("head exists");
                        self.overflow(u, packets)?;
                        continue;
                    }
                }
            }
            let u = self.queue.pop().expect("head exists");
            self.bn.add(pts, bytes);
            self.emit_unit(u)?;
        }
    }

    /// Write a unit's PES packets into the output queue.
    fn emit_unit(&mut self, u: Queued) -> Result<(), InjectError> {
        if self.merge.is_none() && self.klv_writer.is_none() {
            self.dropped(DropReason::Unplaced, &u);
            return Ok(());
        }
        let unit = match u.pts {
            Some(pts) => PesUnit::new(&u.data).with_pts(pts),
            None => PesUnit::new(&u.data),
        };
        let value = u.pts.unwrap_or(0);
        let (q, writer) = match (&mut self.merge, &mut self.klv_writer) {
            (Some(m), _) => {
                if let Some(seq) = m.seq {
                    m.writer.set_sequence_number(seq);
                }
                (&mut m.fifo, &mut m.writer)
            }
            (None, Some(w)) => (&mut self.klv_q, w),
            _ => return Ok(()),
        };
        let start = q.len();
        writer.write(&unit, &mut |pk: &Packet| {
            q.push_back(Out {
                p: *pk,
                flags: 0,
                value,
            })
        })?;
        if q.len() > start {
            q[start].flags |= O_UNIT_FIRST;
            if let Some(last) = q.back_mut() {
                last.flags |= O_UNIT_LAST;
            }
        }
        if let Some(m) = &mut self.merge {
            m.seq = Some(m.writer.sequence_number());
        }
        if let Some(pts) = u.pts {
            self.queue.last_sent = Some(pts);
        }
        Ok(())
    }

    // ------------------------------------------------------------ output

    fn write_out<S: PacketSink + ?Sized>(
        &mut self,
        mut o: Out,
        sink: &mut S,
        kind: Kind,
    ) -> Result<(), InjectError> {
        match kind {
            Kind::Psi => {
                self.tb_sys.add(&self.clock);
                self.stats.psi_packets += 1;
                let cc = Some(o.p[3] & 0x0F);
                if u16::from_be_bytes([o.p[1] & 0x1F, o.p[2]]) == PAT_PID {
                    self.pat_last_cc = cc;
                } else {
                    self.pmt_last_cc = cc;
                }
                if o.flags & O_EXTRA != 0 {
                    self.stats.psi_extra_packets += 1;
                }
            }
            Kind::Klv => self.stats.klv_packets += 1,
            Kind::Merged => {
                let input = o.flags & O_INPUT != 0;
                if let Some(m) = &mut self.merge {
                    m.stamp(&mut o.p, input);
                }
                if !input {
                    self.stats.klv_packets += 1;
                }
            }
        }
        if kind == Kind::Psi && o.flags & (O_PAT_END | O_PMT_END) != 0 {
            let pos = self.clock.pos;
            if o.flags & O_PAT_END != 0 {
                self.last_pat = Some(pos);
                self.pat_copy_queued = false;
            } else {
                self.last_pmt = Some(pos);
                self.pmt_copy_queued = false;
                if !self.gate.listed {
                    // R-ISO13818-088 and D-ISO13818-13: the section leaves
                    // B_sys at no less than 80 kbit/s (2700 ticks a byte),
                    // then 4 ms more.
                    self.gate.listed = true;
                    self.gate.open_at = self.clock.timed.then_some(pos + o.value * 2700 + 4 * MS);
                }
            }
        }
        if kind != Kind::Psi && o.flags & O_UNIT_LAST != 0 && self.cbr.is_some() {
            if let (Some(now), true) = (self.clock.base(), self.cfg.carriage == KlvCarriage::Sync) {
                let lead = pts_delta(o.value, now) * 300;
                if lead < self.min_lead {
                    match self.cbr {
                        Some(OnOverflow::DropOldest) => {
                            self.stats.late_units += 1;
                            self.event(InjectEvent::Warning(InjectWarning::LateUnit {
                                pts: o.value,
                            }));
                        }
                        _ => {
                            return Err(InjectError::CbrOverflow {
                                pts: Some(o.value),
                                packets: 1,
                                free_per_second: (self.slots.rate * PCR_HZ as f64) as u64,
                            })
                        }
                    }
                }
            }
        }
        if kind != Kind::Psi && o.flags & O_UNIT_LAST != 0 {
            self.stats.units_inserted += 1;
        }
        sink.packet(&o.p);
        self.stats.packets_out += 1;
        Ok(())
    }

    fn write_input<S: PacketSink + ?Sized>(&mut self, p: &Packet, sink: &mut S) {
        sink.packet(p);
        self.stats.packets_out += 1;
    }

    /// Write what goes out for one input packet.
    fn write_slot<S: PacketSink + ?Sized>(
        &mut self,
        slot: Slot,
        p: &Packet,
        sink: &mut S,
    ) -> Result<(), InjectError> {
        if self.cbr.is_none() {
            // VBR: everything due goes before the input packet; PSI as
            // TB_sys allows (D-ISO13818-21).
            // An input that sends PSI faster than TB_sys drains overflows
            // it anyway; the queue never grows past a bound.
            while !self.psi_q.is_empty()
                && (self.tb_sys.room(&self.clock) || self.psi_q.len() > MAX_PSI_QUEUE)
            {
                let o = self.psi_q.pop_front().expect("not empty");
                self.inserted_in_interval += 1;
                self.write_out(o, sink, Kind::Psi)?;
            }
            while let Some(o) = self.merge.as_mut().and_then(|m| m.fifo.pop_front()) {
                self.inserted_in_interval += 1;
                self.write_out(o, sink, Kind::Merged)?;
            }
            while let Some(o) = self.klv_q.pop_front() {
                self.inserted_in_interval += 1;
                self.write_out(o, sink, Kind::Klv)?;
            }
            match slot {
                Slot::Keep | Slot::Free { null: true } => self.write_input(p, sink),
                Slot::Free { null: false } => {}
                Slot::Replace(q) => self.write_input(&q, sink),
                Slot::Merged => self.merged_input(p, sink)?,
            }
            return Ok(());
        }
        // CBR: one packet out for every packet in. PSI and diverted input
        // packets that cannot find slots are not dropped: that is a failure
        // (never a silent switch to VBR).
        if self.psi_q.len() > MAX_PSI_QUEUE
            || self.merge.as_ref().is_some_and(|m| m.fifo.len() > MAX_FIFO)
        {
            return Err(InjectError::CbrOverflow {
                pts: None,
                packets: self.psi_q.len(),
                free_per_second: (self.slots.rate * PCR_HZ as f64) as u64,
            });
        }
        match slot {
            Slot::Keep => self.write_input(p, sink),
            Slot::Replace(q) => self.write_input(&q, sink),
            Slot::Merged if self.merge.as_ref().map_or(true, |m| m.fifo.is_empty()) => {
                self.merged_input(p, sink)?
            }
            Slot::Merged => {
                // Our PES is still going out on this PID: the input packet
                // waits behind it and its slot becomes free.
                let mut q = *p;
                if let Some(m) = &mut self.merge {
                    m.take_input(&mut q);
                    self.merge_au();
                }
                if let Some(m) = &mut self.merge {
                    m.fifo.push_back(Out {
                        p: q,
                        flags: O_INPUT,
                        value: 0,
                    });
                }
                self.fill(p, false, sink)?;
            }
            Slot::Free { null } => self.fill(p, null, sink)?,
        }
        Ok(())
    }

    /// A free CBR slot: PSI first (as TB_sys allows), then KLV, else a null
    /// packet (R-ISO13818-019, R-ST1402TIME-05).
    fn fill<S: PacketSink + ?Sized>(
        &mut self,
        p: &Packet,
        null: bool,
        sink: &mut S,
    ) -> Result<(), InjectError> {
        self.stats.free_slots += 1;
        if !self.psi_q.is_empty() && self.tb_sys.room(&self.clock) {
            let o = self.psi_q.pop_front().expect("not empty");
            self.stats.slots_filled += 1;
            return self.write_out(o, sink, Kind::Psi);
        }
        // The slots PSI leaves are the room KLV has (MUXING_PLAN 6.9).
        self.slots.in_interval += 1;
        if let Some(o) = self.merge.as_mut().and_then(|m| m.fifo.pop_front()) {
            self.stats.slots_filled += 1;
            return self.write_out(o, sink, Kind::Merged);
        }
        if let Some(o) = self.klv_q.pop_front() {
            self.stats.slots_filled += 1;
            return self.write_out(o, sink, Kind::Klv);
        }
        if null {
            self.write_input(p, sink);
        } else {
            self.write_input(&NULL_PACKET, sink);
        }
        Ok(())
    }

    /// An input packet of the merged PID written in place.
    fn merged_input<S: PacketSink + ?Sized>(
        &mut self,
        p: &Packet,
        sink: &mut S,
    ) -> Result<(), InjectError> {
        let mut q = *p;
        if let Some(m) = &mut self.merge {
            m.take_input(&mut q);
            m.stamp(&mut q, true);
        }
        self.merge_au();
        self.write_input(&q, sink);
        Ok(())
    }

    /// Account an input access unit of the merged stream in B_n.
    fn merge_au(&mut self) {
        let Some(m) = &mut self.merge else { return };
        let au = m.input_au.take();
        let warn = m.saw_ours && !m.warned;
        m.warned |= warn;
        let id = m.our_service;
        if warn {
            self.event(InjectEvent::Warning(InjectWarning::ServiceIdInInput {
                service_id: id,
            }));
        }
        if let (Some((pts, bytes)), true) = (au, self.clock.timed) {
            self.bn.add(pts, bytes);
        }
    }

    /// End of input: VBR sends what can still be placed; CBR has no slots
    /// left. The rest is dropped and counted.
    pub fn end<S: PacketSink + ?Sized>(&mut self, sink: &mut S) -> Result<(), InjectError> {
        if self.cbr.is_none() {
            while let Some(o) = self.psi_q.pop_front() {
                self.write_out(o, sink, Kind::Psi)?;
            }
            let can_send = self.plan.is_some()
                && self.gate_open()
                && self.merge.as_ref().map_or(true, |m| m.idle());
            while let Some(u) = self.queue.pop() {
                let ok = can_send
                    && u.epoch == self.epoch
                    && match (u.pts, self.clock.base()) {
                        (Some(pts), Some(now)) => {
                            let lead = pts_delta(pts, now) * 300;
                            lead >= self.min_lead && lead <= self.max_lead
                        }
                        _ => true,
                    };
                if ok {
                    self.emit_unit(u)?;
                } else {
                    self.dropped(DropReason::Unplaced, &u);
                }
            }
            while let Some(o) = self.merge.as_mut().and_then(|m| m.fifo.pop_front()) {
                self.write_out(o, sink, Kind::Merged)?;
            }
            while let Some(o) = self.klv_q.pop_front() {
                self.write_out(o, sink, Kind::Klv)?;
            }
        } else {
            while let Some(u) = self.queue.pop() {
                self.dropped(DropReason::Unplaced, &u);
            }
            let units = self
                .klv_q
                .iter()
                .filter(|o| o.flags & O_UNIT_LAST != 0)
                .count();
            self.stats.dropped_unplaced += units as u64;
            self.klv_q.clear();
            self.psi_q.clear();
            if let Some(m) = &mut self.merge {
                m.fifo.clear();
            }
        }
        Ok(())
    }
}
