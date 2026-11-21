//! Extracting packets, PES packets and sections from a feed.

use crate::codec::{Codec, StreamKind};
use crate::config::{Config, Overflow};
use crate::crc::crc32_mpeg2;
use crate::demuxer::{has_optional_header, read_timestamp, PACKET_SIZE};
use crate::psi::{SectionAssembler, MAX_SECTION_LENGTH};
use std::collections::{HashMap, VecDeque};

/// Which PIDs a subscription follows. Every selection except
/// [`Select::Pid`] follows the tables as they change, so a subscription to
/// KLV picks up a KLV stream that appears later in a live feed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Select {
    /// One PID, whatever it carries: PAT (0), CAT (1), a PMT, the NIT, DVB
    /// SI, an elementary stream, or anything else.
    Pid(u16),
    /// Every PID of one program: its PMT, its PCR PID and its elementary
    /// streams.
    Program(u16),
    /// Elementary streams with this format, in any program.
    Codec(Codec),
    /// Elementary streams of this kind, in any program.
    Kind(StreamKind),
    /// Tables: the PAT, the CAT, every PMT, the NIT, PIDs 0x0002-0x001F
    /// (TSDT and DVB/ARIB SI) and 0x1FFB (ATSC PSIP).
    Psi,
    /// Every PID.
    All,
}

/// What a subscription delivers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[repr(u32)]
#[non_exhaustive]
pub enum Unit {
    /// Each 188-byte TS packet exactly as received, including damaged,
    /// scrambled and duplicate packets. (For 192/204-byte input the extra
    /// framing bytes are not included.)
    Packets = 1,
    /// Each reassembled PES packet, with its PTS/DTS. Applies to PIDs that
    /// carry PES, unless the PID was chosen with [`Select::Pid`].
    Pes = 2,
    /// Each complete section, with a CRC check. Applies to PIDs that carry
    /// sections (tables, SCTE-35, private sections), unless the PID was
    /// chosen with [`Select::Pid`].
    Sections = 3,
}

/// A request for data, passed to [`Demuxer::subscribe`](crate::Demuxer::subscribe).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct Subscription {
    /// Which PIDs to follow.
    pub select: Select,
    /// What to deliver from them.
    pub unit: Unit,
    /// Start by replaying matching packets from the history (see
    /// [`Config::history_packets`]).
    pub replay: bool,
    /// For PES from metadata streams (stream_id 0xFC, e.g. sync KLV),
    /// remove the 5-byte metadata access unit cell headers (ISO/IEC
    /// 13818-1 2.12.4) so the payload is just the metadata, cells joined
    /// in order. A payload that is not valid cells is left untouched and
    /// marked damaged. Off by default: the payload is delivered as carried.
    pub strip_metadata_au: bool,
}

impl Subscription {
    /// Follow `select`, delivering `unit`, from now on.
    pub fn new(select: Select, unit: Unit) -> Self {
        Subscription { select, unit, replay: false, strip_metadata_au: false }
    }

    /// Also deliver matching data still held in the packet history.
    pub fn with_replay(mut self) -> Self {
        self.replay = true;
        self
    }

    /// Deliver metadata stream PES without their access unit cell headers
    /// (see [`strip_metadata_au`](Self::strip_metadata_au)).
    pub fn strip_metadata_au(mut self) -> Self {
        self.strip_metadata_au = true;
        self
    }
}

/// Identifies a subscription. Ids are never reused within a demuxer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct SubscriptionId(pub(crate) u32);

impl SubscriptionId {
    /// The numeric id (as used by the C ABI).
    pub fn get(self) -> u32 {
        self.0
    }
}

/// One unit of extracted data.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Output {
    /// The subscription that produced it.
    pub subscription: SubscriptionId,
    /// What `data` holds.
    pub unit: Unit,
    /// The PID it came from.
    pub pid: u16,
    /// Feed packet number (1-based) of the last packet that contributed.
    pub packet: u64,
    /// The 188-byte packet, the whole PES packet (header included), or the
    /// whole section (CRC included).
    pub data: Vec<u8>,
    /// For PES: where the elementary stream bytes start in `data`.
    pub payload_offset: usize,
    /// For PES: the stream_id.
    pub stream_id: Option<u8>,
    /// For PES: presentation timestamp (90 kHz).
    pub pts: Option<u64>,
    /// For PES: decoding timestamp (90 kHz).
    pub dts: Option<u64>,
    /// For sections: the table_id.
    pub table_id: Option<u8>,
    /// For sections with a CRC: whether it checks out.
    pub crc_ok: Option<bool>,
    /// Data is missing inside this unit (continuity gap, transport error,
    /// scrambling, or a PES shorter than it declared).
    pub damaged: bool,
    /// Cut short at [`Config::max_pes_bytes`] or the reassembly budget.
    pub truncated: bool,
}

