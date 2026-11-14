//! Stable C ABI. The contract is `include/mpegts.h`; read its header
//! comment for the rules every function follows.
#![allow(unsafe_code, non_camel_case_types, clippy::missing_safety_doc)]

use crate::{
    Codec, Config, Demuxer, Output, Overflow, Report, Select, StreamKind, Subscription,
    SubscriptionId, Unit, PACKET_SIZE,
};
use std::ffi::CString;
use std::os::raw::c_char;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::OnceLock;
use std::{mem, ptr, slice};

pub const MPEGTS_ABI_VERSION: u32 = 1;

pub type mpegts_status = i32;
pub const MPEGTS_OK: i32 = 0;
pub const MPEGTS_EMPTY: i32 = 1;
pub const MPEGTS_ERR_NULL: i32 = -1;
pub const MPEGTS_ERR_INVALID: i32 = -2;
pub const MPEGTS_ERR_RANGE: i32 = -3;
pub const MPEGTS_ERR_NOT_FOUND: i32 = -4;
pub const MPEGTS_ERR_PANIC: i32 = -5;
pub const MPEGTS_ERR_POISONED: i32 = -6;

const OUTPUT_HAS_STREAM_ID: u32 = 0x01;
const OUTPUT_HAS_TABLE_ID: u32 = 0x02;
const OUTPUT_HAS_PTS: u32 = 0x04;
const OUTPUT_HAS_DTS: u32 = 0x08;
const OUTPUT_CRC_OK: u32 = 0x10;
const OUTPUT_CRC_BAD: u32 = 0x20;
const OUTPUT_DAMAGED: u32 = 0x40;
const OUTPUT_TRUNCATED: u32 = 0x80;

