//! Pass-through KLV injection into an existing transport stream.
//!
//! [`Injector`] copies an input transport stream through and inserts KLV
//! metadata into one of its programs, as synchronous (stream_type 0x15,
//! metadata access unit cells with a PTS) or asynchronous (stream_type
//! 0x06, SMPTE RP 217) carriage of MISB ST 1402.2. What it changes:
//!
//! - **PID 0**: the input's own PAT sections are sent again, byte for byte,
//!   with the injector's continuity counter.
//! - **The target program's PMT PID**: the target PMT is rewritten with the
//!   KLV stream entry, its version_number one above the input's and a new
//!   CRC (D-ISO13818-12); other sections on that PID are copied.
//! - **PSI cadence**: besides every input occurrence, copies are added so
//!   that neither the PAT nor the target PMT is ever more than
//!   [`InjectConfig::psi_interval_ms`] apart (DECISIONS X-3).
//! - **The KLV PID**: packets of the new stream; with
//!   [`ExistingKlv::Keep`] and an existing synchronous stream, the injector
//!   joins that stream as a new metadata service and renumbers its
//!   continuity counters and cell sequence numbers (st1402-time.md D-5);
//!   with [`ExistingKlv::Replace`] the input's KLV is removed.
//!
//! - **The video PID**, only with [`VideoSei::Write`] and a write from the
//!   frame callback: the access unit's SEI user data is inserted or
//!   rewritten and the PES re-packetized (see [`Units::write_user_data`]).
//!
//! Every other packet goes out unchanged, in order. In [`RateMode::Vbr`]
//! inserted packets are added before the input packet they precede; in
//! [`RateMode::Cbr`] they replace null packets, so the packet count and the
//! position of every PCR stay exactly as in the input (D-ISO13818-15).
//!
//! Synchronous units are queued by PTS and sent within the T-STD window of
//! ISO/IEC 13818-1 2.4.2.6 and MISB ST 1402-12: at most
//! [`max_lead_ms`](InjectConfig::max_lead_ms) before their PTS and at least
//! [`min_lead_ms`](InjectConfig::min_lead_ms) before it, by default right
//! before the video frame they describe (D-ISO13818-14, st1402-time.md
//! D-7). The program clock of every packet comes from the input's PCRs
//! ([`PcrTimeline`]), interpolated (the default) or, for live input that
//! must not wait, extrapolated ([`InjectConfig::clock`]).
//!
//! With [`VideoSei::Scan`] or [`VideoSei::Write`] the frame callback also
//! sees the user data messages (SEI user_data_unregistered, MPEG-2
//! user_data) of each access unit, for example MISB ST 0604 timestamps,
//! and with `Write` it can write one per access unit
//! ([`Units::write_user_data`]). The injector stays MISB-agnostic: it
//! reads and writes messages, the caller decides what is in them.
//!
//! The input and output can also be 192-byte M2TS records
//! ([`RecordFormat::M2ts`]).
//!
//! ```
//! use mpegts::mux::{Frame, InjectConfig, Injector, InsertUnit, KlvCarriage, Units};
//!
//! # fn input() -> Vec<u8> { Vec::new() }
//! # fn st0601_for(pts: u64) -> Vec<u8> { vec![0x06, 0x0E, 0x2B, 0x34] }
//! let mut injector = Injector::new(InjectConfig::default().with_carriage(KlvCarriage::Sync))?;
//! // One KLV packet per video frame, made when the frame passes through and
//! // sent right before it.
//! let mut on_frame = |frame: &Frame, units: &mut Units<'_>| {
//!     let _ = units.insert(InsertUnit::sync(frame.pts, st0601_for(frame.pts)));
//! };
//! let mut out = Vec::new();
//! injector.push_with(&input(), &mut out, &mut on_frame)?;
//! # if false {
//! let stats = injector.finish_with(&mut out, &mut on_frame)?;
//! println!("{} KLV units inserted", stats.units_inserted);
//! # }
//! # Ok::<(), mpegts::mux::InjectError>(())
//! ```

mod core;
mod framer;
mod m2ts;
mod plan;
mod reorder;
mod tables;
mod video;

use self::core::{Core, UnitQueue};
use self::framer::Framer;
use self::tables::{Applied, Pids, Sections, Tables};
use self::video::AuContext;
use super::{MuxError, Packet, PacketSink, Record, SyncKlvOptions, M2TS_RECORD_SIZE};
use crate::nal::{UserData, VideoCodec};
use crate::{ClockMode, PcrTimeline, PACKET_SIZE, PTS_WRAP};
use std::collections::VecDeque;
use std::fmt;

pub use self::reorder::FrameReorder;

/// Which program receives the KLV stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[non_exhaustive]
pub enum ProgramSelect {
    /// The first program (in PAT order) with a video stream, else the first
    /// program. Decided once every program's PMT has arrived, or when
    /// [`InjectConfig::max_held_packets`] is reached with at least one.
    #[default]
    Auto,
    /// The program with this program_number. Decided once every program's
    /// PMT has arrived (so the PID and service id chosen are free in the
    /// whole stream), or at the hold limit once this program's PMT has.
    Number(u16),
}

/// How the KLV is carried (MISB ST 1402.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[non_exhaustive]
pub enum KlvCarriage {
    /// Synchronous: stream_type 0x15, stream_id 0xFC, metadata access unit
    /// cells, a PTS on every access unit. Units need a PTS.
    #[default]
    Sync,
    /// Asynchronous (SMPTE RP 217): stream_type 0x06, stream_id 0xBD, no
    /// PTS. Units have no PTS and go out at the next opportunity.
    Async,
}

/// What to do when the target program already carries KLV.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[non_exhaustive]
pub enum ExistingKlv {
    /// Keep it. Synchronous injection into a program that has a synchronous
    /// KLV stream joins that stream as a new metadata service (ST 1402-13,
    /// st1402-time.md D-5); otherwise the new stream is added on its own
    /// PID.
    #[default]
    Keep,
    /// Remove the program's KLV streams from its PMT and drop their packets
    /// (a PCR they carry is kept in an adaptation-field-only packet).
    Replace,
    /// Refuse with [`InjectError::ExistingKlv`].
    Fail,
}

/// What [`RateMode::Cbr`] does when queued KLV cannot be placed in time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[non_exhaustive]
pub enum OnOverflow {
    /// Fail with [`InjectError::CbrOverflow`].
    #[default]
    Fail,
    /// Drop units not yet started, oldest first, and count them.
    DropOldest,
}

/// Whether the output keeps the input's packet rate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[non_exhaustive]
pub enum RateMode {
    /// Inserted packets are added before the input packet they precede;
    /// packets the injector consumes (PSI it rewrites, KLV it removes)
    /// vanish. The rate of the PCR interval rises, which is conformant for
    /// a single program (D-ISO13818-15); with several programs on
    /// independent time bases a warning is raised (R-ISO13818-044).
    #[default]
    Vbr,
    /// The output has exactly as many packets as the input. Inserted
    /// packets go into free slots: null packets and packets the injector
    /// consumes, PSI first; a free slot with nothing to fill stays a null
    /// packet. No packet moves, so no PCR moves (R-ISO13818-040,
    /// R-ISO13818-172). Never falls back to VBR.
    Cbr {
        /// When there is not enough room in time.
        on_overflow: OnOverflow,
    },
}