impl Output {
    fn new(subscription: SubscriptionId, unit: Unit, pid: u16, packet: u64, data: Vec<u8>) -> Self {
        Output {
            subscription,
            unit,
            pid,
            packet,
            data,
            payload_offset: 0,
            stream_id: None,
            pts: None,
            dts: None,
            table_id: None,
            crc_ok: None,
            damaged: false,
            truncated: false,
        }
    }

    /// The useful bytes: for PES the elementary stream data after the PES
    /// header (for KLV, the KLV itself); otherwise all of `data`.
    pub fn payload(&self) -> &[u8] {
        &self.data[self.payload_offset.min(self.data.len())..]
    }
}

/// Counters for extracted data.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct OutputStats {
    /// Outputs queued since the demuxer was created.
    pub queued: u64,
    /// Outputs discarded because the queue was full.
    pub dropped: u64,
    /// Outputs cut short at a size limit.
    pub truncated: u64,
    /// Outputs with data missing inside them.
    pub damaged: u64,
    /// Outputs waiting to be collected.
    pub pending: usize,
    /// Bytes waiting to be collected.
    pub pending_bytes: usize,
    /// Bytes held by PES packets still being assembled.
    pub reassembly_bytes: usize,
    /// Active subscriptions.
    pub subscriptions: usize,
}

// ---------------------------------------------------------------- internals

/// The packet repeated the previous one (same CC, same payload).
pub(crate) const FLAG_DUPLICATE: u8 = 1;
/// Data was lost just before this packet on its PID.
pub(crate) const FLAG_RESET: u8 = 2;
/// Most subscriptions per demuxer.
pub(crate) const MAX_SUBSCRIPTIONS: usize = 256;
/// Output buffers kept for reuse, and the largest one worth keeping.
const POOL_MAX: usize = 256;
const POOL_MAX_CAPACITY: usize = 64 << 10;

pub(crate) struct Sub {
    pub id: SubscriptionId,
    pub spec: Subscription,
    /// The unit delivered from each PID, if followed.
    pub matched: Box<[Option<Unit>]>,
    asm: HashMap<u16, Assembly>,
}

enum Assembly {
    Pes(PesAsm),
    Sections(SectionAssembler),
}

#[derive(Default)]
struct PesAsm {
    buf: Vec<u8>,
    active: bool,
    last_packet: u64,
    damaged: bool,
    truncated: bool,
}

impl PesAsm {
    fn start(&mut self, packet: u64) {
        self.active = true;
        self.damaged = false;
        self.truncated = false;
        self.last_packet = packet;
    }

    fn append(&mut self, data: &[u8], packet: u64, budget: &mut Budget) {
        let room = budget
            .max_unit
            .saturating_sub(self.buf.len())
            .min(budget.max_total.saturating_sub(budget.used));
        let n = data.len().min(room);
        if n < data.len() {
            self.truncated = true;
        }
        self.buf.extend_from_slice(&data[..n]);
        budget.used += n;
        self.last_packet = packet;
    }

    /// Total length this PES declares, when bounded.
    fn declared_len(&self) -> Option<usize> {
        let b = &self.buf;
        if b.len() < 6 || b[..3] != [0, 0, 1] {
            return None;
        }
        let len = usize::from(u16::from_be_bytes([b[4], b[5]]));
        (len != 0).then_some(6 + len)
    }

    /// Hand the assembled PES to the queue.
    fn emit(&mut self, sub: (SubscriptionId, bool), pid: u16, queue: &mut Queue, budget: &mut Budget) {
        let (id, strip) = sub;
        let short = !self.truncated && self.declared_len().is_some_and(|n| self.buf.len() < n);
        budget.used -= self.buf.len();
        let data = std::mem::replace(&mut self.buf, queue.take_buf());
        self.active = false;
        let mut out = Output::new(id, Unit::Pes, pid, self.last_packet, data);
        out.damaged = self.damaged || short;
        out.truncated = self.truncated;
        describe_pes(&mut out);
        if strip && out.stream_id == Some(METADATA_STREAM_ID) {
            strip_metadata_au(&mut out);
        }
        queue.push(out);
    }

