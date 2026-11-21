//! C ABI of the KLV injector (features `capi` and `mux`). The rules of
//! `include/mpegts.h` apply. Output goes to a caller callback, whole
//! packets at a time, while a call runs.

use super::*;
use crate::mux::{
    DropReason, ExistingKlv, Frame, InjectConfig, InjectError, InjectEvent, InjectStats,
    InjectWarning, Injector, InsertUnit, KlvCarriage, OnOverflow, Packet, PacketSink,
    ProgramSelect, RateMode, SyncKlvOptions, Target, Units,
};
use crate::packet::NULL_PID;
use std::os::raw::c_void;

pub const MPEGTS_INJECT_SYNC: u32 = 1;
pub const MPEGTS_INJECT_ASYNC: u32 = 2;

pub const MPEGTS_EXISTING_KLV_KEEP: u32 = 0;
pub const MPEGTS_EXISTING_KLV_REPLACE: u32 = 1;
pub const MPEGTS_EXISTING_KLV_FAIL: u32 = 2;

pub const MPEGTS_RATE_VBR: u32 = 0;
pub const MPEGTS_RATE_CBR_FAIL: u32 = 1;
pub const MPEGTS_RATE_CBR_DROP_OLDEST: u32 = 2;

pub const MPEGTS_INJECT_HAS_SERVICE_ID: u32 = 0x01;
pub const MPEGTS_INJECT_HAS_PID: u32 = 0x02;
pub const MPEGTS_INJECT_NO_FRAME_ALIGN: u32 = 0x04;
pub const MPEGTS_INJECT_SYNC_REGISTRATION: u32 = 0x08;
const CONFIG_FLAGS: u32 = 0x0F;

pub const MPEGTS_INJECT_UNIT_HAS_PTS: u32 = 0x01;

pub const MPEGTS_FRAME_HAS_DTS: u32 = 0x01;
pub const MPEGTS_FRAME_HAS_CLOCK: u32 = 0x02;
pub const MPEGTS_FRAME_RANDOM_ACCESS: u32 = 0x04;
pub const MPEGTS_FRAME_DISCONTINUITY: u32 = 0x08;

pub const MPEGTS_TARGET_MERGED: u32 = 0x01;
pub const MPEGTS_TARGET_HAS_SERVICE_ID: u32 = 0x02;

pub const MPEGTS_EVENT_TARGET_FOUND: u32 = 1;
pub const MPEGTS_EVENT_TARGET_CHANGED: u32 = 2;
pub const MPEGTS_EVENT_TARGET_LOST: u32 = 3;
pub const MPEGTS_EVENT_DISCONTINUITY: u32 = 4;
pub const MPEGTS_EVENT_DROPPED: u32 = 5;
pub const MPEGTS_EVENT_WARNING: u32 = 6;

pub const MPEGTS_EVENT_HAS_PTS: u32 = 0x01;

pub const MPEGTS_DROP_LATE: u32 = 1;
pub const MPEGTS_DROP_OVERFLOW: u32 = 2;
pub const MPEGTS_DROP_DISCONTINUITY: u32 = 3;
pub const MPEGTS_DROP_UNPLACED: u32 = 4;

pub const MPEGTS_WARN_MULTI_PROGRAM_VBR: u32 = 1;
pub const MPEGTS_WARN_SERVICE_ID_CHANGED: u32 = 2;
pub const MPEGTS_WARN_SERVICE_ID_IN_INPUT: u32 = 3;
pub const MPEGTS_WARN_LATE_UNIT: u32 = 4;

/// Receives output packets: `len` is a multiple of 188.
pub type mpegts_packets_fn =
    Option<unsafe extern "C" fn(user: *mut c_void, packets: *const u8, len: usize)>;
/// Called right before the first packet of each video frame of the target
/// is written; `units` is valid during the call only.
pub type mpegts_frame_fn = Option<
    unsafe extern "C" fn(
        user: *mut c_void,
        frame: *const mpegts_inject_frame,
        units: *mut mpegts_inject_units,
    ),