/// Whether the injector reads and writes user data in the target's video
/// (MUXING_PLAN 6.8, st1402-time.md D-12 to D-14).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[non_exhaustive]
pub enum VideoSei {
    /// Video bytes are never parsed or changed.
    #[default]
    Off,
    /// Read the user data messages in the prefix of every access unit
    /// (everything before its first VCL NAL unit, or first slice in MPEG-2)
    /// of H.264, H.265 and MPEG-2 video, for the frame callback
    /// ([`Units::user_data`]). The output is unchanged; the frame callback
    /// runs once the prefix has been read, so the packets from the frame's
    /// first packet up to then are held (usually none: the prefix is in
    /// the first packet).
    Scan,
    /// Scan, and let the frame callback write one user data message per
    /// access unit ([`Units::write_user_data`]). H.264 (stream_type 0x1B)
    /// and H.265 (0x24) only: a target program with other video, or none,
    /// fails with [`InjectError::Unsupported`] when it is chosen. In
    /// [`RateMode::Cbr`] each video PES is held whole, with the packets
    /// between, so its free room is known before the callback runs.
    Write,
}

/// Packet format of the input and of the output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[non_exhaustive]
pub enum RecordFormat {
    /// 188-byte transport stream packets.
    #[default]
    Ts,
    /// 192-byte M2TS records (BDAV MPEG-2 transport stream, IEC 61883-4
    /// source packets): a 4-byte TP_extra_header (2-bit
    /// copy_permission_indicator, 30-bit arrival_time_stamp), then the
    /// packet. Input records keep their header. A packet the injector adds
    /// gets the copy_permission_indicator of the next input record and an
    /// arrival_time_stamp interpolated linearly (modulo 2^30) between the
    /// previous and next input records, evenly spaced; when it replaces an
    /// input packet (a rewritten PAT or PMT) the last of them lands on that
    /// record's stamp, and after the last input record the spacing of the
    /// last two is continued. In [`RateMode::Cbr`] every output record is
    /// an input record and keeps its header. The output goes to
    /// [`PacketSink::record`]; byte input is framed at 192 bytes
    /// ([`Injector::push`]), packet input comes through
    /// [`Injector::push_record`].
    M2ts,
}

/// Default for [`InjectConfig::video_sei_max_prefix`]: 64 KiB.
pub const DEFAULT_VIDEO_SEI_MAX_PREFIX: usize = 64 << 10;

/// Most payload bytes of one [`Units::write_user_data`]: 65,519 (with the
/// 16-byte uuid, 65,535).
pub const MAX_USER_DATA_PAYLOAD: usize = 65_535 - 16;

/// Settings of an [`Injector`]. Start from [`Default`] and change what you
/// need, directly or with the `with_*` setters.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct InjectConfig {
    /// The program to inject into.
    pub program: ProgramSelect,
    /// Synchronous or asynchronous KLV.
    pub carriage: KlvCarriage,
    /// metadata_service_id of the new service (synchronous only). `None`:
    /// the lowest id no metadata service in the transport stream uses, 0
    /// when free (D-ISO13818-03); taking over a replaced stream's PID
    /// reuses its id.
    pub service_id: Option<u8>,
    /// PID of the new stream. `None`: a replaced KLV stream's PID when it
    /// may be reused, else the first free PID above the program's highest
    /// elementary PID (D-ISO13818-11). A fixed PID must be free.
    pub pid: Option<u16>,
    /// What to do with KLV the program already carries.
    pub existing_klv: ExistingKlv,
    /// VBR or strict CBR output.
    pub rate: RateMode,
    /// Longest gap between PAT copies and between target PMT copies, in
    /// milliseconds of program clock (DECISIONS X-3). 1 to 249; default
    /// 100 (MISB ST 1402.2 asks for more than 4 per second,
    /// R-ST1402TIME-03).
    pub psi_interval_ms: u32,
    /// Earliest arrival of a synchronous unit before its PTS, in
    /// milliseconds. At most 1000 (R-ISO13818-157); default 990, leaving
    /// 10 ms of margin (D-ISO13818-14).
    pub max_lead_ms: u32,
    /// Latest arrival of a synchronous unit's last byte before its PTS, in
    /// milliseconds; at least 1. Default 1 (D-ISO13818-14). A unit that can
    /// no longer make it is dropped and counted.
    pub min_lead_ms: u32,
    /// Send a synchronous unit right before the video frame it describes
    /// (the first video PES with a PTS at least its own), within the
    /// window above (st1402-time.md D-7). Default true. Without video, or
    /// with this off, units go out as soon as the window opens.
    pub align_to_frames: bool,
    /// With `align_to_frames`, send a unit whose frame has not arrived once
    /// it is this close to its PTS, in milliseconds. Default 50.
    pub fallback_lead_ms: u32,
    /// Most units waiting to be sent. Default 4096.
    pub max_queued_units: usize,
    /// Most bytes of units waiting to be sent. Default 8 MiB.
    pub max_queued_bytes: usize,
    /// Most input packets held while waiting for the target program's PMT,
    /// for [`Injector::push`] for the next PCR, and with
    /// [`VideoSei::Scan`] or [`VideoSei::Write`] for a video access unit's
    /// prefix (or, in CBR with `Write`, its whole PES); each of these holds
    /// is bounded by it. Default 50,000 (about 9.4 MB).
    pub max_held_packets: usize,
    /// metadata_STD_descriptor values and the optional registration
    /// descriptor of a synchronous stream. Its `service_id` is not used;
    /// see [`service_id`](Self::service_id).
    pub sync: SyncKlvOptions,
    /// Read, or read and write, user data in the target's video. Default
    /// [`VideoSei::Off`].
    pub video_sei: VideoSei,
    /// With [`VideoSei::Scan`] or [`VideoSei::Write`], most elementary
    /// stream bytes of an access unit read while looking for its first VCL
    /// NAL unit. Past it the frame callback runs with what was found and
    /// writes are refused and counted. Default
    /// [`DEFAULT_VIDEO_SEI_MAX_PREFIX`] (64 KiB); at least 16.
    pub video_sei_max_prefix: usize,
    /// How [`Injector::push`] times packets: [`ClockMode::Interpolate`]
    /// (default) holds each PCR interval until its closing PCR;
    /// [`ClockMode::Extrapolate`] holds nothing, so with [`VideoSei::Off`]
    /// every packet pushed (once the target is known, and while the input
    /// stays in sync) is written before `push` returns: the mode for live
    /// injection. Extrapolation uses the rate of the previous PCR interval
    /// (10 Mbit/s until two PCRs have been seen), is corrected at each PCR
    /// and never goes backwards within a time base. Packets pushed with
    /// their own clock ([`Injector::push_packet`]) are not affected.
    pub clock: ClockMode,
    /// 188-byte packets or 192-byte M2TS records, in and out. Default
    /// [`RecordFormat::Ts`].
    pub record: RecordFormat,
}

impl Default for InjectConfig {
    fn default() -> Self {
        InjectConfig {
            program: ProgramSelect::Auto,
            carriage: KlvCarriage::Sync,
            service_id: None,
            pid: None,
            existing_klv: ExistingKlv::Keep,
            rate: RateMode::Vbr,
            psi_interval_ms: 100,
            max_lead_ms: 990,
            min_lead_ms: 1,
            align_to_frames: true,
            fallback_lead_ms: 50,
            max_queued_units: 4096,
            max_queued_bytes: 8 << 20,
            max_held_packets: 50_000,
            sync: SyncKlvOptions::default(),
            video_sei: VideoSei::Off,
            video_sei_max_prefix: DEFAULT_VIDEO_SEI_MAX_PREFIX,
            clock: ClockMode::Interpolate,
            record: RecordFormat::Ts,
        }
    }
}

impl InjectConfig {
    /// The defaults.
    pub fn new() -> Self {
        Self::default()
    }

    /// With this program selection.
    pub fn with_program(mut self, program: ProgramSelect) -> Self {
        self.program = program;
        self
    }

    /// With this carriage.
    pub fn with_carriage(mut self, carriage: KlvCarriage) -> Self {
        self.carriage = carriage;
        self
    }