    /// Drop the PES in progress.
    fn discard(&mut self, budget: &mut Budget) {
        budget.used -= self.buf.len();
        self.buf.clear();
        self.active = false;
    }
}

/// stream_id of a metadata_stream, whose payload is metadata access unit
/// cells.
const METADATA_STREAM_ID: u8 = 0xFC;

/// Replace the access unit cells after the PES header with their data.
/// Each cell is metadata_service_id, sequence_number, a flags byte and a
/// 16-bit AU_cell_data_length, then that many bytes. Only a payload that
/// is entirely valid cells is changed; anything else is left exactly as
/// received and marked damaged, so bytes are never altered on a guess.
fn strip_metadata_au(out: &mut Output) {
    let cell_len = |d: &[u8], at: usize| usize::from(u16::from_be_bytes([d[at + 3], d[at + 4]]));
    let d = &mut out.data;
    let mut at = out.payload_offset;
    while at < d.len() {
        if d.len() - at < 5 || cell_len(d, at) > d.len() - at - 5 {
            out.damaged = true;
            return;
        }
        at += 5 + cell_len(d, at);
    }
    let (mut read, mut write) = (out.payload_offset, out.payload_offset);
    while read < d.len() {
        let len = cell_len(d, read);
        d.copy_within(read + 5..read + 5 + len, write);
        write += len;
        read += 5 + len;
    }
    d.truncate(write);
}

/// Fill in stream_id, timestamps and payload offset from the PES header.
fn describe_pes(out: &mut Output) {
    let d = &out.data;
    if d.len() < 6 || d[..3] != [0, 0, 1] {
        out.damaged = true;
        return;
    }
    let sid = d[3];
    out.stream_id = Some(sid);
    out.payload_offset = 6;
    if has_optional_header(sid) && d.len() >= 9 {
        let header_len = usize::from(d[8]);
        out.payload_offset = (9 + header_len).min(d.len());
        let flags = d[7] >> 6;
        if flags & 0b10 != 0 && header_len >= 5 && d.len() >= 14 {
            out.pts = Some(read_timestamp(&d[9..14]));
            if flags == 0b11 && header_len >= 10 && d.len() >= 19 {
                out.dts = Some(read_timestamp(&d[14..19]));
            }
        }
    }
}

struct Budget {
    used: usize,
    max_total: usize,
    max_unit: usize,
}

struct Queue {
    items: VecDeque<Output>,
    bytes: usize,
    max_items: usize,
    max_bytes: usize,
    overflow: Overflow,
    pool: Vec<Vec<u8>>,
    queued: u64,
    dropped: u64,
    truncated: u64,
    damaged: u64,
}

impl Queue {
    fn take_buf(&mut self) -> Vec<u8> {
        self.pool.pop().unwrap_or_default()
    }

    fn recycle(&mut self, mut buf: Vec<u8>) {
        if self.pool.len() < POOL_MAX && buf.capacity() <= POOL_MAX_CAPACITY {
            buf.clear();
            self.pool.push(buf);
        }
    }

    fn push(&mut self, out: Output) {
        self.damaged += u64::from(out.damaged);
        self.truncated += u64::from(out.truncated);
        let size = out.data.len();
        if size > self.max_bytes {
            self.dropped += 1;
            self.recycle(out.data);
            return;
        }
        while self.items.len() >= self.max_items || self.bytes + size > self.max_bytes {
            match self.overflow {
                Overflow::DropOldest => {
                    let Some(old) = self.items.pop_front() else { break };
                    self.bytes -= old.data.len();
                    self.dropped += 1;
                    self.recycle(old.data);
                }
                Overflow::DropNewest => {
                    self.dropped += 1;
                    self.recycle(out.data);
                    return;
                }
            }
        }
        self.bytes += size;
        self.queued += 1;
        self.items.push_back(out);
    }

    fn pop(&mut self) -> Option<Output> {
        let out = self.items.pop_front()?;
        self.bytes -= out.data.len();
        Some(out)
    }
}

struct HistEntry {
    packet: u64,
    flags: u8,
    data: [u8; PACKET_SIZE],
}

/// Subscriptions, their in-progress assemblies, the output queue and the
/// packet history.
pub(crate) struct Outputs {
    pub subs: Vec<Sub>,
    next_id: u32,
    queue: Queue,
    budget: Budget,
    history: VecDeque<HistEntry>,
    history_max: usize,
}