>;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct mpegts_inject_config {
    pub struct_size: u32,
    pub flags: u32,
    pub program: u32,
    pub carriage: u32,
    pub existing_klv: u32,
    pub rate: u32,
    pub pid: u16,
    pub service_id: u8,
    pub reserved: u8,
    pub psi_interval_ms: u32,
    pub max_lead_ms: u32,
    pub min_lead_ms: u32,
    pub fallback_lead_ms: u32,
    pub input_leak_rate: u32,
    pub buffer_size: u32,
    pub output_leak_rate: u32,
    pub max_queued_units: u64,
    pub max_queued_bytes: u64,
    pub max_held_packets: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct mpegts_inject_unit {
    pub struct_size: u32,
    pub flags: u32,
    pub pts: u64,
    pub data: *const u8,
    pub data_len: u64,
}

impl Default for mpegts_inject_unit {
    fn default() -> Self {
        mpegts_inject_unit {
            struct_size: 0,
            flags: 0,
            pts: 0,
            data: ptr::null(),
            data_len: 0,
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct mpegts_inject_frame {
    pub struct_size: u32,
    pub flags: u32,
    pub pid: u16,
    pub reserved: u16,
    pub reserved2: u32,
    pub pts: u64,
    pub dts: u64,
    pub clock: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct mpegts_inject_target {
    pub struct_size: u32,
    pub flags: u32,
    pub program: u16,
    pub pmt_pid: u16,
    pub pcr_pid: u16,
    pub video_pid: u16,
    pub klv_pid: u16,
    pub service_id: u8,
    pub carriage: u8,
    pub stripped_count: u32,
    pub stripped: [u16; 8],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct mpegts_inject_stats {
    pub struct_size: u32,
    pub reserved: u32,
    pub packets_in: u64,
    pub packets_out: u64,
    pub bytes_skipped: u64,
    pub sync_losses: u64,
    pub held_packets: u64,
    pub frames: u64,
    pub units_inserted: u64,
    pub units_queued: u64,
    pub bytes_queued: u64,
    pub klv_packets: u64,
    pub psi_packets: u64,
    pub psi_extra_packets: u64,
    pub stripped_packets: u64,
    pub pcr_only_packets: u64,
    pub null_packets: u64,
    pub free_slots: u64,
    pub slots_filled: u64,
    pub dropped_late: u64,
    pub dropped_overflow: u64,
    pub dropped_discontinuity: u64,
    pub dropped_unplaced: u64,
    pub late_units: u64,
    pub psi_crc_errors: u64,
    pub metadata_buffer_peak: u64,
    pub null_bitrate: u64,
    pub free_bitrate: u64,
    pub added_bitrate: i64,
    pub events_dropped: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct mpegts_inject_event {
    pub struct_size: u32,
    pub kind: u32,
    pub code: u32,
    pub flags: u32,
    pub value: u64,
    pub bytes: u64,
}

/// Opaque injector handle.
#[derive(Debug)]
pub struct mpegts_injector {
    injector: Injector,
    /// Output collected for the callback, reused between calls.
    out: Vec<u8>,
    error: String,
    poisoned: bool,
}

/// Opaque handle to insert units from a frame callback.
#[derive(Debug)]
pub struct mpegts_inject_units {
    /// The `Units` of the running callback.
    units: *mut c_void,
}

impl mpegts_inject_config {
    fn from_config(c: &InjectConfig) -> Self {
        let mut flags = 0;
        if c.service_id.is_some() {
            flags |= MPEGTS_INJECT_HAS_SERVICE_ID;
        }
        if c.pid.is_some() {
            flags |= MPEGTS_INJECT_HAS_PID;
        }
        if !c.align_to_frames {
            flags |= MPEGTS_INJECT_NO_FRAME_ALIGN;
        }
        if c.sync.sync_registration_descriptor {
            flags |= MPEGTS_INJECT_SYNC_REGISTRATION;
        }
        mpegts_inject_config {
            struct_size: size_of_u32::<mpegts_inject_config>(),
            flags,
            program: match c.program {
                ProgramSelect::Number(n) => u32::from(n),
                _ => 0,
            },
            carriage: match c.carriage {
                KlvCarriage::Async => MPEGTS_INJECT_ASYNC,
                _ => MPEGTS_INJECT_SYNC,
            },
            existing_klv: match c.existing_klv {
                ExistingKlv::Replace => MPEGTS_EXISTING_KLV_REPLACE,
                ExistingKlv::Fail => MPEGTS_EXISTING_KLV_FAIL,
                _ => MPEGTS_EXISTING_KLV_KEEP,
            },
            rate: match c.rate {
                RateMode::Cbr {
                    on_overflow: OnOverflow::DropOldest,
                } => MPEGTS_RATE_CBR_DROP_OLDEST,
                RateMode::Cbr { .. } => MPEGTS_RATE_CBR_FAIL,
                _ => MPEGTS_RATE_VBR,
            },
            pid: c.pid.unwrap_or(0),
            service_id: c.service_id.unwrap_or(0),
            reserved: 0,
            psi_interval_ms: c.psi_interval_ms,
            max_lead_ms: c.max_lead_ms,
            min_lead_ms: c.min_lead_ms,
            fallback_lead_ms: c.fallback_lead_ms,
            input_leak_rate: c.sync.input_leak_rate,
            buffer_size: c.sync.buffer_size,
            output_leak_rate: c.sync.output_leak_rate,
            max_queued_units: c.max_queued_units as u64,
            max_queued_bytes: c.max_queued_bytes as u64,
            max_held_packets: c.max_held_packets as u64,
        }
    }

    fn to_config(self) -> Result<InjectConfig, &'static str> {
        if self.flags & !CONFIG_FLAGS != 0 {
            return Err("unknown MPEGTS_INJECT_* flags");
        }
        let size = |v: u64| usize::try_from(v).map_err(|_| "a limit is too large");
        let sync = SyncKlvOptions {
            sync_registration_descriptor: self.flags & MPEGTS_INJECT_SYNC_REGISTRATION != 0,
            input_leak_rate: self.input_leak_rate,
            buffer_size: self.buffer_size,
            output_leak_rate: self.output_leak_rate,
            ..SyncKlvOptions::default()
        };
        let program = match self.program {
            0 => ProgramSelect::Auto,
            n => ProgramSelect::Number(
                u16::try_from(n).map_err(|_| "program must be at most 65535")?,
            ),
        };
        let carriage = match self.carriage {
            MPEGTS_INJECT_SYNC => KlvCarriage::Sync,
            MPEGTS_INJECT_ASYNC => KlvCarriage::Async,
            _ => return Err("carriage must be MPEGTS_INJECT_SYNC or MPEGTS_INJECT_ASYNC"),
        };
        let existing = match self.existing_klv {
            MPEGTS_EXISTING_KLV_KEEP => ExistingKlv::Keep,
            MPEGTS_EXISTING_KLV_REPLACE => ExistingKlv::Replace,
            MPEGTS_EXISTING_KLV_FAIL => ExistingKlv::Fail,
            _ => return Err("existing_klv must be an MPEGTS_EXISTING_KLV_* value"),
        };
        let rate = match self.rate {
            MPEGTS_RATE_VBR => RateMode::Vbr,
            MPEGTS_RATE_CBR_FAIL => RateMode::Cbr {
                on_overflow: OnOverflow::Fail,
            },
            MPEGTS_RATE_CBR_DROP_OLDEST => RateMode::Cbr {
                on_overflow: OnOverflow::DropOldest,
            },
            _ => return Err("rate must be an MPEGTS_RATE_* value"),
        };
        let mut c = InjectConfig::default()
            .with_program(program)
            .with_carriage(carriage)
            .with_existing_klv(existing)
            .with_rate(rate)
            .with_psi_interval_ms(self.psi_interval_ms)
            .with_lead_ms(self.min_lead_ms, self.max_lead_ms)
            .with_align_to_frames(self.flags & MPEGTS_INJECT_NO_FRAME_ALIGN == 0)
            .with_queue_limits(size(self.max_queued_units)?, size(self.max_queued_bytes)?)
            .with_max_held_packets(size(self.max_held_packets)?)
            .with_sync_options(sync);
        c.fallback_lead_ms = self.fallback_lead_ms;
        if self.flags & MPEGTS_INJECT_HAS_SERVICE_ID != 0 {
            c = c.with_service_id(self.service_id);
        }
        if self.flags & MPEGTS_INJECT_HAS_PID != 0 {
            c = c.with_pid(self.pid);
        }
        Ok(c)
    }
}

fn inject_status(e: &InjectError) -> i32 {
    match e {
        InjectError::InvalidConfig(_)
        | InjectError::InvalidUnit(_)
        | InjectError::OutOfOrder { .. }
        | InjectError::Usage(_) => MPEGTS_ERR_INVALID,
        InjectError::QueueFull { .. } => MPEGTS_ERR_FULL,
        _ => MPEGTS_ERR_STREAM,
    }
}

fn stats_struct(s: &InjectStats) -> mpegts_inject_stats {
    mpegts_inject_stats {
        struct_size: size_of_u32::<mpegts_inject_stats>(),
        reserved: 0,
        packets_in: s.packets_in,
        packets_out: s.packets_out,
        bytes_skipped: s.bytes_skipped,
        sync_losses: s.sync_losses,
        held_packets: s.held_packets,
        frames: s.frames,
        units_inserted: s.units_inserted,
        units_queued: s.units_queued,
        bytes_queued: s.bytes_queued,
        klv_packets: s.klv_packets,
        psi_packets: s.psi_packets,
        psi_extra_packets: s.psi_extra_packets,
        stripped_packets: s.stripped_packets,
        pcr_only_packets: s.pcr_only_packets,
        null_packets: s.null_packets,
        free_slots: s.free_slots,
        slots_filled: s.slots_filled,
        dropped_late: s.dropped_late,
        dropped_overflow: s.dropped_overflow,
        dropped_discontinuity: s.dropped_discontinuity,
        dropped_unplaced: s.dropped_unplaced,
        late_units: s.late_units,
        psi_crc_errors: s.psi_crc_errors,
        metadata_buffer_peak: s.metadata_buffer_peak,
        null_bitrate: s.null_bitrate,
        free_bitrate: s.free_bitrate,
        added_bitrate: s.added_bitrate,
        events_dropped: s.events_dropped,
    }
}

fn target_struct(t: &Target) -> mpegts_inject_target {
    let mut stripped = [0u16; 8];
    for (d, s) in stripped.iter_mut().zip(&t.stripped) {
        *d = *s;
    }
    mpegts_inject_target {
        struct_size: size_of_u32::<mpegts_inject_target>(),
        flags: if t.merged { MPEGTS_TARGET_MERGED } else { 0 }
            | if t.service_id.is_some() {
                MPEGTS_TARGET_HAS_SERVICE_ID
            } else {
                0
            },
        program: t.program,
        pmt_pid: t.pmt_pid,
        pcr_pid: t.pcr_pid.unwrap_or(NULL_PID),
        video_pid: t.video_pid.unwrap_or(NULL_PID),
        klv_pid: t.klv_pid,
        service_id: t.service_id.unwrap_or(0),
        carriage: match t.carriage {
            KlvCarriage::Async => MPEGTS_INJECT_ASYNC as u8,
            _ => MPEGTS_INJECT_SYNC as u8,
        },
        stripped_count: t.stripped.len() as u32,
        stripped,
    }
}

fn event_struct(e: &InjectEvent) -> mpegts_inject_event {
    let mut ev = mpegts_inject_event {
        struct_size: size_of_u32::<mpegts_inject_event>(),
        ..Default::default()
    };
    match e {
        InjectEvent::TargetFound(t) => {
            ev.kind = MPEGTS_EVENT_TARGET_FOUND;
            ev.value = u64::from(t.program);
        }
        InjectEvent::TargetChanged(t) => {
            ev.kind = MPEGTS_EVENT_TARGET_CHANGED;
            ev.value = u64::from(t.program);
        }
        InjectEvent::TargetLost { program } => {
            ev.kind = MPEGTS_EVENT_TARGET_LOST;
            ev.value = u64::from(*program);
        }
        InjectEvent::Discontinuity { pcr } => {
            ev.kind = MPEGTS_EVENT_DISCONTINUITY;
            ev.value = *pcr;
        }
        InjectEvent::Dropped { reason, pts, bytes } => {
            ev.kind = MPEGTS_EVENT_DROPPED;
            ev.code = match reason {
                DropReason::Late => MPEGTS_DROP_LATE,
                DropReason::Overflow => MPEGTS_DROP_OVERFLOW,
                DropReason::Discontinuity => MPEGTS_DROP_DISCONTINUITY,
                _ => MPEGTS_DROP_UNPLACED,
            };
            if let Some(pts) = pts {
                ev.flags = MPEGTS_EVENT_HAS_PTS;
                ev.value = *pts;
            }
            ev.bytes = *bytes as u64;
        }
        InjectEvent::Warning(w) => {
            ev.kind = MPEGTS_EVENT_WARNING;
            match *w {
                InjectWarning::MultiProgramVbr { programs } => {
                    ev.code = MPEGTS_WARN_MULTI_PROGRAM_VBR;
                    ev.value = programs as u64;
                }
                InjectWarning::ServiceIdChanged { from, to } => {
                    ev.code = MPEGTS_WARN_SERVICE_ID_CHANGED;
                    ev.value = u64::from(to);
                    ev.bytes = u64::from(from);
                }
                InjectWarning::ServiceIdInInput { service_id } => {
                    ev.code = MPEGTS_WARN_SERVICE_ID_IN_INPUT;
                    ev.value = u64::from(service_id);
                }
                InjectWarning::LateUnit { pts } => {
                    ev.code = MPEGTS_WARN_LATE_UNIT;
                    ev.flags = MPEGTS_EVENT_HAS_PTS;
                    ev.value = pts;
                }
            }
        }
    }
    ev
}

/// Output collected for the caller's callback, in batches.
struct CallbackSink<'a> {
    f: unsafe extern "C" fn(*mut c_void, *const u8, usize),
    user: *mut c_void,
    buf: &'a mut Vec<u8>,
}

/// Batch size for the packet callback: 348 packets.
const BATCH: usize = 188 * 348;

impl CallbackSink<'_> {
    fn flush(&mut self) {
        if !self.buf.is_empty() {
            // SAFETY: the caller passed a function and user pointer that
            // accept this call; the buffer is live for its duration.
            unsafe { (self.f)(self.user, self.buf.as_ptr(), self.buf.len()) };
            self.buf.clear();
        }
    }
}

impl PacketSink for CallbackSink<'_> {
    fn packet(&mut self, packet: &Packet) {
        self.buf.extend_from_slice(packet);
        if self.buf.len() >= BATCH {
            self.flush();
        }
    }
}

unsafe fn injector_mut<'a>(p: *mut mpegts_injector) -> Result<&'a mut mpegts_injector, i32> {
    let h = p.as_mut().ok_or(MPEGTS_ERR_NULL)?;
    if h.poisoned {
        return Err(MPEGTS_ERR_POISONED);
    }
    Ok(h)
}

unsafe fn injector_ref<'a>(p: *const mpegts_injector) -> Result<&'a mpegts_injector, i32> {
    let h = p.as_ref().ok_or(MPEGTS_ERR_NULL)?;
    if h.poisoned {
        return Err(MPEGTS_ERR_POISONED);
    }
    Ok(h)
}

/// Run `f` on an injector; a panic poisons it.
fn injector_guard(h: &mut mpegts_injector, f: impl FnOnce(&mut mpegts_injector) -> i32) -> i32 {
    match catch_unwind(AssertUnwindSafe(|| f(&mut *h))) {
        Ok(status) => status,
        Err(_) => {
            h.poisoned = true;
            h.out.clear();
            MPEGTS_ERR_PANIC
        }
    }
}

fn record(h: &mut mpegts_injector, r: Result<(), InjectError>) -> i32 {
    match r {
        Ok(()) => {
            h.error.clear();
            MPEGTS_OK
        }
        Err(e) => {
            h.error = e.to_string();
            inject_status(&e)
        }
    }
}

fn frame_struct(f: &Frame) -> mpegts_inject_frame {
    let mut flags = 0;
    if f.dts.is_some() {
        flags |= MPEGTS_FRAME_HAS_DTS;
    }
    if f.clock.is_some() {
        flags |= MPEGTS_FRAME_HAS_CLOCK;
    }
    if f.random_access {
        flags |= MPEGTS_FRAME_RANDOM_ACCESS;
    }
    if f.discontinuity {
        flags |= MPEGTS_FRAME_DISCONTINUITY;
    }
    mpegts_inject_frame {
        struct_size: size_of_u32::<mpegts_inject_frame>(),
        flags,
        pid: f.pid,
        reserved: 0,
        reserved2: 0,
        pts: f.pts,
        dts: f.dts.unwrap_or(0),
        clock: f.clock.unwrap_or(0),
    }
}

/// The caller's unit, honouring its struct_size.
unsafe fn read_inject_unit(unit: *const mpegts_inject_unit) -> Result<InsertUnit, i32> {
    let u = read_in(unit, mpegts_inject_unit::default())?;
    if u.flags & !MPEGTS_INJECT_UNIT_HAS_PTS != 0 {
        return Err(MPEGTS_ERR_INVALID);
    }
    let len = usize::try_from(u.data_len).map_err(|_| MPEGTS_ERR_INVALID)?;
    let data = input(u.data, len)?.to_vec();
    Ok(if u.flags & MPEGTS_INJECT_UNIT_HAS_PTS != 0 {
        InsertUnit::sync(u.pts, data)
    } else {
        InsertUnit::asynchronous(data)
    })
}

/// Push through a caller sink and frame callback.
unsafe fn push_with(
    injector: *mut mpegts_injector,
    sink: mpegts_packets_fn,
    sink_user: *mut c_void,
    on_frame: mpegts_frame_fn,
    frame_user: *mut c_void,
    f: impl FnOnce(
        &mut Injector,
        &mut CallbackSink<'_>,
        &mut dyn FnMut(&Frame, &mut Units<'_>),
    ) -> Result<(), InjectError>,
) -> mpegts_status {
    let h = check!(injector_mut(injector));
    let Some(sink) = sink else {
        return MPEGTS_ERR_NULL;
    };
    injector_guard(h, |h| {
        let mut out = std::mem::take(&mut h.out);
        let mut cb = |frame: &Frame, units: &mut Units<'_>| {
            if let Some(on_frame) = on_frame {
                let c = frame_struct(frame);
                let mut handle = mpegts_inject_units {
                    units: units as *mut Units<'_> as *mut c_void,
                };
                on_frame(frame_user, &c, &mut handle);
            }
        };
        let mut s = CallbackSink {
            f: sink,
            user: sink_user,
            buf: &mut out,
        };
        let r = f(&mut h.injector, &mut s, &mut cb);
        s.flush();
        h.out = out;
        record(h, r)
    })
}

// ---------------------------------------------------------------- injector

#[no_mangle]
pub unsafe extern "C" fn mpegts_inject_config_default(
    config: *mut mpegts_inject_config,
) -> mpegts_status {
    write_out(
        config,
        mpegts_inject_config::from_config(&InjectConfig::default()),
    )
}

#[no_mangle]
pub unsafe extern "C" fn mpegts_injector_new(
    config: *const mpegts_inject_config,
    out: *mut *mut mpegts_injector,
) -> mpegts_status {
    if out.is_null() {
        return MPEGTS_ERR_NULL;
    }
    out.write(ptr::null_mut());
    let cfg = if config.is_null() {
        InjectConfig::default()
    } else {
        let c = check!(read_in(
            config,
            mpegts_inject_config::from_config(&InjectConfig::default())
        ));
        check!(c.to_config().map_err(|_| MPEGTS_ERR_INVALID))
    };
    guard(|| match Injector::new(cfg) {
        Ok(injector) => {
            let h = Box::new(mpegts_injector {
                injector,
                out: Vec::new(),
                error: String::new(),
                poisoned: false,
            });
            out.write(Box::into_raw(h));
            MPEGTS_OK
        }
        Err(e) => inject_status(&e),
    })
}

#[no_mangle]
pub unsafe extern "C" fn mpegts_inject_config_check(
    config: *const mpegts_inject_config,
    message: *mut c_char,
    message_len: usize,
) -> mpegts_status {
    if message.is_null() && message_len > 0 {
        return MPEGTS_ERR_NULL;
    }
    let c = check!(read_in(
        config,
        mpegts_inject_config::from_config(&InjectConfig::default())
    ));
    guard(|| {
        let r = c
            .to_config()
            .map_err(|why| why.to_string())
            .and_then(|cfg| Injector::new(cfg).map(|_| ()).map_err(|e| e.to_string()));
        match r {
            Ok(()) => {
                write_message("", message, message_len);
                MPEGTS_OK
            }
            Err(why) => {
                write_message(&why, message, message_len);
                MPEGTS_ERR_INVALID
            }
        }
    })
}

#[no_mangle]
pub unsafe extern "C" fn mpegts_injector_free(injector: *mut mpegts_injector) {
    if !injector.is_null() {
        let _ = catch_unwind(AssertUnwindSafe(|| drop(Box::from_raw(injector))));
    }
}

#[no_mangle]
pub unsafe extern "C" fn mpegts_injector_error(
    injector: *const mpegts_injector,
    message: *mut c_char,
    message_len: usize,
) -> mpegts_status {
    let Some(h) = injector.as_ref() else {
        return MPEGTS_ERR_NULL;
    };
    if message.is_null() && message_len > 0 {
        return MPEGTS_ERR_NULL;
    }
    let msg = if h.poisoned {
        "internal error; free this object"
    } else {
        &h.error
    };
    write_message(msg, message, message_len);
    MPEGTS_OK
}

#[no_mangle]
pub unsafe extern "C" fn mpegts_injector_push(
    injector: *mut mpegts_injector,
    data: *const u8,
    len: usize,
    sink: mpegts_packets_fn,
    sink_user: *mut c_void,
    on_frame: mpegts_frame_fn,
    frame_user: *mut c_void,
) -> mpegts_status {
    let bytes = check!(input(data, len));
    push_with(
        injector,
        sink,
        sink_user,
        on_frame,
        frame_user,
        |inj, s, cb| inj.push_with(bytes, s, cb),
    )
}

#[no_mangle]
pub unsafe extern "C" fn mpegts_injector_push_packet(
    injector: *mut mpegts_injector,
    packet: *const u8,
    clock: *const u64,
    sink: mpegts_packets_fn,
    sink_user: *mut c_void,
    on_frame: mpegts_frame_fn,
    frame_user: *mut c_void,
) -> mpegts_status {
    if packet.is_null() {
        return MPEGTS_ERR_NULL;
    }
    let p: &Packet = &*(packet as *const Packet);
    let clock = clock.as_ref().copied();
    push_with(
        injector,
        sink,
        sink_user,
        on_frame,
        frame_user,
        |inj, s, cb| inj.push_packet_with(p, clock, s, cb),
    )
}

#[no_mangle]
pub unsafe extern "C" fn mpegts_injector_insert(
    injector: *mut mpegts_injector,
    unit: *const mpegts_inject_unit,
) -> mpegts_status {
    let h = check!(injector_mut(injector));
    let unit = check!(read_inject_unit(unit));
    injector_guard(h, |h| {
        let r = h.injector.insert(unit);
        record(h, r)
    })
}

#[no_mangle]
pub unsafe extern "C" fn mpegts_inject_units_insert(
    units: *mut mpegts_inject_units,
    unit: *const mpegts_inject_unit,
) -> mpegts_status {
    let Some(h) = units.as_mut() else {
        return MPEGTS_ERR_NULL;
    };
    let Some(u) = (h.units as *mut Units<'_>).as_mut() else {
        return MPEGTS_ERR_NULL;
    };
    let unit = check!(read_inject_unit(unit));
    // Runs inside the injector's own guarded call.
    match u.insert(unit) {
        Ok(()) => MPEGTS_OK,
        Err(e) => inject_status(&e),
    }
}

#[no_mangle]
pub unsafe extern "C" fn mpegts_inject_units_clock(
    units: *const mpegts_inject_units,
    clock: *mut u64,
) -> mpegts_status {
    let Some(h) = units.as_ref() else {
        return MPEGTS_ERR_NULL;
    };
    let Some(u) = (h.units as *const Units<'_>).as_ref() else {
        return MPEGTS_ERR_NULL;
    };
    if clock.is_null() {
        return MPEGTS_ERR_NULL;
    }
    match u.clock() {
        Some(c) => {
            clock.write(c);
            MPEGTS_OK
        }
        None => MPEGTS_EMPTY,
    }
}

#[no_mangle]
pub unsafe extern "C" fn mpegts_inject_units_target(
    units: *const mpegts_inject_units,
    target: *mut mpegts_inject_target,
) -> mpegts_status {
    let Some(h) = units.as_ref() else {
        return MPEGTS_ERR_NULL;
    };
    let Some(u) = (h.units as *const Units<'_>).as_ref() else {
        return MPEGTS_ERR_NULL;
    };
    if target.is_null() {
        return MPEGTS_ERR_NULL;
    }
    match u.target() {
        Some(t) => write_out(target, target_struct(t)),
        None => MPEGTS_EMPTY,
    }
}

#[no_mangle]
pub unsafe extern "C" fn mpegts_injector_end_input(
    injector: *mut mpegts_injector,
    sink: mpegts_packets_fn,
    sink_user: *mut c_void,
    on_frame: mpegts_frame_fn,
    frame_user: *mut c_void,
) -> mpegts_status {
    push_with(injector, sink, sink_user, on_frame, frame_user, |inj, s, cb| inj.end_input_with(s, cb))
}

#[no_mangle]
pub unsafe extern "C" fn mpegts_injector_finish(
    injector: *mut mpegts_injector,
    sink: mpegts_packets_fn,
    sink_user: *mut c_void,
    on_frame: mpegts_frame_fn,
    frame_user: *mut c_void,
    stats: *mut mpegts_inject_stats,
) -> mpegts_status {
    let status = push_with(
        injector,
        sink,
        sink_user,
        on_frame,
        frame_user,
        |inj, s, cb| inj.finish_with(s, cb).map(|_| ()),
    );
    if status == MPEGTS_OK && !stats.is_null() {
        return mpegts_injector_stats(injector, stats);
    }
    status
}

#[no_mangle]
pub unsafe extern "C" fn mpegts_injector_stats(
    injector: *const mpegts_injector,
    stats: *mut mpegts_inject_stats,
) -> mpegts_status {
    let h = check!(injector_ref(injector));
    write_out(stats, stats_struct(&h.injector.stats()))
}

#[no_mangle]
pub unsafe extern "C" fn mpegts_injector_target(
    injector: *const mpegts_injector,
    target: *mut mpegts_inject_target,
) -> mpegts_status {
    let h = check!(injector_ref(injector));
    if target.is_null() {
        return MPEGTS_ERR_NULL;
    }
    match h.injector.target() {
        Some(t) => write_out(target, target_struct(t)),
        None => MPEGTS_EMPTY,
    }
}

#[no_mangle]
pub unsafe extern "C" fn mpegts_injector_clock(
    injector: *const mpegts_injector,
    clock: *mut u64,
) -> mpegts_status {
    let h = check!(injector_ref(injector));
    if clock.is_null() {
        return MPEGTS_ERR_NULL;
    }
    match h.injector.clock() {
        Some(c) => {
            clock.write(c);
            MPEGTS_OK
        }
        None => MPEGTS_EMPTY,
    }
}

#[no_mangle]
pub unsafe extern "C" fn mpegts_injector_pop_event(
    injector: *mut mpegts_injector,
    event: *mut mpegts_inject_event,
) -> mpegts_status {
    let h = check!(injector_mut(injector));
    if event.is_null() {
        return MPEGTS_ERR_NULL;
    }
    // Checked before taking the event, so a short struct loses nothing.
    if (event as *const u32).read() < 4 {
        return MPEGTS_ERR_INVALID;
    }
    match h.injector.pop_event() {
        Some(e) => write_out(event, event_struct(&e)),
        None => MPEGTS_EMPTY,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sized<T: Default>() -> T {
        let mut v = T::default();
        // SAFETY: every ABI struct starts with a u32 struct_size.
        unsafe { (ptr::addr_of_mut!(v) as *mut u32).write(mem::size_of::<T>() as u32) };
        v
    }

    unsafe extern "C" fn collect(user: *mut c_void, packets: *const u8, len: usize) {
        let out = &mut *(user as *mut Vec<u8>);
        out.extend_from_slice(slice::from_raw_parts(packets, len));
    }

    unsafe extern "C" fn per_frame(
        user: *mut c_void,
        frame: *const mpegts_inject_frame,
        units: *mut mpegts_inject_units,
    ) {
        let count = &mut *(user as *mut u32);
        *count += 1;
        let data = [0x06u8, 0x0E, 0x2B, 0x34, 0x01, 0xAA];
        let mut unit: mpegts_inject_unit = sized();
        unit.flags = MPEGTS_INJECT_UNIT_HAS_PTS;
        unit.pts = (*frame).pts;
        unit.data = data.as_ptr();
        unit.data_len = data.len() as u64;
        assert_eq!(mpegts_inject_units_insert(units, &unit), MPEGTS_OK);
        let mut t: mpegts_inject_target = sized();
        assert_eq!(mpegts_inject_units_target(units, &mut t), MPEGTS_OK);
        assert_eq!(t.klv_pid, 0x101);
    }

    fn stream() -> Vec<u8> {
        let mut t = crate::mux::ProgramTables::new(1);
        t.add_program(1, 0x1000, Some(0x100)).unwrap();
        t.add_stream(1, 0x100, &crate::mux::StreamSpec::h264())
            .unwrap();
        let mut v = crate::mux::PesWriter::new(0x100, &crate::mux::StreamSpec::h264()).unwrap();
        let mut ts = Vec::new();
        for i in 0..20u64 {
            t.write_all(&mut ts).unwrap();
            let pts = 90_000 + i * 3003;
            let unit = crate::mux::PesUnit::new(&[0u8; 400])
                .with_pts(pts)
                .with_pcr((pts - 9000) * 300);
            v.write(&unit, &mut ts).unwrap();
        }
        ts
    }

    #[test]
    fn inject_through_the_c_abi() {
        unsafe {
            let mut cfg: mpegts_inject_config = sized();
            assert_eq!(mpegts_inject_config_default(&mut cfg), MPEGTS_OK);
            assert_eq!(
                (cfg.psi_interval_ms, cfg.max_lead_ms, cfg.carriage),
                (100, 990, MPEGTS_INJECT_SYNC)
            );
            let mut inj = ptr::null_mut();
            assert_eq!(mpegts_injector_new(&cfg, &mut inj), MPEGTS_OK);
            let ts = stream();
            let mut out: Vec<u8> = Vec::new();
            let mut frames = 0u32;
            let user = &mut out as *mut Vec<u8> as *mut c_void;
            let status = mpegts_injector_push(
                inj,
                ts.as_ptr(),
                ts.len(),
                Some(collect),
                user,
                Some(per_frame),
                &mut frames as *mut u32 as *mut c_void,
            );
            assert_eq!(status, MPEGTS_OK);
            let mut stats: mpegts_inject_stats = sized();
            let frame_user = &mut frames as *mut u32 as *mut c_void;
            assert_eq!(
                mpegts_injector_finish(
                    inj,
                    Some(collect),
                    user,
                    Some(per_frame),
                    frame_user,
                    &mut stats
                ),
                MPEGTS_OK
            );
            assert_eq!(frames, 20);
            assert_eq!(stats.units_inserted, 20);
            assert_eq!(stats.packets_out as usize, out.len() / 188);
            let mut t: mpegts_inject_target = sized();
            assert_eq!(mpegts_injector_target(inj, &mut t), MPEGTS_OK);
            assert_eq!(
                (t.program, t.klv_pid, t.service_id, t.pcr_pid),
                (1, 0x101, 0, 0x100)
            );
            let mut ev: mpegts_inject_event = sized();
            assert_eq!(mpegts_injector_pop_event(inj, &mut ev), MPEGTS_OK);
            assert_eq!((ev.kind, ev.value), (MPEGTS_EVENT_TARGET_FOUND, 1));
            assert_eq!(mpegts_injector_pop_event(inj, &mut ev), MPEGTS_EMPTY);
            // Finished: further calls are refused with a reason.
            assert_eq!(
                mpegts_injector_push(
                    inj,
                    ts.as_ptr(),
                    188,
                    Some(collect),
                    user,
                    None,
                    ptr::null_mut()
                ),
                MPEGTS_ERR_INVALID
            );
            let mut buf = [0 as c_char; 128];
            assert_eq!(
                mpegts_injector_error(inj, buf.as_mut_ptr(), buf.len()),
                MPEGTS_OK
            );
            assert!(std::ffi::CStr::from_ptr(buf.as_ptr())
                .to_bytes()
                .starts_with(b"the injector is finished"));
            mpegts_injector_free(inj);

            let report = crate::analyze(&out);
            assert_eq!(report.stream(0x101).unwrap().codec, crate::Codec::KlvSync);
        }
    }

    #[test]
    fn statuses_and_null_arguments() {
        unsafe {
            assert_eq!(
                mpegts_injector_new(ptr::null(), ptr::null_mut()),
                MPEGTS_ERR_NULL
            );
            let mut cfg: mpegts_inject_config = sized();
            mpegts_inject_config_default(&mut cfg);
            cfg.rate = 9;
            let mut inj = ptr::null_mut();
            assert_eq!(mpegts_injector_new(&cfg, &mut inj), MPEGTS_ERR_INVALID);
            assert!(inj.is_null());
            let mut msg = [0 as c_char; 128];
            assert_eq!(
                mpegts_inject_config_check(&cfg, msg.as_mut_ptr(), msg.len()),
                MPEGTS_ERR_INVALID
            );
            cfg.rate = MPEGTS_RATE_VBR;
            cfg.psi_interval_ms = 300;
            assert_eq!(
                mpegts_inject_config_check(&cfg, msg.as_mut_ptr(), msg.len()),
                MPEGTS_ERR_INVALID
            );
            assert!(std::ffi::CStr::from_ptr(msg.as_ptr())
                .to_bytes()
                .starts_with(b"invalid injector configuration"));
            assert_eq!(mpegts_injector_new(ptr::null(), &mut inj), MPEGTS_OK);
            assert_eq!(
                mpegts_injector_push(
                    inj,
                    ptr::null(),
                    0,
                    None,
                    ptr::null_mut(),
                    None,
                    ptr::null_mut()
                ),
                MPEGTS_ERR_NULL
            );
            let mut unit: mpegts_inject_unit = sized();
            unit.data_len = 3;
            assert_eq!(mpegts_injector_insert(inj, &unit), MPEGTS_ERR_NULL);
            let data = [1u8, 2, 3];
            unit.data = data.as_ptr();
            assert_eq!(
                mpegts_injector_insert(inj, &unit),
                MPEGTS_ERR_INVALID,
                "sync needs a PTS"
            );
            unit.flags = MPEGTS_INJECT_UNIT_HAS_PTS;
            let mut cfg2: mpegts_inject_config = sized();
            mpegts_inject_config_default(&mut cfg2);
            cfg2.max_queued_units = 1;
            let mut small = ptr::null_mut();
            assert_eq!(mpegts_injector_new(&cfg2, &mut small), MPEGTS_OK);
            assert_eq!(mpegts_injector_insert(small, &unit), MPEGTS_OK);
            assert_eq!(mpegts_injector_insert(small, &unit), MPEGTS_ERR_FULL);
            // No PAT anywhere: a stream error at the end.
            assert_eq!(
                mpegts_injector_finish(
                    small,
                    Some(collect),
                    &mut Vec::<u8>::new() as *mut Vec<u8> as *mut c_void,
                    None,
                    ptr::null_mut(),
                    ptr::null_mut()
                ),
                MPEGTS_ERR_STREAM
            );
            assert_eq!(
                mpegts_injector_insert(small, &unit),
                MPEGTS_ERR_STREAM,
                "the error is final"
            );
            let mut t: mpegts_inject_target = sized();
            assert_eq!(mpegts_injector_target(inj, &mut t), MPEGTS_EMPTY);
            let mut clock = 0u64;
            assert_eq!(mpegts_injector_clock(inj, &mut clock), MPEGTS_EMPTY);
            assert_eq!(mpegts_injector_clock(inj, ptr::null_mut()), MPEGTS_ERR_NULL);
            assert_eq!(
                mpegts_inject_units_insert(ptr::null_mut(), &unit),
                MPEGTS_ERR_NULL
            );
            mpegts_injector_free(small);
            mpegts_injector_free(inj);
            mpegts_injector_free(ptr::null_mut());
        }
    }
}