    /// With a fixed metadata_service_id.
    pub fn with_service_id(mut self, id: u8) -> Self {
        self.service_id = Some(id);
        self
    }

    /// With a fixed PID.
    pub fn with_pid(mut self, pid: u16) -> Self {
        self.pid = Some(pid);
        self
    }

    /// With this policy for existing KLV.
    pub fn with_existing_klv(mut self, existing: ExistingKlv) -> Self {
        self.existing_klv = existing;
        self
    }

    /// With this rate mode.
    pub fn with_rate(mut self, rate: RateMode) -> Self {
        self.rate = rate;
        self
    }

    /// With this PSI interval.
    pub fn with_psi_interval_ms(mut self, ms: u32) -> Self {
        self.psi_interval_ms = ms;
        self
    }

    /// With this insertion window.
    pub fn with_lead_ms(mut self, min: u32, max: u32) -> Self {
        self.min_lead_ms = min;
        self.max_lead_ms = max;
        self
    }

    /// With frame alignment on or off.
    pub fn with_align_to_frames(mut self, align: bool) -> Self {
        self.align_to_frames = align;
        self
    }

    /// With this fallback for frame alignment.
    pub fn with_fallback_lead_ms(mut self, ms: u32) -> Self {
        self.fallback_lead_ms = ms;
        self
    }

    /// With these queue limits.
    pub fn with_queue_limits(mut self, units: usize, bytes: usize) -> Self {
        self.max_queued_units = units;
        self.max_queued_bytes = bytes;
        self
    }

    /// With this hold limit.
    pub fn with_max_held_packets(mut self, packets: usize) -> Self {
        self.max_held_packets = packets;
        self
    }

    /// With these synchronous stream options.
    pub fn with_sync_options(mut self, sync: SyncKlvOptions) -> Self {
        self.sync = sync;
        self
    }

    /// With this video user data mode.
    pub fn with_video_sei(mut self, mode: VideoSei) -> Self {
        self.video_sei = mode;
        self
    }

    /// With this limit on the access unit prefix read.
    pub fn with_video_sei_max_prefix(mut self, bytes: usize) -> Self {
        self.video_sei_max_prefix = bytes;
        self
    }

    /// With this clock for [`Injector::push`].
    pub fn with_clock(mut self, clock: ClockMode) -> Self {
        self.clock = clock;
        self
    }

    /// With this packet format.
    pub fn with_record_format(mut self, record: RecordFormat) -> Self {
        self.record = record;
        self
    }

    /// Bytes per input and output unit: 188 or 192.
    fn stride(&self) -> usize {
        match self.record {
            RecordFormat::M2ts => M2TS_RECORD_SIZE,
            _ => PACKET_SIZE,
        }
    }

    fn validate(&self) -> Result<(), InjectError> {
        let bad = |why| Err(InjectError::InvalidConfig(why));
        if !(1..250).contains(&self.psi_interval_ms) {
            return bad("psi_interval_ms must be 1 to 249 (ST 1402.2 asks for more than 4 PSI repetitions per second)");
        }
        if self.max_lead_ms > 1000 {
            return bad("max_lead_ms must be at most 1000 (ISO/IEC 13818-1 2.4.2.6)");
        }
        if self.min_lead_ms == 0 || self.min_lead_ms >= self.max_lead_ms {
            return bad("min_lead_ms must be at least 1 and below max_lead_ms");
        }
        if !(self.min_lead_ms..=self.max_lead_ms).contains(&self.fallback_lead_ms) {
            return bad("fallback_lead_ms must lie between min_lead_ms and max_lead_ms");
        }
        if self.max_queued_units == 0 || self.max_queued_bytes == 0 || self.max_held_packets == 0 {
            return bad("queue and hold limits must be at least 1");
        }
        if self.video_sei_max_prefix < 16 {
            return bad("video_sei_max_prefix must be at least 16");
        }
        if let Some(pid) = self.pid {
            if !(0x0010..crate::NULL_PID).contains(&pid) {
                return bad("pid must be 0x0010 to 0x1FFE (ISO/IEC 13818-1 Table 2-3)");
            }
        }
        if let ProgramSelect::Number(0) = self.program {
            return bad("program_number 0 is the network PID, not a program");
        }
        match self.carriage {
            KlvCarriage::Async if self.service_id.is_some() => {
                bad("service_id applies to synchronous carriage only")
            }
            KlvCarriage::Sync => {
                super::metadata_std_descriptor(
                    self.sync.input_leak_rate,
                    self.sync.buffer_size,
                    self.sync.output_leak_rate,
                )
                .map_err(|_| {
                    InjectError::InvalidConfig("metadata_STD_descriptor fields must fit in 22 bits")
                })?;
                if self.sync.buffer_size == 0 {
                    return bad("sync.buffer_size must be at least 1 (1024 bytes)");
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }
}

/// A video frame of the target program, as its first PES packet is about
/// to be written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct Frame {
    /// The video PID.
    pub pid: u16,
    /// Presentation timestamp, 90 kHz.
    pub pts: u64,
    /// Decoding timestamp, 90 kHz, when the PES has one.
    pub dts: Option<u64>,
    /// Program clock (27 MHz) at the frame's first packet, once a PCR has
    /// been seen.
    pub clock: Option<u64>,
    /// random_access_indicator of the frame's first packet.
    pub random_access: bool,
    /// The first frame after a time base discontinuity: PTS values before
    /// it belong to the old time base (R-ISO13818-031).
    pub discontinuity: bool,
}

impl Frame {
    /// A frame with only a PID and a PTS.
    pub fn new(pid: u16, pts: u64) -> Self {
        Frame {
            pid,
            pts,
            dts: None,
            clock: None,
            random_access: false,
            discontinuity: false,
        }
    }

    /// With a DTS.
    pub fn with_dts(mut self, dts: u64) -> Self {
        self.dts = Some(dts);
        self
    }
}

/// One KLV unit to insert: a whole KLV packet (an ST 0601 local set, for
/// example), with a PTS for synchronous carriage and none for asynchronous.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct InsertUnit {
    /// Presentation timestamp, 90 kHz (taken modulo 2^33).
    pub pts: Option<u64>,
    /// The KLV bytes.
    pub data: Vec<u8>,
}

impl InsertUnit {
    /// A synchronous unit.
    pub fn sync(pts: u64, data: Vec<u8>) -> Self {
        InsertUnit {
            pts: Some(pts % PTS_WRAP),
            data,
        }
    }

    /// An asynchronous unit.
    pub fn asynchronous(data: Vec<u8>) -> Self {
        InsertUnit { pts: None, data }
    }
}

/// Inserts units from inside a frame callback (see [`Injector::push_with`]).
/// Units inserted here go out before the frame's first packet when their
/// window allows. With [`VideoSei::Scan`] or [`VideoSei::Write`] it also
/// gives the access unit's user data and, with `Write`, writes it.
#[derive(Debug)]
pub struct Units<'a> {
    queue: &'a mut UnitQueue,
    clock: Option<u64>,
    target: Option<&'a Target>,
    au: Option<&'a mut AuContext>,
}

/// What was read of an access unit's prefix (see [`Units::video_prefix`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct VideoPrefix {
    /// The video format.
    pub codec: VideoCodec,
    /// The prefix was read up to the first VCL NAL unit (first slice in
    /// MPEG-2), so a write can be placed. False when the PES did not start
    /// with a start code (nothing is reported then), ended first, or
    /// [`InjectConfig::video_sei_max_prefix`] or
    /// [`InjectConfig::max_held_packets`] was reached first.
    pub complete: bool,
    /// Elementary stream bytes of the prefix read: up to the first VCL NAL
    /// unit's start code when complete.
    pub bytes: usize,
}