// ---------------------------------------------------------------- structs

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct mpegts_config {
    pub struct_size: u32,
    pub sync_confirmations: u32,
    pub probe_pes: u32,
    pub overflow: u32,
    pub max_streams: u64,
    pub history_packets: u64,
    pub queue_max_items: u64,
    pub queue_max_bytes: u64,
    pub max_pes_bytes: u64,
    pub max_reassembly_bytes: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct mpegts_subscription {
    pub struct_size: u32,
    pub select: u32,
    pub value: u32,
    pub unit: u32,
    pub replay: u32,
    pub strip_metadata_au: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct mpegts_output {
    pub struct_size: u32,
    pub subscription: u32,
    pub unit: u32,
    pub flags: u32,
    pub pid: u16,
    pub stream_id: u8,
    pub table_id: u8,
    pub reserved: u32,
    pub packet: u64,
    pub pts: u64,
    pub dts: u64,
    pub data: *const u8,
    pub data_len: u64,
    pub payload_offset: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct mpegts_output_stats {
    pub struct_size: u32,
    pub subscriptions: u32,
    pub queued: u64,
    pub dropped: u64,
    pub truncated: u64,
    pub damaged: u64,
    pub pending: u64,
    pub pending_bytes: u64,
    pub reassembly_bytes: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct mpegts_report_summary {
    pub struct_size: u32,
    pub flags: u32,
    pub revision: u64,
    pub transport_stream_id: u16,
    pub network_pid: u16,
    pub pat_version: u8,
    pub reserved: [u8; 3],
    pub packet_size: u32,
    pub program_count: u32,
    pub pid_count: u32,
    pub ca_system_count: u32,
    pub packets: u64,
    pub bytes_skipped: u64,
    pub sync_losses: u64,
    pub crc_errors: u64,
    pub streams_dropped: u64,
    pub duration_ns: u64,
    pub bitrate: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct mpegts_program_info {
    pub struct_size: u32,
    pub flags: u32,
    pub number: u16,
    pub pmt_pid: u16,
    pub pcr_pid: u16,
    pub version: u8,
    pub reserved: u8,
    pub registration: [u8; 4],
    pub stream_count: u32,
    pub descriptor_count: u32,
    pub reserved2: u32,
    pub first_pcr: u64,
    pub last_pcr: u64,
    pub pcr_span: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct mpegts_stream_info {
    pub struct_size: u32,
    pub flags: u32,
    pub program: u16,
    pub pid: u16,
    pub stream_type: u8,
    pub pes_stream_id: u8,
    pub reserved: u16,
    pub codec: u32,
    pub kind: u32,
    pub basis: u32,
    pub language: [c_char; 4],
    pub registration: [u8; 4],
    pub metadata_format: [u8; 4],
    pub descriptor_count: u32,
    pub reserved2: u32,
    pub first_pts: u64,
    pub last_pts: u64,
    pub last_dts: u64,
    pub pts_span: u64,
    pub packets: u64,
    pub cc_errors: u64,
    pub scrambled_packets: u64,
    pub last_packet: u64,
    pub bitrate: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct mpegts_pid_info {
    pub struct_size: u32,
    pub pid: u16,
    pub reserved: u16,
    pub packets: u64,
    pub cc_errors: u64,
    pub transport_errors: u64,
    pub scrambled_packets: u64,
    pub last_packet: u64,
}

/// Opaque demuxer handle.
#[derive(Debug)]
pub struct mpegts_demuxer {
    demux: Demuxer,
    /// The output last handed out; its buffer backs `mpegts_output.data`.
    current: Option<Output>,
    /// A panic left the demuxer in an unknown state.
    poisoned: bool,
}

/// Opaque report handle.
#[derive(Debug)]
pub struct mpegts_report {
    report: Report,
}

// ---------------------------------------------------------------- helpers

macro_rules! check {
    ($e:expr) => {
        match $e {
            Ok(v) => v,
            Err(status) => return status,
        }
    };
}

/// Run `f`, turning a panic into a status code.
fn guard(f: impl FnOnce() -> i32) -> i32 {
    catch_unwind(AssertUnwindSafe(f)).unwrap_or(MPEGTS_ERR_PANIC)
}

/// Run `f` on a demuxer; a panic poisons it.
fn guard_mut(h: &mut mpegts_demuxer, f: impl FnOnce(&mut mpegts_demuxer) -> i32) -> i32 {
    match catch_unwind(AssertUnwindSafe(|| f(&mut *h))) {
        Ok(status) => status,
        Err(_) => {
            h.poisoned = true;
            MPEGTS_ERR_PANIC
        }
    }
}

unsafe fn handle_mut<'a>(d: *mut mpegts_demuxer) -> Result<&'a mut mpegts_demuxer, i32> {
    let h = d.as_mut().ok_or(MPEGTS_ERR_NULL)?;
    if h.poisoned {
        return Err(MPEGTS_ERR_POISONED);
    }
    Ok(h)
}

unsafe fn handle<'a>(d: *const mpegts_demuxer) -> Result<&'a mpegts_demuxer, i32> {
    let h = d.as_ref().ok_or(MPEGTS_ERR_NULL)?;
    if h.poisoned {
        return Err(MPEGTS_ERR_POISONED);
    }
    Ok(h)
}

unsafe fn report_ref<'a>(r: *const mpegts_report) -> Result<&'a Report, i32> {
    r.as_ref().map(|r| &r.report).ok_or(MPEGTS_ERR_NULL)
}

/// Read a caller's struct, honouring its `struct_size`: fields the caller
/// does not know about keep the values in `base`.
unsafe fn read_in<T: Copy>(p: *const T, base: T) -> Result<T, i32> {
    if p.is_null() {
        return Err(MPEGTS_ERR_NULL);
    }
    let size = (p as *const u32).read() as usize;
    if size < mem::size_of::<u32>() {
        return Err(MPEGTS_ERR_INVALID);
    }
    let mut v = base;
    ptr::copy_nonoverlapping(p as *const u8, ptr::addr_of_mut!(v) as *mut u8, size.min(mem::size_of::<T>()));
    Ok(v)
}

/// Write `v` into a caller's struct, no further than its `struct_size`,
/// and set `struct_size` to the bytes written.
unsafe fn write_out<T: Copy>(p: *mut T, v: T) -> i32 {
    if p.is_null() {
        return MPEGTS_ERR_NULL;
    }
    let size = (p as *const u32).read() as usize;
    if size < mem::size_of::<u32>() {
        return MPEGTS_ERR_INVALID;
    }
    let n = size.min(mem::size_of::<T>());
    ptr::copy_nonoverlapping(ptr::addr_of!(v) as *const u8, p as *mut u8, n);
    (p as *mut u32).write(n as u32);
    MPEGTS_OK
}

fn size_of_u32<T>() -> u32 {
    mem::size_of::<T>() as u32
}

fn index(i: u32, len: usize) -> Result<usize, i32> {
    let i = i as usize;
    if i < len {
        Ok(i)
    } else {
        Err(MPEGTS_ERR_RANGE)
    }
}

impl From<&Config> for mpegts_config {
    fn from(c: &Config) -> Self {
        mpegts_config {
            struct_size: size_of_u32::<mpegts_config>(),
            sync_confirmations: c.sync_confirmations as u32,
            probe_pes: c.probe_pes,
            overflow: match c.overflow {
                Overflow::DropOldest => 1,
                _ => 0,
            },
            max_streams: c.max_streams as u64,
            history_packets: c.history_packets as u64,
            queue_max_items: c.queue_max_items as u64,
            queue_max_bytes: c.queue_max_bytes as u64,
            max_pes_bytes: c.max_pes_bytes as u64,
            max_reassembly_bytes: c.max_reassembly_bytes as u64,
        }
    }
}

impl mpegts_config {
    /// The Rust config, or why this one is invalid.
    fn to_config(self) -> Result<Config, String> {
        let size = |field: &str, v: u64| usize::try_from(v).map_err(|_| format!("invalid config: {field} is too large"));
        let mut c = Config::default();
        c.sync_confirmations = self.sync_confirmations as usize;
        c.probe_pes = self.probe_pes;
        c.overflow = match self.overflow {
            0 => Overflow::DropNewest,
            1 => Overflow::DropOldest,
            _ => return Err("invalid config: overflow must be an MPEGTS_OVERFLOW_* value".into()),
        };
        c.max_streams = size("max_streams", self.max_streams)?;
        c.history_packets = size("history_packets", self.history_packets)?;
        c.queue_max_items = size("queue_max_items", self.queue_max_items)?;
        c.queue_max_bytes = size("queue_max_bytes", self.queue_max_bytes)?;
        c.max_pes_bytes = size("max_pes_bytes", self.max_pes_bytes)?;
        c.max_reassembly_bytes = size("max_reassembly_bytes", self.max_reassembly_bytes)?;
        c.validate().map_err(|e| e.to_string())?;
        Ok(c)
    }
}

/// Copy `msg` into a caller's buffer as a NUL-terminated string, cut to
/// fit. Nothing is written when `buf` is NULL or `len` is 0.
unsafe fn write_message(msg: &str, buf: *mut c_char, len: usize) {
    if buf.is_null() || len == 0 {
        return;
    }
    let n = msg.len().min(len - 1);
    ptr::copy_nonoverlapping(msg.as_ptr() as *const c_char, buf, n);
    buf.add(n).write(0);
}

/// The caller's bytes as a slice: `data` may be NULL only when `len` is 0.
unsafe fn input<'a>(data: *const u8, len: usize) -> Result<&'a [u8], i32> {
    if len > isize::MAX as usize {
        return Err(MPEGTS_ERR_INVALID);
    }
    match (data.is_null(), len) {
        (_, 0) => Ok(&[]),
        (true, _) => Err(MPEGTS_ERR_NULL),
        (false, _) => Ok(slice::from_raw_parts(data, len)),
    }
}

impl mpegts_subscription {
    fn to_subscription(self) -> Result<Subscription, i32> {
        let v = self.value;
        let select = match self.select {
            1 if v <= 0x1FFF => Select::Pid(v as u16),
            2 if v <= 0xFFFF => Select::Program(v as u16),
            3 => Select::Codec(Codec::from_code(v).ok_or(MPEGTS_ERR_INVALID)?),
            4 => Select::Kind(StreamKind::from_code(v).ok_or(MPEGTS_ERR_INVALID)?),
            5 => Select::Psi,
            6 => Select::All,
            _ => return Err(MPEGTS_ERR_INVALID),
        };
        let unit = match self.unit {
            1 => Unit::Packets,
            2 => Unit::Pes,
            3 => Unit::Sections,
            _ => return Err(MPEGTS_ERR_INVALID),
        };
        let mut sub = Subscription::new(select, unit);
        sub.replay = self.replay != 0;
        sub.strip_metadata_au = self.strip_metadata_au != 0;
        Ok(sub)
    }
}

fn output_struct(o: &Output) -> mpegts_output {
    let mut flags = 0;
    let mut set = |cond: bool, f: u32| {
        if cond {
            flags |= f;
        }
    };
    set(o.stream_id.is_some(), OUTPUT_HAS_STREAM_ID);
    set(o.table_id.is_some(), OUTPUT_HAS_TABLE_ID);
    set(o.pts.is_some(), OUTPUT_HAS_PTS);
    set(o.dts.is_some(), OUTPUT_HAS_DTS);
    set(o.crc_ok == Some(true), OUTPUT_CRC_OK);
    set(o.crc_ok == Some(false), OUTPUT_CRC_BAD);
    set(o.damaged, OUTPUT_DAMAGED);
    set(o.truncated, OUTPUT_TRUNCATED);
    mpegts_output {
        struct_size: size_of_u32::<mpegts_output>(),
        subscription: o.subscription.get(),
        unit: o.unit as u32,
        flags,
        pid: o.pid,
        stream_id: o.stream_id.unwrap_or(0),
        table_id: o.table_id.unwrap_or(0),
        reserved: 0,
        packet: o.packet,
        pts: o.pts.unwrap_or(0),
        dts: o.dts.unwrap_or(0),
        data: if o.data.is_empty() { ptr::null() } else { o.data.as_ptr() },
        data_len: o.data.len() as u64,
        payload_offset: o.payload_offset as u64,
    }
}

/// NUL-terminated copies of names, made once.
fn c_names() -> &'static (Vec<(u32, CString)>, Vec<(u32, CString)>) {
    static NAMES: OnceLock<(Vec<(u32, CString)>, Vec<(u32, CString)>)> = OnceLock::new();
    NAMES.get_or_init(|| {
        let codecs = Codec::ALL.iter().map(|c| (c.code(), CString::new(c.name()).unwrap())).collect();
        let kinds = StreamKind::ALL.iter().map(|k| (k.code(), CString::new(k.name()).unwrap())).collect();
        (codecs, kinds)
    })
}

// ---------------------------------------------------------------- library

#[no_mangle]
pub extern "C" fn mpegts_abi_version() -> u32 {
    MPEGTS_ABI_VERSION
}

#[no_mangle]
pub extern "C" fn mpegts_version() -> *const c_char {
    concat!(env!("CARGO_PKG_VERSION"), "\0").as_ptr() as *const c_char
}

#[no_mangle]
pub extern "C" fn mpegts_status_string(status: mpegts_status) -> *const c_char {
    let s: &'static str = match status {
        MPEGTS_OK => "ok\0",
        MPEGTS_EMPTY => "empty\0",
        MPEGTS_ERR_NULL => "null pointer\0",
        MPEGTS_ERR_INVALID => "invalid argument\0",
        MPEGTS_ERR_RANGE => "index out of range\0",
        MPEGTS_ERR_NOT_FOUND => "not found\0",
        MPEGTS_ERR_PANIC => "internal error\0",
        MPEGTS_ERR_POISONED => "demuxer unusable after an internal error\0",
        _ => "unknown status\0",
    };
    s.as_ptr() as *const c_char
}

#[no_mangle]
pub extern "C" fn mpegts_codec_name(codec: u32) -> *const c_char {
    let found = catch_unwind(|| c_names().0.iter().find(|(c, _)| *c == codec).map(|(_, n)| n.as_ptr()));
    found.ok().flatten().unwrap_or(ptr::null())
}

#[no_mangle]
pub extern "C" fn mpegts_codec_kind(codec: u32) -> u32 {
    Codec::from_code(codec).map_or(0, |c| c.kind().code())
}

#[no_mangle]
pub extern "C" fn mpegts_kind_name(kind: u32) -> *const c_char {
    let found = catch_unwind(|| c_names().1.iter().find(|(k, _)| *k == kind).map(|(_, n)| n.as_ptr()));
    found.ok().flatten().unwrap_or(ptr::null())
}

// ---------------------------------------------------------------- demuxer

#[no_mangle]
pub unsafe extern "C" fn mpegts_config_default(config: *mut mpegts_config) -> mpegts_status {
    write_out(config, mpegts_config::from(&Config::default()))
}

#[no_mangle]
pub unsafe extern "C" fn mpegts_config_check(
    config: *const mpegts_config,
    message: *mut c_char,
    message_len: usize,
) -> mpegts_status {
    let c = match read_in(config, mpegts_config::from(&Config::default())) {
        Ok(c) => c,
        Err(status) => {
            if status == MPEGTS_ERR_INVALID {
                write_message("invalid config: struct_size must be at least 4", message, message_len);
            }
            return status;
        }
    };
    guard(|| match c.to_config() {
        Ok(_) => {
            write_message("", message, message_len);
            MPEGTS_OK
        }
        Err(why) => {
            write_message(&why, message, message_len);
            MPEGTS_ERR_INVALID
        }
    })
}

#[no_mangle]
pub unsafe extern "C" fn mpegts_demuxer_new(
    config: *const mpegts_config,
    out: *mut *mut mpegts_demuxer,
) -> mpegts_status {
    if out.is_null() {
        return MPEGTS_ERR_NULL;
    }
    out.write(ptr::null_mut());
    let config = if config.is_null() {
        Config::default()
    } else {
        let c = check!(read_in(config, mpegts_config::from(&Config::default())));
        check!(c.to_config().map_err(|_| MPEGTS_ERR_INVALID))
    };
    guard(|| match Demuxer::with_config(config) {
        Ok(demux) => {
            let h = Box::new(mpegts_demuxer { demux, current: None, poisoned: false });
            out.write(Box::into_raw(h));
            MPEGTS_OK
        }
        Err(_) => MPEGTS_ERR_INVALID,
    })
}

#[no_mangle]
pub unsafe extern "C" fn mpegts_demuxer_free(demuxer: *mut mpegts_demuxer) {
    if !demuxer.is_null() {
        let _ = catch_unwind(AssertUnwindSafe(|| drop(Box::from_raw(demuxer))));
    }
}

#[no_mangle]
pub unsafe extern "C" fn mpegts_demuxer_push(
    demuxer: *mut mpegts_demuxer,
    data: *const u8,
    len: usize,
) -> mpegts_status {
    let h = check!(handle_mut(demuxer));
    let bytes = check!(input(data, len));
    guard_mut(h, |h| {
        h.demux.push(bytes);
        MPEGTS_OK
    })
}

#[no_mangle]
pub unsafe extern "C" fn mpegts_demuxer_finish(demuxer: *mut mpegts_demuxer) -> mpegts_status {
    let h = check!(handle_mut(demuxer));
    guard_mut(h, |h| {
        h.demux.finish();
        MPEGTS_OK
    })
}

#[no_mangle]
pub unsafe extern "C" fn mpegts_demuxer_revision(demuxer: *const mpegts_demuxer, out: *mut u64) -> mpegts_status {
    let h = check!(handle(demuxer));
    if out.is_null() {
        return MPEGTS_ERR_NULL;
    }
    out.write(h.demux.revision());
    MPEGTS_OK
}

#[no_mangle]
pub unsafe extern "C" fn mpegts_demuxer_subscribe(
    demuxer: *mut mpegts_demuxer,
    sub: *const mpegts_subscription,
    id: *mut u32,
) -> mpegts_status {
    let h = check!(handle_mut(demuxer));
    if id.is_null() {
        return MPEGTS_ERR_NULL;
    }
    let sub = check!(check!(read_in(sub, mpegts_subscription::default())).to_subscription());
    guard_mut(h, |h| match h.demux.subscribe(sub) {
        Ok(new) => {
            id.write(new.get());
            MPEGTS_OK
        }
        Err(_) => MPEGTS_ERR_INVALID,
    })
}

#[no_mangle]
pub unsafe extern "C" fn mpegts_demuxer_unsubscribe(demuxer: *mut mpegts_demuxer, id: u32) -> mpegts_status {
    let h = check!(handle_mut(demuxer));
    guard_mut(h, |h| {
        if h.demux.unsubscribe(SubscriptionId(id)) {
            MPEGTS_OK
        } else {
            MPEGTS_ERR_NOT_FOUND
        }
    })
}

#[no_mangle]
pub unsafe extern "C" fn mpegts_demuxer_next_output(
    demuxer: *mut mpegts_demuxer,
    out: *mut mpegts_output,
) -> mpegts_status {
    let h = check!(handle_mut(demuxer));
    if out.is_null() {
        return MPEGTS_ERR_NULL;
    }
    if (out as *const u32).read() < size_of_u32::<u32>() {
        return MPEGTS_ERR_INVALID;
    }
    guard_mut(h, |h| {
        if let Some(previous) = h.current.take() {
            h.demux.recycle(previous);
        }
        h.current = h.demux.pop_output();
        match &h.current {
            Some(o) => write_out(out, output_struct(o)),
            None => MPEGTS_EMPTY,
        }
    })
}

#[no_mangle]
pub unsafe extern "C" fn mpegts_demuxer_output_stats(
    demuxer: *const mpegts_demuxer,
    out: *mut mpegts_output_stats,
) -> mpegts_status {
    let h = check!(handle(demuxer));
    let s = h.demux.output_stats();
    write_out(
        out,
        mpegts_output_stats {
            struct_size: size_of_u32::<mpegts_output_stats>(),
            subscriptions: s.subscriptions as u32,
            queued: s.queued,
            dropped: s.dropped,
            truncated: s.truncated,
            damaged: s.damaged,
            pending: s.pending as u64,
            pending_bytes: s.pending_bytes as u64,
            reassembly_bytes: s.reassembly_bytes as u64,
        },
    )
}

#[no_mangle]
pub unsafe extern "C" fn mpegts_demuxer_history_len(demuxer: *const mpegts_demuxer, out: *mut u64) -> mpegts_status {
    let h = check!(handle(demuxer));
    if out.is_null() {
        return MPEGTS_ERR_NULL;
    }
    out.write(h.demux.history_len() as u64);
    MPEGTS_OK
}

#[no_mangle]
pub unsafe extern "C" fn mpegts_demuxer_history_get(
    demuxer: *const mpegts_demuxer,
    index: u64,
    packet: *mut u64,
    data: *mut *const u8,
) -> mpegts_status {
    let h = check!(handle(demuxer));
    if packet.is_null() || data.is_null() {
        return MPEGTS_ERR_NULL;
    }
    let Some((n, bytes)) = usize::try_from(index).ok().and_then(|i| h.demux.history_get(i)) else {
        return MPEGTS_ERR_RANGE;
    };
    packet.write(n);
    data.write(bytes.as_ptr());
    debug_assert_eq!(bytes.len(), PACKET_SIZE);
    MPEGTS_OK
}

// ---------------------------------------------------------------- reports

#[no_mangle]
pub unsafe extern "C" fn mpegts_analyze(data: *const u8, len: usize, out: *mut *mut mpegts_report) -> mpegts_status {
    if out.is_null() {
        return MPEGTS_ERR_NULL;
    }
    out.write(ptr::null_mut());
    let bytes = check!(input(data, len));
    guard(|| {
        let report = Box::new(mpegts_report { report: crate::analyze(bytes) });
        out.write(Box::into_raw(report));
        MPEGTS_OK
    })
}

#[no_mangle]
pub unsafe extern "C" fn mpegts_demuxer_report(
    demuxer: *const mpegts_demuxer,
    out: *mut *mut mpegts_report,
) -> mpegts_status {
    if out.is_null() {
        return MPEGTS_ERR_NULL;
    }
    out.write(ptr::null_mut());
    let h = check!(handle(demuxer));
    guard(|| {
        let report = Box::new(mpegts_report { report: h.demux.report() });
        out.write(Box::into_raw(report));
        MPEGTS_OK
    })
}

#[no_mangle]
pub unsafe extern "C" fn mpegts_report_free(report: *mut mpegts_report) {
    if !report.is_null() {
        let _ = catch_unwind(AssertUnwindSafe(|| drop(Box::from_raw(report))));
    }
}

#[no_mangle]
pub unsafe extern "C" fn mpegts_report_summary_get(
    report: *const mpegts_report,
    out: *mut mpegts_report_summary,
) -> mpegts_status {
    let r = check!(report_ref(report));
    let s = &r.stats;
    let mut v = mpegts_report_summary {
        struct_size: size_of_u32::<mpegts_report_summary>(),
        revision: r.revision,
        program_count: r.programs.len() as u32,
        pid_count: r.pids.len() as u32,
        ca_system_count: r.ca_systems.len() as u32,
        packets: s.packets,
        bytes_skipped: s.bytes_skipped,
        sync_losses: s.sync_losses,
        crc_errors: s.crc_errors,
        streams_dropped: s.streams_dropped,
        ..Default::default()
    };
    if let Some(id) = r.transport_stream_id {
        v.flags |= 0x01;
        v.transport_stream_id = id;
    }
    if let Some(ver) = r.pat_version {
        v.flags |= 0x02;
        v.pat_version = ver;
    }
    if let Some(pid) = r.network_pid {
        v.flags |= 0x04;
        v.network_pid = pid;
    }
    if let Some(size) = s.packet_size {
        v.flags |= 0x08;
        v.packet_size = size as u32;
    }
    if let Some(d) = r.duration() {
        v.flags |= 0x10;
        v.duration_ns = u64::try_from(d.as_nanos()).unwrap_or(u64::MAX);
    }
    if let Some(b) = r.bitrate() {
        v.flags |= 0x20;
        v.bitrate = b;
    }
    write_out(out, v)
}

#[no_mangle]
pub unsafe extern "C" fn mpegts_report_program(
    report: *const mpegts_report,
    program_index: u32,
    out: *mut mpegts_program_info,
) -> mpegts_status {
    let r = check!(report_ref(report));
    let p = &r.programs[check!(index(program_index, r.programs.len()))];
    let mut v = mpegts_program_info {
        struct_size: size_of_u32::<mpegts_program_info>(),
        number: p.number,
        pmt_pid: p.pmt_pid,
        stream_count: p.streams.len() as u32,
        descriptor_count: p.descriptors.len() as u32,
        ..Default::default()
    };
    if let Some(ver) = p.version {
        v.flags |= 0x01;
        v.version = ver;
    }
    if let Some(pid) = p.pcr_pid {
        v.flags |= 0x02;
        v.pcr_pid = pid;
    }
    if let Some(reg) = p.registration {
        v.flags |= 0x04;
        v.registration = reg.0;
    }
    if let Some(t) = p.first_pcr {
        v.flags |= 0x08;
        v.first_pcr = t;
    }
    if let Some(t) = p.last_pcr {
        v.flags |= 0x10;
        v.last_pcr = t;
    }
    if let Some(t) = p.pcr_span {
        v.flags |= 0x20;
        v.pcr_span = t;
    }
    write_out(out, v)
}

#[no_mangle]
pub unsafe extern "C" fn mpegts_report_stream(
    report: *const mpegts_report,
    program_index: u32,
    stream_index: u32,
    out: *mut mpegts_stream_info,
) -> mpegts_status {
    let r = check!(report_ref(report));
    let p = &r.programs[check!(index(program_index, r.programs.len()))];
    let s = &p.streams[check!(index(stream_index, p.streams.len()))];
    let mut v = mpegts_stream_info {
        struct_size: size_of_u32::<mpegts_stream_info>(),
        program: s.program,
        pid: s.pid,
        stream_type: s.stream_type,
        codec: s.codec.code(),
        kind: s.kind().code(),
        basis: s.basis.code(),
        descriptor_count: s.descriptors.len() as u32,
        packets: s.packets,
        cc_errors: s.cc_errors,
        scrambled_packets: s.scrambled_packets,
        ..Default::default()
    };
    let opt = |v: Option<u64>, flag: u32, flags: &mut u32| {
        if v.is_some() {
            *flags |= flag;
        }
        v.unwrap_or(0)
    };
    if let Some(lang) = s.language {
        v.flags |= 0x001;
        for (dst, src) in v.language.iter_mut().zip(lang.0) {
            *dst = src as c_char;
        }
    }
    if let Some(reg) = s.registration {
        v.flags |= 0x002;
        v.registration = reg.0;
    }
    if let Some(fmt) = s.metadata_format {
        v.flags |= 0x004;
        v.metadata_format = fmt.0;
    }
    if let Some(id) = s.pes_stream_id {
        v.flags |= 0x008;
        v.pes_stream_id = id;
    }
    v.first_pts = opt(s.first_pts, 0x010, &mut v.flags);
    v.last_pts = opt(s.last_pts, 0x020, &mut v.flags);
    v.last_dts = opt(s.last_dts, 0x040, &mut v.flags);
    v.pts_span = opt(s.pts_span, 0x080, &mut v.flags);
    v.bitrate = opt(s.bitrate, 0x100, &mut v.flags);
    v.last_packet = opt(s.last_packet, 0x200, &mut v.flags);
    if s.is_scrambled() {
        v.flags |= 0x400;
    }
    write_out(out, v)
}

#[no_mangle]
pub unsafe extern "C" fn mpegts_report_pid(
    report: *const mpegts_report,
    i: u32,
    out: *mut mpegts_pid_info,
) -> mpegts_status {
    let r = check!(report_ref(report));
    let p = &r.pids[check!(index(i, r.pids.len()))];
    write_out(
        out,
        mpegts_pid_info {
            struct_size: size_of_u32::<mpegts_pid_info>(),
            pid: p.pid,
            reserved: 0,
            packets: p.packets,
            cc_errors: p.cc_errors,
            transport_errors: p.transport_errors,
            scrambled_packets: p.scrambled_packets,
            last_packet: p.last_packet,
        },
    )
}

#[no_mangle]
pub unsafe extern "C" fn mpegts_report_ca_system(
    report: *const mpegts_report,
    i: u32,
    system_id: *mut u16,
    pid: *mut u16,
) -> mpegts_status {
    let r = check!(report_ref(report));
    if system_id.is_null() || pid.is_null() {
        return MPEGTS_ERR_NULL;
    }
    let ca = &r.ca_systems[check!(index(i, r.ca_systems.len()))];
    system_id.write(ca.system_id);
    pid.write(ca.pid);
    MPEGTS_OK
}

unsafe fn write_descriptor(
    d: &crate::Descriptor,
    tag: *mut u8,
    data: *mut *const u8,
    len: *mut u64,
) -> mpegts_status {
    if tag.is_null() || data.is_null() || len.is_null() {
        return MPEGTS_ERR_NULL;
    }
    tag.write(d.tag);
    data.write(if d.data.is_empty() { ptr::null() } else { d.data.as_ptr() });
    len.write(d.data.len() as u64);
    MPEGTS_OK
}

#[no_mangle]
pub unsafe extern "C" fn mpegts_report_program_descriptor(
    report: *const mpegts_report,
    program_index: u32,
    descriptor_index: u32,
    tag: *mut u8,
    data: *mut *const u8,
    len: *mut u64,
) -> mpegts_status {
    let r = check!(report_ref(report));
    let p = &r.programs[check!(index(program_index, r.programs.len()))];
    let d = &p.descriptors[check!(index(descriptor_index, p.descriptors.len()))];
    write_descriptor(d, tag, data, len)
}

#[no_mangle]
pub unsafe extern "C" fn mpegts_report_stream_descriptor(
    report: *const mpegts_report,
    program_index: u32,
    stream_index: u32,
    descriptor_index: u32,
    tag: *mut u8,
    data: *mut *const u8,
    len: *mut u64,
) -> mpegts_status {
    let r = check!(report_ref(report));
    let p = &r.programs[check!(index(program_index, r.programs.len()))];
    let s = &p.streams[check!(index(stream_index, p.streams.len()))];
    let d = &s.descriptors[check!(index(descriptor_index, s.descriptors.len()))];
    write_descriptor(d, tag, data, len)
}

#[cfg(test)]
mod tests;