impl Outputs {
    pub fn new(config: &Config) -> Self {
        Outputs {
            subs: Vec::new(),
            next_id: 1,
            queue: Queue {
                items: VecDeque::new(),
                bytes: 0,
                max_items: config.queue_max_items,
                max_bytes: config.queue_max_bytes,
                overflow: config.overflow,
                pool: Vec::new(),
                queued: 0,
                dropped: 0,
                truncated: 0,
                damaged: 0,
            },
            budget: Budget {
                used: 0,
                max_total: config.max_reassembly_bytes,
                max_unit: config.max_pes_bytes,
            },
            history: VecDeque::new(),
            history_max: config.history_packets,
        }
    }

    pub fn add(&mut self, spec: Subscription, matched: Box<[Option<Unit>]>) -> SubscriptionId {
        let id = SubscriptionId(self.next_id);
        self.next_id = self.next_id.wrapping_add(1).max(1);
        self.subs.push(Sub { id, spec, matched, asm: HashMap::new() });
        if spec.replay {
            self.replay(self.subs.len() - 1);
        }
        id
    }

    pub fn remove(&mut self, id: SubscriptionId) -> bool {
        let Some(i) = self.subs.iter().position(|s| s.id == id) else { return false };
        let sub = self.subs.remove(i);
        for asm in sub.asm.into_values() {
            if let Assembly::Pes(mut pes) = asm {
                pes.discard(&mut self.budget);
            }
        }
        true
    }

    /// Change what subscription `si` takes from `pid`. A PES still being
    /// assembled is delivered as it stands: for an unbounded PES it is
    /// usually complete, its end just not yet signalled.
    pub fn set_match(&mut self, si: usize, pid: u16, unit: Option<Unit>) {
        let sub = &mut self.subs[si];
        if sub.matched[usize::from(pid)] == unit {
            return;
        }
        sub.matched[usize::from(pid)] = unit;
        if let Some(Assembly::Pes(mut pes)) = sub.asm.remove(&pid) {
            if pes.active {
                pes.emit((sub.id, sub.spec.strip_metadata_au), pid, &mut self.queue, &mut self.budget);
            }
        }
    }

    pub fn records_history(&self) -> bool {
        self.history_max > 0
    }

    pub fn record(&mut self, packet: u64, flags: u8, data: &[u8; PACKET_SIZE]) {
        if self.history.len() == self.history_max {
            self.history.pop_front();
        }
        self.history.push_back(HistEntry { packet, flags, data: *data });
    }

    pub fn history(&self) -> impl Iterator<Item = (u64, &[u8; PACKET_SIZE])> + '_ {
        self.history.iter().map(|h| (h.packet, &h.data))
    }

    #[cfg(feature = "capi")]
    pub fn history_len(&self) -> usize {
        self.history.len()
    }

    #[cfg(feature = "capi")]
    pub fn history_get(&self, i: usize) -> Option<(u64, &[u8; PACKET_SIZE])> {
        self.history.get(i).map(|h| (h.packet, &h.data))
    }

    /// Run the history through subscription `si`.
    fn replay(&mut self, si: usize) {
        for h in &self.history {
            let pid = (u16::from(h.data[1] & 0x1F) << 8) | u16::from(h.data[2]);
            if let Some(unit) = self.subs[si].matched[usize::from(pid)] {
                deliver_to(&mut self.subs[si], &mut self.queue, &mut self.budget, unit, pid, &h.data, h.packet, h.flags);
            }
        }
    }

    /// Give a packet to every subscription following its PID.
    pub fn deliver(&mut self, pid: u16, p: &[u8; PACKET_SIZE], packet: u64, flags: u8) {
        for sub in &mut self.subs {
            if let Some(unit) = sub.matched[usize::from(pid)] {
                deliver_to(sub, &mut self.queue, &mut self.budget, unit, pid, p, packet, flags);
            }
        }
    }

    /// End of input: emit PES packets still being assembled and drop
    /// incomplete sections.
    pub fn finish(&mut self) {
        for sub in &mut self.subs {
            // Deliver in arrival order, not hash order, so results never
            // depend on anything but the input.
            let mut open: Vec<(u64, u16)> = Vec::new();
            for (&pid, asm) in sub.asm.iter_mut() {
                match asm {
                    Assembly::Pes(pes) if pes.active => open.push((pes.last_packet, pid)),
                    Assembly::Pes(_) => {}
                    Assembly::Sections(s) => s.reset(),
                }
            }
            open.sort_unstable();
            for (_, pid) in open {
                if let Some(Assembly::Pes(pes)) = sub.asm.get_mut(&pid) {
                    pes.emit((sub.id, sub.spec.strip_metadata_au), pid, &mut self.queue, &mut self.budget);
                }
            }
        }
    }

    pub fn pop(&mut self) -> Option<Output> {
        self.queue.pop()
    }

    pub fn recycle(&mut self, out: Output) {
        self.queue.recycle(out.data);
    }

    pub fn stats(&self) -> OutputStats {
        OutputStats {
            queued: self.queue.queued,
            dropped: self.queue.dropped,
            truncated: self.queue.truncated,
            damaged: self.queue.damaged,
            pending: self.queue.items.len(),
            pending_bytes: self.queue.bytes,
            reassembly_bytes: self.budget.used,
            subscriptions: self.subs.len(),
        }
    }
}