impl Units<'_> {
    /// Queue a unit; see [`Injector::insert`].
    pub fn insert(&mut self, unit: InsertUnit) -> Result<(), InjectError> {
        self.queue.insert(unit)
    }

    /// The program clock (27 MHz) at the frame's first packet; see
    /// [`Injector::clock`].
    pub fn clock(&self) -> Option<u64> {
        self.clock
    }

    /// What the injector writes and where.
    pub fn target(&self) -> Option<&Target> {
        self.target
    }

    /// The configured carriage: whether units need a PTS.
    pub fn carriage(&self) -> KlvCarriage {
        self.queue.carriage
    }

    /// Units waiting to be sent.
    pub fn queued(&self) -> usize {
        self.queue.len()
    }

    /// With [`VideoSei::Scan`] or [`VideoSei::Write`], what was read of
    /// this access unit; `None` when nothing was (video sei off, a video
    /// format the injector does not read).
    pub fn video_prefix(&self) -> Option<VideoPrefix> {
        self.au.as_ref().map(|a| a.prefix())
    }

    /// The user data messages in this access unit's prefix, in stream
    /// order: SEI user_data_unregistered messages (H.264, H.265) and MPEG-2
    /// user_data, each with its 16-byte identifier and payload (emulation
    /// prevention removed). Positions are offsets in the PES payload (the
    /// elementary stream bytes of the frame's PES packet, from its first
    /// byte after the PES header). Empty without [`VideoSei::Scan`] or
    /// [`VideoSei::Write`]. At most 64 messages are reported.
    pub fn user_data(&self) -> &[UserData] {
        self.au.as_ref().map_or(&[], |a| a.user_data.as_slice())
    }

    /// Write one user_data_unregistered message into this access unit
    /// ([`VideoSei::Write`], H.264 or H.265):
    ///
    /// - **Rewrite**: when a message of [`user_data`](Self::user_data) has
    ///   its identifier in `replace` or equal to `uuid` (the first such
    ///   message), it becomes `uuid` + `payload` in place: same SEI NAL
    ///   unit, same position, the other messages of that NAL unit
    ///   unchanged, payloadSize coded again and emulation prevention run
    ///   again on the NAL unit (st1402-time.md D-12, R-ST1402TIME-81). Pass
    ///   the message's own identifier as `uuid` to keep it, as D-12 does.
    ///   A NAL unit that gets shorter is followed by zero bytes
    ///   (trailing_zero_8bits, ITU-T H.264 B.1, H.265 B.2), so the PES
    ///   keeps its size.
    /// - **Insert**: otherwise a new SEI NAL unit holding only this
    ///   message, with a 4-byte start code, goes immediately before the
    ///   first VCL NAL unit, after the access unit delimiter, parameter
    ///   sets and every existing SEI NAL unit (R-ST1402TIME-82, -83, -86,
    ///   -87). H.264: header 0x06 (nal_ref_idc 0, R-ST1402TIME-79), before
    ///   a prefix NAL unit (type 14) that directly precedes the first VCL
    ///   NAL unit. H.265: PREFIX_SEI_NUT, nuh_layer_id 0,
    ///   nuh_temporal_id_plus1 of the first VCL NAL unit (R-ST1402TIME-84,
    ///   -85).
    ///
    /// The PES is then re-packetized: the bytes go into the adaptation
    /// field stuffing of the PES's packets, the PES_packet_length is
    /// rewritten when it is not 0, PTS and DTS never change
    /// (R-ST1402TIME-09, -14), the video PID's continuity counters are
    /// renumbered, and every PCR stays on its packet (R-ST1402TIME-11). In
    /// [`RateMode::Vbr`] what stuffing cannot absorb goes into added video
    /// packets at the end of the PES; in [`RateMode::Cbr`] into null
    /// packets of the held PES, keeping the packet count. The write is
    /// applied when the callback returns; when it cannot be (no room in
    /// CBR with [`OnOverflow::DropOldest`], a PES_packet_length over 65535)
    /// the access unit goes out unchanged and
    /// [`InjectStats::user_data_skipped`] and an
    /// [`InjectWarning::UserDataSkipped`] event report it. With
    /// [`OnOverflow::Fail`] a lack of room is [`InjectError::CbrOverflow`].
    ///
    /// Refused with [`InjectError::UserData`]: without
    /// [`VideoSei::Write`], for video the injector does not write, for a
    /// second write in one access unit, a payload over
    /// [`MAX_USER_DATA_PAYLOAD`], and when the prefix was not read
    /// completely ([`VideoPrefix::complete`]; this one is also counted as
    /// skipped).
    ///
    /// ```
    /// use mpegts::mux::{Frame, InjectConfig, Injector, Units, VideoSei};
    ///
    /// // The MISB ST 0604 identifier for H.264 (st1402-time.md 3.8).
    /// const MISP: [u8; 16] = *b"MISPmicrosectime";
    /// let mut injector = Injector::new(InjectConfig::default().with_video_sei(VideoSei::Write))?;
    /// let mut on_frame = |_: &Frame, units: &mut Units<'_>| {
    ///     // Time Status and the modified time stamp; rewrites the access
    ///     // unit's own ST 0604 message when it has one.
    ///     let payload = [0x9F, 0x00, 0x05, 0xFF, 0xE3, 0x97, 0xFF, 0xF8, 0x7C, 0xFF, 0x11, 0x6A];
    ///     let _ = units.write_user_data(MISP, &payload, &[MISP]);
    /// };
    /// # let input: Vec<u8> = Vec::new();
    /// let mut out = Vec::new();
    /// injector.push_with(&input, &mut out, &mut on_frame)?;
    /// # Ok::<(), mpegts::mux::InjectError>(())
    /// ```
    pub fn write_user_data(&mut self, uuid: [u8; 16], payload: &[u8], replace: &[[u8; 16]]) -> Result<(), InjectError> {
        match &mut self.au {
            Some(au) => au.request_write(uuid, payload, replace),
            None => Err(InjectError::UserData(
                "no access unit prefix was read: needs VideoSei::Write and H.264 or H.265 video",
            )),
        }
    }
}

/// The program the injector writes into, and how.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Target {
    /// program_number.
    pub program: u16,
    /// PID of its PMT.
    pub pmt_pid: u16,
    /// Its PCR_PID, if it has a PCR.
    pub pcr_pid: Option<u16>,
    /// Its first video PID, the one frames are taken from.
    pub video_pid: Option<u16>,
    /// stream_type of that video stream.
    pub video_stream_type: Option<u8>,
    /// Its format, when it is one whose user data the injector reads
    /// ([`VideoSei`]).
    pub video_codec: Option<VideoCodec>,
    /// PID of the KLV stream written.
    pub klv_pid: u16,
    /// metadata_service_id of the service written (synchronous only).
    pub service_id: Option<u8>,
    /// The carriage.
    pub carriage: KlvCarriage,
    /// The service joins an existing synchronous KLV stream on `klv_pid`.
    pub merged: bool,
    /// Input KLV PIDs whose packets are removed ([`ExistingKlv::Replace`]).
    pub stripped: Vec<u16>,
}