/// The parts of a packet that delivery needs.
struct View<'a> {
    tei: bool,
    scrambled: bool,
    pusi: bool,
    payload: Option<&'a [u8]>,
}

fn view(p: &[u8; PACKET_SIZE]) -> View<'_> {
    let afc = (p[3] >> 4) & 0x3;
    let start = if afc & 2 != 0 { 5 + usize::from(p[4]) } else { 4 };
    View {
        tei: p[1] & 0x80 != 0,
        scrambled: p[3] & 0xC0 != 0,
        pusi: p[1] & 0x40 != 0,
        payload: (afc & 1 != 0 && start < PACKET_SIZE).then(|| &p[start..]),
    }
}

#[allow(clippy::too_many_arguments)]
fn deliver_to(
    sub: &mut Sub,
    queue: &mut Queue,
    budget: &mut Budget,
    unit: Unit,
    pid: u16,
    p: &[u8; PACKET_SIZE],
    packet: u64,
    flags: u8,
) {
    let id = sub.id;
    let pes_sub = (id, sub.spec.strip_metadata_au);
    match unit {
        Unit::Packets => {
            let mut data = queue.take_buf();
            data.extend_from_slice(p);
            queue.push(Output::new(id, Unit::Packets, pid, packet, data));
        }
        Unit::Pes => {
            let asm = sub.asm.entry(pid).or_insert_with(|| Assembly::Pes(PesAsm::default()));
            if let Assembly::Sections(_) = asm {
                *asm = Assembly::Pes(PesAsm::default());
            }
            let Assembly::Pes(pes) = asm else { return };
            let v = view(p);
            if v.tei {
                pes.damaged |= pes.active;
                return;
            }
            if flags & FLAG_DUPLICATE != 0 {
                return;
            }
            let Some(payload) = v.payload else { return };
            if v.scrambled {
                pes.damaged |= pes.active;
                return;
            }
            if flags & FLAG_RESET != 0 {
                pes.damaged |= pes.active;
            }
            if v.pusi {
                if pes.active {
                    pes.emit(pes_sub, pid, queue, budget);
                }
                pes.start(packet);
            } else if !pes.active {
                return; // Joined mid-PES: wait for the next start.
            }
            pes.append(payload, packet, budget);
            if let Some(len) = pes.declared_len() {
                if pes.buf.len() >= len {
                    budget.used -= pes.buf.len() - len;
                    pes.buf.truncate(len);
                    pes.emit(pes_sub, pid, queue, budget);
                }
            }
        }
        Unit::Sections => {
            let asm = sub
                .asm
                .entry(pid)
                .or_insert_with(|| Assembly::Sections(SectionAssembler::with_max_len(MAX_SECTION_LENGTH)));
            if let Assembly::Pes(pes) = asm {
                pes.discard(budget);
                *asm = Assembly::Sections(SectionAssembler::with_max_len(MAX_SECTION_LENGTH));
            }
            let Assembly::Sections(sections) = asm else { return };
            let v = view(p);
            if v.tei || v.scrambled {
                sections.reset();
                return;
            }
            if flags & FLAG_DUPLICATE != 0 {
                return;
            }
            let Some(payload) = v.payload else { return };
            if flags & FLAG_RESET != 0 {
                sections.reset();
            }
            sections.feed(payload, v.pusi, |s| {
                let mut data = queue.take_buf();
                data.extend_from_slice(s);
                let mut out = Output::new(id, Unit::Sections, pid, packet, data);
                out.table_id = s.first().copied();
                out.crc_ok = (s.len() >= 7 && s[1] & 0x80 != 0).then(|| crc32_mpeg2(s) == 0);
                queue.push(out);
            });
        }
    }
}