/// Counters of an [`Injector`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub struct InjectStats {
    /// Input packets processed.
    pub packets_in: u64,
    /// Packets written.
    pub packets_out: u64,
    /// Input bytes skipped to find packet sync ([`Injector::push`]).
    pub bytes_skipped: u64,
    /// Times packet sync was lost.
    pub sync_losses: u64,
    /// Input packets held, waiting for the target's PMT or a PCR.
    pub held_packets: u64,
    /// Video frames of the target seen.
    pub frames: u64,
    /// Units written.
    pub units_inserted: u64,
    /// Units waiting.
    pub units_queued: u64,
    /// Bytes of units waiting.
    pub bytes_queued: u64,
    /// KLV packets written (the injector's own, not the input's).
    pub klv_packets: u64,
    /// PAT and PMT packets written.
    pub psi_packets: u64,
    /// Of those, packets of copies added for the PSI interval.
    pub psi_extra_packets: u64,
    /// Input KLV packets removed.
    pub stripped_packets: u64,
    /// Adaptation-field-only packets written to keep the PCR of a removed
    /// stream.
    pub pcr_only_packets: u64,
    /// Input null packets.
    pub null_packets: u64,
    /// Free slots (null and consumed packets) in [`RateMode::Cbr`].
    pub free_slots: u64,
    /// Free slots filled with the injector's packets.
    pub slots_filled: u64,
    /// Units dropped because they could no longer arrive before their PTS.
    pub dropped_late: u64,
    /// Units dropped for lack of room ([`OnOverflow::DropOldest`]).
    pub dropped_overflow: u64,
    /// Units dropped because a time base discontinuity separated them from
    /// their insertion point.
    pub dropped_discontinuity: u64,
    /// Units still queued at [`Injector::finish`] that could not be placed.
    pub dropped_unplaced: u64,
    /// Units whose last packet arrived later than `min_lead_ms` before
    /// their PTS in [`RateMode::Cbr`] with [`OnOverflow::DropOldest`].
    pub late_units: u64,
    /// PAT or PMT sections with a bad CRC, not sent on.
    pub psi_crc_errors: u64,
    /// Peak occupancy of the metadata buffer B_n of the KLV stream, in
    /// bytes, as the T-STD models it (R-ISO13818-160).
    pub metadata_buffer_peak: u64,
    /// Measured rate of input null packets, bit/s.
    pub null_bitrate: u64,
    /// Measured rate of free slots in [`RateMode::Cbr`], bit/s: the
    /// headroom.
    pub free_bitrate: u64,
    /// Rate the injector added, bit/s (packets written minus packets read).
    pub added_bitrate: i64,
    /// Events dropped because nobody popped them.
    pub events_dropped: u64,
    /// Video access units whose prefix was read ([`VideoSei::Scan`],
    /// [`VideoSei::Write`]).
    pub frames_scanned: u64,
    /// User data messages found in those prefixes.
    pub user_data_found: u64,
    /// SEI NAL units inserted ([`Units::write_user_data`]).
    pub user_data_inserted: u64,
    /// User data messages rewritten in place.
    pub user_data_rewritten: u64,
    /// Writes not applied because of the stream: the prefix not read
    /// completely, a PES_packet_length that would exceed 65535, no room in
    /// [`RateMode::Cbr`] with [`OnOverflow::DropOldest`].
    pub user_data_skipped: u64,
    /// Video packets added to carry written user data ([`RateMode::Vbr`]).
    pub video_packets_added: u64,
    /// Null packets that became video packets to carry written user data
    /// ([`RateMode::Cbr`]).
    pub video_slots_filled: u64,
}

/// Why units were dropped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum DropReason {
    /// Too late to arrive before its PTS.
    Late,
    /// No room ([`OnOverflow::DropOldest`]).
    Overflow,
    /// Across a time base discontinuity.
    Discontinuity,
    /// Still queued at the end.
    Unplaced,
}

/// Why a user data write was not applied (see
/// [`InjectWarning::UserDataSkipped`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum UserDataSkip {
    /// The access unit's prefix was not read up to its first VCL NAL unit
    /// ([`VideoPrefix::complete`] is false).
    Incomplete,
    /// The PES_packet_length would exceed 65535.
    PesTooLong,
    /// Not enough room in [`RateMode::Cbr`] ([`OnOverflow::DropOldest`]).
    NoRoom,
}

/// Things worth knowing that are not errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum InjectWarning {
    /// VBR insertion into a transport stream with several programs, whose
    /// independent time bases a variable rate breaks (R-ISO13818-044,
    /// D-ISO13818-15); prefer [`RateMode::Cbr`].
    MultiProgramVbr {
        /// Programs in the PAT.
        programs: usize,
    },
    /// The metadata_service_id moved because the input started using it.
    ServiceIdChanged {
        /// The old id.
        from: u8,
        /// The new id.
        to: u8,
    },
    /// The input uses the injector's metadata_service_id in cells of the
    /// merged stream.
    ServiceIdInInput {
        /// The id.
        service_id: u8,
    },
    /// A unit's last packet went out less than `min_lead_ms` before its PTS
    /// ([`RateMode::Cbr`] with [`OnOverflow::DropOldest`]).
    LateUnit {
        /// Its PTS.
        pts: u64,
    },
    /// A user data write of the frame callback was not applied; the access
    /// unit went out unchanged.
    UserDataSkipped {
        /// PTS of the access unit.
        pts: u64,
        /// Why.
        reason: UserDataSkip,
    },
}

/// Something that happened, for logging or monitoring.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum InjectEvent {
    /// The target program was chosen.
    TargetFound(Target),
    /// The target or how it is written changed (a PSI change).
    TargetChanged(Target),
    /// The target program left the PAT; nothing is inserted until it is
    /// back.
    TargetLost {
        /// Its program_number.
        program: u16,
    },
    /// A time base discontinuity on the target's PCR_PID.
    Discontinuity {
        /// The first PCR of the new time base.
        pcr: u64,
    },
    /// Units dropped.
    Dropped {
        /// Why.
        reason: DropReason,
        /// The PTS of the unit, if it had one.
        pts: Option<u64>,
        /// Its size.
        bytes: usize,
    },
    /// A warning.
    Warning(InjectWarning),
}

/// Why the injector refused something. Errors from processing input (all
/// but [`InvalidUnit`](Self::InvalidUnit), [`OutOfOrder`](Self::OutOfOrder),
/// [`QueueFull`](Self::QueueFull) and [`UserData`](Self::UserData)) are
/// final: every later call returns the same error.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum InjectError {
    /// An [`InjectConfig`] value out of range.
    InvalidConfig(&'static str),
    /// A unit that breaks the carriage's rules.
    InvalidUnit(&'static str),
    /// A synchronous unit with a PTS below the last one sent: metadata
    /// access units leave the buffer at their PTS in stream order
    /// (R-ISO13818-160).
    OutOfOrder {
        /// The unit's PTS.
        pts: u64,
        /// The last PTS sent.
        last: u64,
    },
    /// The unit queue is full; insert again later.
    QueueFull {
        /// Units queued.
        units: usize,
        /// Bytes queued.
        bytes: usize,
    },
    /// No target program was found within
    /// [`InjectConfig::max_held_packets`] packets, or by the end of input.
    NoTarget {
        /// Packets held.
        held: usize,
    },
    /// The target program is scrambled; its streams cannot be read
    /// (R-ISO13818-007).
    Scrambled {
        /// program_number.
        program: u16,
        /// The scrambled PID.
        pid: u16,
    },
    /// The target program already carries KLV ([`ExistingKlv::Fail`]).
    ExistingKlv {
        /// program_number.
        program: u16,
        /// The KLV PID.
        pid: u16,
    },
    /// The PID is not free.
    PidInUse {
        /// The PID.
        pid: u16,
        /// Who uses it.
        reason: &'static str,
    },
    /// No free PID for the KLV stream.
    NoFreePid,
    /// The metadata_service_id is used by another service.
    ServiceIdInUse(u8),
    /// All 256 metadata_service_ids are used.
    NoFreeServiceId,
    /// The rewritten PMT would exceed section_length 1021 (R-ISO13818-091).
    SectionTooLong {
        /// The section_length it would need.
        section_length: usize,
    },
    /// [`RateMode::Cbr`] with [`OnOverflow::Fail`] and not enough free
    /// slots to place a unit before its deadline.
    CbrOverflow {
        /// The unit's PTS.
        pts: Option<u64>,
        /// Packets it needed.
        packets: usize,
        /// Measured free slots per second.
        free_per_second: u64,
    },
    /// An input the injector does not handle.
    Unsupported(&'static str),
    /// A user data write refused ([`Units::write_user_data`]).
    UserData(&'static str),
    /// [`Injector::push`] and [`Injector::push_packet`] mixed on one
    /// injector, or a call after [`Injector::finish`].
    Usage(&'static str),
    /// A write primitive refused (an internal error).
    Mux(MuxError),
}

impl fmt::Display for InjectError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            InjectError::InvalidConfig(why) => write!(f, "invalid injector configuration: {why}"),
            InjectError::InvalidUnit(why) => write!(f, "invalid unit: {why}"),
            InjectError::OutOfOrder { pts, last } => write!(
                f,
                "synchronous unit with PTS {pts} after PTS {last} was sent; units must not go back in time (R-ISO13818-160)"
            ),
            InjectError::QueueFull { units, bytes } => write!(f, "unit queue full ({units} units, {bytes} bytes)"),
            InjectError::NoTarget { held } => write!(f, "no target program found (PAT and PMT) within {held} packets"),
            InjectError::Scrambled { program, pid } => {
                write!(f, "program {program} is scrambled (PID 0x{pid:04X}); refusing to inject (R-ISO13818-007)")
            }
            InjectError::ExistingKlv { program, pid } => {
                write!(f, "program {program} already carries KLV on PID 0x{pid:04X}")
            }
            InjectError::PidInUse { pid, reason } => write!(f, "PID 0x{pid:04X} cannot carry the KLV stream: {reason}"),
            InjectError::NoFreePid => f.write_str("no free PID for the KLV stream"),
            InjectError::ServiceIdInUse(id) => {
                write!(f, "metadata_service_id {id} is used by another metadata service (R-ST1402TIME-35)")
            }
            InjectError::NoFreeServiceId => f.write_str("every metadata_service_id is in use"),
            InjectError::SectionTooLong { section_length } => write!(
                f,
                "the rewritten PMT would need section_length {section_length}, over the limit of 1021 (R-ISO13818-091)"
            ),
            InjectError::CbrOverflow { pts, packets, free_per_second } => {
                write!(f, "CBR output has no room for a KLV unit of {packets} packets")?;
                if let Some(pts) = pts {
                    write!(f, " (PTS {pts})")?;
                }
                write!(f, " before its deadline: {free_per_second} free slots per second measured")
            }
            InjectError::Unsupported(why) => write!(f, "unsupported input: {why}"),
            InjectError::UserData(why) => write!(f, "user data write refused: {why}"),
            InjectError::Usage(why) => f.write_str(why),
            InjectError::Mux(e) => write!(f, "write error: {e}"),
        }
    }
}

impl std::error::Error for InjectError {}

impl From<MuxError> for InjectError {
    fn from(e: MuxError) -> Self {
        InjectError::Mux(e)
    }
}

impl InjectError {
    /// Errors that end the injector.
    fn is_final(&self) -> bool {
        !matches!(
            self,
            InjectError::InvalidUnit(_)
                | InjectError::OutOfOrder { .. }
                | InjectError::QueueFull { .. }
                | InjectError::UserData(_)
        )
    }
}

/// Input packets waiting for the target program's PMT, with their clock
/// and M2TS TP_extra_header.
#[derive(Debug, Default)]
struct Hold {
    tables: Tables,
    observed: Pids,
    sections: Sections,
    packets: VecDeque<(Packet, Option<u64>, [u8; 4])>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Input {
    None,
    Bytes,
    Packets,
}

/// Everything after framing.
#[derive(Debug)]
struct Pipeline {
    input: Input,
    hold: Option<Hold>,
    /// The program clock for [`Injector::push`]; `None` while the target
    /// has no PCR.
    timeline: Option<PcrTimeline>,
    /// M2TS TP_extra_headers of the packets the timeline holds, in order.
    recs: VecDeque<[u8; 4]>,
    core: Core,
}

/// Inserts KLV metadata into a transport stream as it passes through.
///
/// Push the input as bytes ([`push`](Self::push), any chunking; packets are
/// timed on the program clock by an internal [`PcrTimeline`], which holds
/// each PCR interval until its closing PCR arrives, or with
/// [`ClockMode::Extrapolate`] holds nothing) or as packets with their clock
/// ([`push_packet`](Self::push_packet), or [`push_record`](Self::push_record)
/// for M2TS, for callers that time the input themselves, for example to
/// pace output). Use one style per injector. Units go in with
/// [`insert`](Self::insert), or from the frame callback of the `_with`
/// variants, which runs right before each video frame of the target is
/// written (with [`VideoSei::Scan`] or [`VideoSei::Write`], once the
/// frame's access unit prefix has been read).
///
/// Until the target program's PMT is known, input is held (at most
/// [`InjectConfig::max_held_packets`] packets), so every decision applies
/// from the first packet. Memory is bounded by the configuration. Nothing
/// panics on any input.
#[derive(Debug)]
pub struct Injector {
    framer: Framer,
    pipe: Pipeline,
    failed: Option<InjectError>,
    /// [`Injector::end_input_with`] ran: no more input, units still go in.
    input_ended: bool,
    finished: bool,
}

impl Injector {
    /// An injector with these settings.
    pub fn new(cfg: InjectConfig) -> Result<Self, InjectError> {
        cfg.validate()?;
        Ok(Injector {
            framer: Framer::new(cfg.stride()),
            pipe: Pipeline {
                input: Input::None,
                hold: Some(Hold::new()),
                timeline: None,
                recs: VecDeque::new(),
                core: Core::new(cfg),
            },
            failed: None,
            input_ended: false,
            finished: false,
        })
    }

    /// The settings.
    pub fn config(&self) -> &InjectConfig {
        &self.pipe.core.cfg
    }

    fn usable(&mut self, input: Option<Input>) -> Result<(), InjectError> {
        if let Some(e) = &self.failed {
            return Err(e.clone());
        }
        if self.finished {
            return Err(InjectError::Usage("the injector is finished"));
        }
        if let Some(input) = input {
            if self.input_ended {
                return Err(InjectError::Usage("the input has ended"));
            }
            match self.pipe.input {
                Input::None => self.pipe.input = input,
                i if i == input => {}
                _ => {
                    return Err(InjectError::Usage(
                        "push and push_packet cannot be mixed on one injector",
                    ))
                }
            }
        }
        Ok(())
    }

    fn outcome(&mut self, r: Result<(), InjectError>) -> Result<(), InjectError> {
        if let Err(e) = &r {
            if e.is_final() {
                self.failed = Some(e.clone());
            }
        }
        r
    }

    /// Push input bytes in any chunking (188-byte packets, or 192-byte
    /// records with [`RecordFormat::M2ts`]); the output goes to `sink`.
    pub fn push<S: PacketSink + ?Sized>(
        &mut self,
        data: &[u8],
        sink: &mut S,
    ) -> Result<(), InjectError> {
        self.push_with(data, sink, &mut |_: &Frame, _: &mut Units<'_>| {})
    }

    /// [`push`](Self::push), calling `on_frame` right before the first
    /// packet of each video frame of the target is written: units inserted
    /// through its [`Units`] can go out immediately before that frame. With
    /// [`VideoSei::Scan`] or [`VideoSei::Write`] it runs once the frame's
    /// access unit prefix has been read, and its [`Units`] gives the user
    /// data found there and takes a write.
    pub fn push_with<S, F>(
        &mut self,
        data: &[u8],
        sink: &mut S,
        on_frame: &mut F,
    ) -> Result<(), InjectError>
    where
        S: PacketSink + ?Sized,
        F: FnMut(&Frame, &mut Units<'_>) + ?Sized,
    {
        self.usable(Some(Input::Bytes))?;
        let mut result = Ok(());
        let pipe = &mut self.pipe;
        let m2ts = pipe.core.cfg.record == RecordFormat::M2ts;
        self.framer.push(data, &mut |unit| {
            let (rec, p) = if m2ts { split_unit(unit) } else { ([0; 4], as_packet(unit)) };
            match pipe.bytes_packet(rec, p, sink, on_frame) {
                Ok(()) => true,
                Err(e) => {
                    result = Err(e);
                    false
                }
            }
        });
        self.pipe.core.stats.bytes_skipped = self.framer.bytes_skipped;
        self.pipe.core.stats.sync_losses = self.framer.sync_losses;
        self.outcome(result)
    }

    /// Push one packet with its program clock (27 MHz, as
    /// [`TimedPacket::clock`](crate::TimedPacket::clock) gives it). No
    /// clock is held back: the caller's clock is used as is. With
    /// [`RecordFormat::M2ts`], use [`push_record`](Self::push_record).
    pub fn push_packet<S: PacketSink + ?Sized>(
        &mut self,
        packet: &Packet,
        clock: Option<u64>,
        sink: &mut S,
    ) -> Result<(), InjectError> {
        self.push_packet_with(packet, clock, sink, &mut |_: &Frame, _: &mut Units<'_>| {})
    }

    /// [`push_packet`](Self::push_packet) with a frame callback, as in
    /// [`push_with`](Self::push_with).
    pub fn push_packet_with<S, F>(
        &mut self,
        packet: &Packet,
        clock: Option<u64>,
        sink: &mut S,
        on_frame: &mut F,
    ) -> Result<(), InjectError>
    where
        S: PacketSink + ?Sized,
        F: FnMut(&Frame, &mut Units<'_>) + ?Sized,
    {
        if self.pipe.core.cfg.record != RecordFormat::Ts {
            self.usable(None)?;
            return Err(InjectError::Usage(
                "push_packet takes 188-byte packets; with RecordFormat::M2ts use push_record",
            ));
        }
        self.usable(Some(Input::Packets))?;
        let r = self.pipe.timed_packet(packet, clock, [0; 4], sink, on_frame);
        self.outcome(r)
    }

    /// Push one 192-byte M2TS record ([`RecordFormat::M2ts`]) with the
    /// program clock of its packet, as [`push_packet`](Self::push_packet)
    /// does for packets. Its TP_extra_header goes out with the packet.
    pub fn push_record<S: PacketSink + ?Sized>(
        &mut self,
        record: &Record,
        clock: Option<u64>,
        sink: &mut S,
    ) -> Result<(), InjectError> {
        self.push_record_with(record, clock, sink, &mut |_: &Frame, _: &mut Units<'_>| {})
    }

    /// [`push_record`](Self::push_record) with a frame callback, as in
    /// [`push_with`](Self::push_with).
    pub fn push_record_with<S, F>(
        &mut self,
        record: &Record,
        clock: Option<u64>,
        sink: &mut S,
        on_frame: &mut F,
    ) -> Result<(), InjectError>
    where
        S: PacketSink + ?Sized,
        F: FnMut(&Frame, &mut Units<'_>) + ?Sized,
    {
        if self.pipe.core.cfg.record != RecordFormat::M2ts {
            self.usable(None)?;
            return Err(InjectError::Usage("push_record needs RecordFormat::M2ts"));
        }
        self.usable(Some(Input::Packets))?;
        let (rec, p) = split_unit(record);
        let r = self.pipe.timed_packet(p, clock, rec, sink, on_frame);
        self.outcome(r)
    }

    /// Queue a unit. Synchronous units need a PTS and are sent in PTS
    /// order; a PTS below the last one sent is refused
    /// ([`InjectError::OutOfOrder`]). Asynchronous units have no PTS and go
    /// out in insertion order. [`InjectError::QueueFull`] asks the caller
    /// to retry after pushing more input.
    pub fn insert(&mut self, unit: InsertUnit) -> Result<(), InjectError> {
        self.usable(None)?;
        self.pipe.core.queue.insert(unit)
    }

    /// The input has ended: process every packet still held (the bytes of
    /// an unfinished packet, the input held for the tables, and the packets
    /// after the last PCR, timed at the last known rate), with the frame
    /// callback for the frames among them, but do not end the stream yet.
    /// Units can still be inserted (for example the metadata of the last
    /// frames, which a scheduler holding frames for presentation order only
    /// releases now) before [`finish`](Self::finish) places what it can.
    /// Pushing input afterwards is an error.
    pub fn end_input_with<S, F>(&mut self, sink: &mut S, on_frame: &mut F) -> Result<(), InjectError>
    where
        S: PacketSink + ?Sized,
        F: FnMut(&Frame, &mut Units<'_>) + ?Sized,
    {
        self.usable(None)?;
        if self.input_ended {
            return Ok(());
        }
        let mut result = Ok(());
        let pipe = &mut self.pipe;
        if pipe.input == Input::Bytes {
            self.framer.finish(&mut |unit| {
                let (rec, p) = split_unit(unit);
                match pipe.bytes_packet(rec, p, sink, on_frame) {
                    Ok(()) => true,
                    Err(e) => {
                        result = Err(e);
                        false
                    }
                }
            });
            pipe.core.stats.bytes_skipped = self.framer.bytes_skipped;
            pipe.core.stats.sync_losses = self.framer.sync_losses;
        }
        let r = result.and_then(|()| pipe.flush_held(sink, on_frame));
        self.outcome(r)?;
        self.input_ended = true;
        Ok(())
    }

    /// End of input: flush what is held, send what can still be placed,
    /// drop and count the rest. Returns the final counters. Frames still
    /// held (after the last PCR) go out without a frame callback; use
    /// [`finish_with`](Self::finish_with) to see them.
    pub fn finish<S: PacketSink + ?Sized>(
        &mut self,
        sink: &mut S,
    ) -> Result<InjectStats, InjectError> {
        self.finish_with(sink, &mut |_: &Frame, _: &mut Units<'_>| {})
    }

    /// [`finish`](Self::finish) with the frame callback of
    /// [`push_with`](Self::push_with), for the frames still held.
    pub fn finish_with<S, F>(
        &mut self,
        sink: &mut S,
        on_frame: &mut F,
    ) -> Result<InjectStats, InjectError>
    where
        S: PacketSink + ?Sized,
        F: FnMut(&Frame, &mut Units<'_>) + ?Sized,
    {
        self.end_input_with(sink, on_frame)?;
        let r = self.pipe.core.end(sink);
        self.outcome(r)?;
        self.finished = true;
        Ok(self.stats())
    }

    /// The target program, once chosen.
    pub fn target(&self) -> Option<&Target> {
        self.pipe.core.target()
    }

    /// The program clock (27 MHz, wrapping like
    /// [`TimedPacket::clock`](crate::TimedPacket::clock)) of the last input
    /// packet processed, once a PCR has been seen. Compare it with PTS x
    /// 300 to decide how far ahead to queue units.
    pub fn clock(&self) -> Option<u64> {
        self.pipe.core.clock()
    }

    /// The counters.
    pub fn stats(&self) -> InjectStats {
        let mut s = self.pipe.core.snapshot();
        let held = self.pipe.hold.as_ref().map_or(0, |h| h.packets.len())
            + self.pipe.timeline.as_ref().map_or(0, |t| t.held())
            + self.pipe.core.video_held();
        s.held_packets = held as u64;
        s
    }

    /// The oldest event not yet popped.
    pub fn pop_event(&mut self) -> Option<InjectEvent> {
        self.pipe.core.events.pop_front()
    }
}

/// A framed 188-byte unit as a packet.
fn as_packet(unit: &[u8]) -> &Packet {
    unit.try_into().expect("the framer gives whole packets")
}

/// A framed unit as its M2TS TP_extra_header (zeros for 188-byte units)
/// and its packet.
fn split_unit(unit: &[u8]) -> ([u8; 4], &Packet) {
    let off = unit.len().saturating_sub(PACKET_SIZE).min(4);
    let mut rec = [0u8; 4];
    rec[..off].copy_from_slice(&unit[..off]);
    let p: &Packet = unit[off..off + PACKET_SIZE].try_into().expect("a unit ends with a packet");
    (rec, p)
}

impl Hold {
    fn new() -> Self {
        Hold {
            tables: Tables::new(),
            ..Hold::default()
        }
    }
}

impl Pipeline {
    /// A framed packet from [`Injector::push`].
    fn bytes_packet<S, F>(
        &mut self,
        rec: [u8; 4],
        p: &Packet,
        sink: &mut S,
        on_frame: &mut F,
    ) -> Result<(), InjectError>
    where
        S: PacketSink + ?Sized,
        F: FnMut(&Frame, &mut Units<'_>) + ?Sized,
    {
        if self.hold.is_some() {
            return self.hold_packet(p, None, rec, sink, on_frame);
        }
        self.timed(p, rec, sink, on_frame)
    }

    /// A packet through the timeline (or straight on without a PCR).
    fn timed<S, F>(&mut self, p: &Packet, rec: [u8; 4], sink: &mut S, on_frame: &mut F) -> Result<(), InjectError>
    where
        S: PacketSink + ?Sized,
        F: FnMut(&Frame, &mut Units<'_>) + ?Sized,
    {
        let core = &mut self.core;
        let r = match &mut self.timeline {
            Some(t) => {
                let mut result = Ok(());
                // The timeline releases packets in order, so M2TS headers
                // come off this queue in step.
                let m2ts = core.cfg.record == RecordFormat::M2ts;
                if m2ts {
                    self.recs.push_back(rec);
                }
                let recs = &mut self.recs;
                t.push(p, &mut |tp| {
                    let rec = if m2ts { recs.pop_front().unwrap_or_default() } else { [0; 4] };
                    if result.is_ok() {
                        result = core.process(&tp.packet, tp.clock, rec, sink, on_frame);
                    }
                });
                result
            }
            None => core.process(p, None, rec, sink, on_frame),
        };
        r?;
        self.follow_pcr_pid();
        Ok(())
    }

    /// A timeline on the target's PCR_PID, in the configured clock mode.
    fn new_timeline(&self, pid: u16) -> PcrTimeline {
        PcrTimeline::new(Some(pid), 10_000_000)
            .with_max_held(self.core.cfg.max_held_packets)
            .with_mode(self.core.cfg.clock)
    }

    /// Keep the timeline on the target's PCR_PID.
    fn follow_pcr_pid(&mut self) {
        if self.input != Input::Bytes {
            return;
        }
        let Some(pid) = self.core.target().and_then(|t| t.pcr_pid) else {
            return;
        };
        match &mut self.timeline {
            Some(t) if t.pcr_pid() != Some(pid) => t.set_pcr_pid(pid),
            Some(_) => {}
            None => self.timeline = Some(self.new_timeline(pid)),
        }
    }

    /// A packet with its clock from [`Injector::push_packet`] or
    /// [`Injector::push_record`].
    fn timed_packet<S, F>(
        &mut self,
        p: &Packet,
        clock: Option<u64>,
        rec: [u8; 4],
        sink: &mut S,
        on_frame: &mut F,
    ) -> Result<(), InjectError>
    where
        S: PacketSink + ?Sized,
        F: FnMut(&Frame, &mut Units<'_>) + ?Sized,
    {
        if self.hold.is_some() {
            return self.hold_packet(p, clock, rec, sink, on_frame);
        }
        self.core.process(p, clock, rec, sink, on_frame)
    }

    /// Hold a packet until the target is known; start when it is.
    fn hold_packet<S, F>(
        &mut self,
        p: &Packet,
        clock: Option<u64>,
        rec: [u8; 4],
        sink: &mut S,
        on_frame: &mut F,
    ) -> Result<(), InjectError>
    where
        S: PacketSink + ?Sized,
        F: FnMut(&Frame, &mut Units<'_>) + ?Sized,
    {
        let hold = self.hold.as_mut().expect("holding");
        let pid = u16::from_be_bytes([p[1] & 0x1F, p[2]]);
        hold.observed.set(pid);
        let mut changed = false;
        if p[0] == crate::SYNC_BYTE && hold.tables.is_psi(pid) {
            hold.sections.clear();
            hold.tables.assemble(p, &mut hold.sections);
            for i in 0..hold.sections.len() {
                changed |= hold.tables.apply(pid, hold.sections.get(i)) == Applied::Changed;
            }
        }
        hold.packets.push_back((*p, clock, rec));
        let full = hold.packets.len() >= self.core.cfg.max_held_packets;
        if changed || full {
            self.try_start(full, sink, on_frame)?;
        }
        Ok(())
    }

    /// Start once the tables allow choosing the target. `last_chance`: the
    /// hold limit or the end of input; choose among what is known or fail.
    fn try_start<S, F>(
        &mut self,
        last_chance: bool,
        sink: &mut S,
        on_frame: &mut F,
    ) -> Result<(), InjectError>
    where
        S: PacketSink + ?Sized,
        F: FnMut(&Frame, &mut Units<'_>) + ?Sized,
    {
        let hold = self.hold.as_ref().expect("holding");
        let t = &hold.tables;
        // Every PMT of the PAT, so that the PID and metadata_service_id
        // chosen are free in the whole transport stream (D-ISO13818-03,
        // D-ISO13818-11); at the hold limit, what is known.
        let ready = match self.core.cfg.program {
            ProgramSelect::Number(n) => {
                t.program(n).is_some_and(|p| p.pmt.is_some()) && (t.complete() || last_chance)
            }
            ProgramSelect::Auto => t.complete() || (last_chance && t.pmts().next().is_some()),
        };
        if !ready {
            if last_chance {
                return Err(InjectError::NoTarget {
                    held: hold.packets.len(),
                });
            }
            return Ok(());
        }
        let mut hold = self.hold.take().expect("holding");
        hold.tables.reset_assemblers();
        self.core.start(hold.tables, hold.observed)?;
        if self.input == Input::Bytes {
            if let Some(pid) = self.core.target().and_then(|t| t.pcr_pid) {
                self.timeline = Some(self.new_timeline(pid));
            }
        }
        while let Some((p, clock, rec)) = hold.packets.pop_front() {
            if self.input == Input::Bytes {
                self.timed(&p, rec, sink, on_frame)?;
            } else {
                self.core.process(&p, clock, rec, sink, on_frame)?;
            }
        }
        Ok(())
    }

    /// Process every packet still held: the input held for the tables, the
    /// internal timeline's last PCR interval and a video access unit's
    /// first packets.
    fn flush_held<S, F>(&mut self, sink: &mut S, on_frame: &mut F) -> Result<(), InjectError>
    where
        S: PacketSink + ?Sized,
        F: FnMut(&Frame, &mut Units<'_>) + ?Sized,
    {
        if self.hold.is_some() {
            self.try_start(true, sink, on_frame)?;
        }
        if let Some(t) = &mut self.timeline {
            let core = &mut self.core;
            let recs = &mut self.recs;
            let mut result = Ok(());
            t.flush(&mut |tp| {
                let rec = recs.pop_front().unwrap_or_default();
                if result.is_ok() {
                    result = core.process(&tp.packet, tp.clock, rec, sink, on_frame);
                }
            });
            result?;
        }
        self.core.flush_video(sink, on_frame)
    }
}
