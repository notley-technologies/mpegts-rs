//! C ABI of the write primitives (features `capi` and `mux`). The rules of
//! `include/mpegts.h` apply; writing functions follow the output buffer
//! convention described there next to MPEGTS_ERR_BUFFER.

use super::*;
use crate::mux::{
    AdaptationFlags, Carriage, MuxError, PesUnit, PesWriter, ProgramTables, SliceSink, StreamSpec, SyncKlvOptions,
    NULL_PACKET,
};
use crate::packet::NULL_PID;

pub const MPEGTS_CARRIAGE_PES: u32 = 1;
pub const MPEGTS_CARRIAGE_METADATA_CELLS: u32 = 2;
pub const MPEGTS_CARRIAGE_ASYNC_KLV: u32 = 3;

pub const MPEGTS_SPEC_SYNC_REGISTRATION: u32 = 0x01;
const SPEC_FLAGS: u32 = MPEGTS_SPEC_SYNC_REGISTRATION;

pub const MPEGTS_PES_HAS_PTS: u32 = 0x01;
pub const MPEGTS_PES_HAS_DTS: u32 = 0x02;
pub const MPEGTS_PES_HAS_PCR: u32 = 0x04;
pub const MPEGTS_PES_DATA_ALIGNMENT: u32 = 0x08;
pub const MPEGTS_PES_RANDOM_ACCESS: u32 = 0x10;
pub const MPEGTS_PES_ES_PRIORITY: u32 = 0x20;
pub const MPEGTS_PES_HAS_SERVICE_ID: u32 = 0x40;
const PES_FLAGS: u32 = 0x7F;
const ADAPTATION_FLAGS: u32 = MPEGTS_PES_HAS_PCR | MPEGTS_PES_RANDOM_ACCESS | MPEGTS_PES_ES_PRIORITY;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct mpegts_stream_spec {
    pub struct_size: u32,
    pub carriage: u32,
    pub stream_type: u8,
    pub stream_id: u8,
    pub service_id: u8,
    pub reserved: u8,
    pub flags: u32,
    pub input_leak_rate: u32,
    pub buffer_size: u32,
    pub output_leak_rate: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct mpegts_pes_unit {
    pub struct_size: u32,
    pub flags: u32,
    pub pts: u64,
    pub dts: u64,
    pub pcr: u64,
    pub data: *const u8,
    pub data_len: u64,
    pub service_id: u8,
    pub reserved: [u8; 7],
}

impl Default for mpegts_pes_unit {
    fn default() -> Self {
        mpegts_pes_unit {
            struct_size: 0,
            flags: 0,
            pts: 0,
            dts: 0,
            pcr: 0,
            data: ptr::null(),
            data_len: 0,
            service_id: 0,
            reserved: [0; 7],
        }
    }
}

/// Opaque program tables handle.
#[derive(Debug)]
pub struct mpegts_program_tables {
    tables: ProgramTables,
    /// Why the last failed call failed; empty after a success.
    error: String,
    poisoned: bool,
}

/// Opaque PES writer handle.
#[derive(Debug)]
pub struct mpegts_pes_writer {
    writer: PesWriter,
    error: String,
    poisoned: bool,
}

trait MuxHandle {
    fn poisoned(&mut self) -> &mut bool;
    fn is_poisoned(&self) -> bool;
    fn error(&mut self) -> &mut String;
}

macro_rules! mux_handle {
    ($t:ty) => {
        impl MuxHandle for $t {
            fn poisoned(&mut self) -> &mut bool {
                &mut self.poisoned
            }
            fn is_poisoned(&self) -> bool {
                self.poisoned
            }
            fn error(&mut self) -> &mut String {
                &mut self.error
            }
        }
    };
}
mux_handle!(mpegts_program_tables);
mux_handle!(mpegts_pes_writer);

unsafe fn mux_mut<'a, T: MuxHandle>(p: *mut T) -> Result<&'a mut T, i32> {
    let h = p.as_mut().ok_or(MPEGTS_ERR_NULL)?;
    if h.is_poisoned() {
        return Err(MPEGTS_ERR_POISONED);
    }
    Ok(h)
}

unsafe fn mux_ref<'a, T: MuxHandle>(p: *const T) -> Result<&'a T, i32> {
    let h = p.as_ref().ok_or(MPEGTS_ERR_NULL)?;
    if h.is_poisoned() {
        return Err(MPEGTS_ERR_POISONED);
    }
    Ok(h)
}

/// Run `f` on a handle; a panic poisons it.
fn mux_guard<T: MuxHandle>(h: &mut T, f: impl FnOnce(&mut T) -> i32) -> i32 {
    match catch_unwind(AssertUnwindSafe(|| f(&mut *h))) {
        Ok(status) => status,
        Err(_) => {
            *h.poisoned() = true;
            MPEGTS_ERR_PANIC
        }
    }
}

fn status_of(e: &MuxError) -> i32 {
    match e {
        MuxError::BufferTooSmall { .. } => MPEGTS_ERR_BUFFER,
        MuxError::UnknownProgram(_) | MuxError::UnknownStream { .. } => MPEGTS_ERR_NOT_FOUND,
        _ => MPEGTS_ERR_INVALID,
    }
}

/// Record the outcome of a call on its handle and turn it into a status.
fn finish<T: MuxHandle>(h: &mut T, r: Result<(), MuxError>) -> i32 {
    match r {
        Ok(()) => {
            h.error().clear();
            MPEGTS_OK
        }
        Err(e) => {
            *h.error() = e.to_string();
            status_of(&e)
        }
    }
}

/// A caller's output buffer: `out` may be NULL only when `capacity` is 0;
/// `written` is required.
unsafe fn output<'a>(out: *mut u8, capacity: usize, written: *mut usize) -> Result<&'a mut [u8], i32> {
    if written.is_null() {
        return Err(MPEGTS_ERR_NULL);
    }
    written.write(0);
    if capacity > isize::MAX as usize {
        return Err(MPEGTS_ERR_INVALID);
    }
    match (out.is_null(), capacity) {
        (_, 0) => Ok(&mut []),
        (true, _) => Err(MPEGTS_ERR_NULL),
        (false, _) => Ok(slice::from_raw_parts_mut(out, capacity)),
    }
}

/// Run a write into a caller's buffer: on success and on MPEGTS_ERR_BUFFER,
/// `*written` is the size the output needs.
unsafe fn write_into<T: MuxHandle>(
    h: &mut T,
    buf: &mut [u8],
    written: *mut usize,
    f: impl FnOnce(&mut T, &mut [u8]) -> Result<usize, MuxError>,
) -> i32 {
    match f(h, buf) {
        Ok(n) => {
            written.write(n);
            h.error().clear();
            MPEGTS_OK
        }
        Err(e) => {
            if let MuxError::BufferTooSmall { needed } = e {
                written.write(needed);
            }
            *h.error() = e.to_string();
            status_of(&e)
        }
    }
}

/// Packets into a caller's buffer: the byte count written.
fn packets<F>(buf: &mut [u8], f: F) -> Result<usize, MuxError>
where
    F: FnOnce(&mut SliceSink<'_>) -> Result<usize, MuxError>,
{
    let mut sink = SliceSink::new(buf);
    f(&mut sink)?;
    Ok(sink.len())
}

unsafe fn write_error<T: MuxHandle>(h: *const T, message: *mut c_char, message_len: usize, get: impl Fn(&T) -> &str) -> i32 {
    // The message stays readable on a poisoned handle: it is the one place
    // that explains what happened.
    let Some(h) = h.as_ref() else { return MPEGTS_ERR_NULL };
    if message.is_null() && message_len > 0 {
        return MPEGTS_ERR_NULL;
    }
    let msg = if h.is_poisoned() { "internal error; free this object" } else { get(h) };
    write_message(msg, message, message_len);
    MPEGTS_OK
}

impl mpegts_stream_spec {
    fn to_spec(self) -> Result<StreamSpec, MuxError> {
        if self.flags & !SPEC_FLAGS != 0 {
            return Err(MuxError::InvalidValue { field: "flags", reason: "unknown MPEGTS_SPEC_* bits" });
        }
        let mut spec = match self.carriage {
            MPEGTS_CARRIAGE_PES => StreamSpec::new(self.stream_type, self.stream_id),
            MPEGTS_CARRIAGE_METADATA_CELLS => {
                let mut o = SyncKlvOptions::new(self.service_id);
                o.sync_registration_descriptor = self.flags & MPEGTS_SPEC_SYNC_REGISTRATION != 0;
                o.input_leak_rate = self.input_leak_rate;
                o.buffer_size = self.buffer_size;
                o.output_leak_rate = self.output_leak_rate;
                StreamSpec::klv_sync_with(&o)?
            }
            MPEGTS_CARRIAGE_ASYNC_KLV => StreamSpec::klv_async(),
            _ => return Err(MuxError::InvalidValue { field: "carriage", reason: "not an MPEGTS_CARRIAGE_* value" }),
        };
        spec.stream_type = self.stream_type;
        spec.stream_id = self.stream_id;
        Ok(spec)
    }

    fn from_spec(spec: &StreamSpec, options: &SyncKlvOptions) -> Self {
        mpegts_stream_spec {
            struct_size: size_of_u32::<mpegts_stream_spec>(),
            carriage: match spec.carriage {
                Carriage::MetadataCells { .. } => MPEGTS_CARRIAGE_METADATA_CELLS,
                Carriage::AsyncKlv => MPEGTS_CARRIAGE_ASYNC_KLV,
                _ => MPEGTS_CARRIAGE_PES,
            },
            stream_type: spec.stream_type,
            stream_id: spec.stream_id,
            service_id: options.service_id,
            reserved: 0,
            flags: if options.sync_registration_descriptor { MPEGTS_SPEC_SYNC_REGISTRATION } else { 0 },
            input_leak_rate: options.input_leak_rate,
            buffer_size: options.buffer_size,
            output_leak_rate: options.output_leak_rate,
        }
    }
}

/// The caller's unit, honouring its struct_size; fields it does not know
/// about are zero (no flags).
unsafe fn read_unit<'a>(unit: *const mpegts_pes_unit) -> Result<PesUnit<'a>, i32> {
    let u = read_in(unit, mpegts_pes_unit::default())?;
    if u.flags & !PES_FLAGS != 0 {
        return Err(MPEGTS_ERR_INVALID);
    }
    let len = usize::try_from(u.data_len).map_err(|_| MPEGTS_ERR_INVALID)?;
    let data = input(u.data, len)?;
    let has = |f: u32| u.flags & f != 0;
    let mut p = PesUnit::new(data);
    p.pts = has(MPEGTS_PES_HAS_PTS).then_some(u.pts);
    p.dts = has(MPEGTS_PES_HAS_DTS).then_some(u.dts);
    p.data_alignment = has(MPEGTS_PES_DATA_ALIGNMENT);
    p.adaptation = adaptation(u.flags, u.pcr);
    p.service_id = has(MPEGTS_PES_HAS_SERVICE_ID).then_some(u.service_id);
    Ok(p)
}

fn adaptation(flags: u32, pcr: u64) -> AdaptationFlags {
    let mut a = AdaptationFlags::new();
    a.pcr = (flags & MPEGTS_PES_HAS_PCR != 0).then_some(pcr);
    a.random_access = flags & MPEGTS_PES_RANDOM_ACCESS != 0;
    a.es_priority = flags & MPEGTS_PES_ES_PRIORITY != 0;
    a
}

fn optional_pid(pid: u16) -> Option<u16> {
    (pid != NULL_PID).then_some(pid)
}

// ---------------------------------------------------------------- stream specs

#[no_mangle]
pub unsafe extern "C" fn mpegts_stream_spec_pes(
    spec: *mut mpegts_stream_spec,
    stream_type: u8,
    stream_id: u8,
) -> mpegts_status {
    write_out(spec, mpegts_stream_spec::from_spec(&StreamSpec::new(stream_type, stream_id), &SyncKlvOptions::default()))
}

#[no_mangle]
pub unsafe extern "C" fn mpegts_stream_spec_klv_sync(spec: *mut mpegts_stream_spec, service_id: u8) -> mpegts_status {
    let options = SyncKlvOptions::new(service_id);
    write_out(spec, mpegts_stream_spec::from_spec(&StreamSpec::klv_sync(service_id), &options))
}

#[no_mangle]
pub unsafe extern "C" fn mpegts_stream_spec_klv_async(spec: *mut mpegts_stream_spec) -> mpegts_status {
    write_out(spec, mpegts_stream_spec::from_spec(&StreamSpec::klv_async(), &SyncKlvOptions::default()))
}

// ---------------------------------------------------------------- program tables

fn new_tables(tables: ProgramTables) -> *mut mpegts_program_tables {
    Box::into_raw(Box::new(mpegts_program_tables { tables, error: String::new(), poisoned: false }))
}

#[no_mangle]
pub unsafe extern "C" fn mpegts_program_tables_new(
    transport_stream_id: u16,
    out: *mut *mut mpegts_program_tables,
) -> mpegts_status {
    if out.is_null() {
        return MPEGTS_ERR_NULL;
    }
    out.write(ptr::null_mut());
    guard(|| {
        out.write(new_tables(ProgramTables::new(transport_stream_id)));
        MPEGTS_OK
    })
}

#[no_mangle]
pub unsafe extern "C" fn mpegts_program_tables_from_report(
    report: *const mpegts_report,
    out: *mut *mut mpegts_program_tables,
) -> mpegts_status {
    if out.is_null() {
        return MPEGTS_ERR_NULL;
    }
    out.write(ptr::null_mut());
    let r = check!(report_ref(report));
    guard(|| match ProgramTables::from_report(r) {
        Ok(t) => {
            out.write(new_tables(t));
            MPEGTS_OK
        }
        Err(e) => status_of(&e),
    })
}

#[no_mangle]
pub unsafe extern "C" fn mpegts_program_tables_free(tables: *mut mpegts_program_tables) {
    if !tables.is_null() {
        let _ = catch_unwind(AssertUnwindSafe(|| drop(Box::from_raw(tables))));
    }
}

#[no_mangle]
pub unsafe extern "C" fn mpegts_program_tables_error(
    tables: *const mpegts_program_tables,
    message: *mut c_char,
    message_len: usize,
) -> mpegts_status {
    write_error(tables, message, message_len, |h| &h.error)
}

/// Apply a change to the tables.
unsafe fn change(
    tables: *mut mpegts_program_tables,
    f: impl FnOnce(&mut ProgramTables) -> Result<(), MuxError>,
) -> mpegts_status {
    let h = check!(mux_mut(tables));
    mux_guard(h, |h| {
        let r = f(&mut h.tables);
        finish(h, r)
    })
}

#[no_mangle]
pub unsafe extern "C" fn mpegts_program_tables_add_program(
    tables: *mut mpegts_program_tables,
    number: u16,
    pmt_pid: u16,
    pcr_pid: u16,
) -> mpegts_status {
    change(tables, |t| t.add_program(number, pmt_pid, optional_pid(pcr_pid)))
}

#[no_mangle]
pub unsafe extern "C" fn mpegts_program_tables_remove_program(
    tables: *mut mpegts_program_tables,
    number: u16,
) -> mpegts_status {
    change(tables, |t| t.remove_program(number))
}

#[no_mangle]
pub unsafe extern "C" fn mpegts_program_tables_set_pcr_pid(
    tables: *mut mpegts_program_tables,
    number: u16,
    pcr_pid: u16,
) -> mpegts_status {
    change(tables, |t| t.set_pcr_pid(number, optional_pid(pcr_pid)))
}

#[no_mangle]
pub unsafe extern "C" fn mpegts_program_tables_set_network_pid(
    tables: *mut mpegts_program_tables,
    pid: u16,
) -> mpegts_status {
    change(tables, |t| t.set_network_pid(optional_pid(pid)))
}

#[no_mangle]
pub unsafe extern "C" fn mpegts_program_tables_add_program_descriptor(
    tables: *mut mpegts_program_tables,
    number: u16,
    tag: u8,
    data: *const u8,
    len: usize,
) -> mpegts_status {
    let bytes = check!(input(data, len));
    change(tables, |t| t.add_program_descriptor(number, tag, bytes))
}

#[no_mangle]
pub unsafe extern "C" fn mpegts_program_tables_add_stream(
    tables: *mut mpegts_program_tables,
    number: u16,
    pid: u16,
    spec: *const mpegts_stream_spec,
) -> mpegts_status {
    let s = check!(read_in(spec, mpegts_stream_spec::default()));
    change(tables, |t| t.add_stream(number, pid, &s.to_spec()?))
}

#[no_mangle]
pub unsafe extern "C" fn mpegts_program_tables_add_stream_descriptor(
    tables: *mut mpegts_program_tables,
    number: u16,
    pid: u16,
    tag: u8,
    data: *const u8,
    len: usize,
) -> mpegts_status {
    let bytes = check!(input(data, len));
    change(tables, |t| t.add_stream_descriptor(number, pid, tag, bytes))
}

#[no_mangle]
pub unsafe extern "C" fn mpegts_program_tables_remove_stream(
    tables: *mut mpegts_program_tables,
    number: u16,
    pid: u16,
) -> mpegts_status {
    change(tables, |t| t.remove_stream(number, pid))
}

#[no_mangle]
pub unsafe extern "C" fn mpegts_program_tables_set_pat_version(
    tables: *mut mpegts_program_tables,
    version: u8,
) -> mpegts_status {
    change(tables, |t| t.set_pat_version(version))
}

#[no_mangle]
pub unsafe extern "C" fn mpegts_program_tables_set_pmt_version(
    tables: *mut mpegts_program_tables,
    number: u16,
    version: u8,
) -> mpegts_status {
    change(tables, |t| t.set_pmt_version(number, version))
}

#[no_mangle]
pub unsafe extern "C" fn mpegts_program_tables_pat_version(
    tables: *const mpegts_program_tables,
    version: *mut u8,
) -> mpegts_status {
    let h = check!(mux_ref(tables));
    if version.is_null() {
        return MPEGTS_ERR_NULL;
    }
    version.write(h.tables.pat_version());
    MPEGTS_OK
}

#[no_mangle]
pub unsafe extern "C" fn mpegts_program_tables_pmt_version(
    tables: *const mpegts_program_tables,
    number: u16,
    version: *mut u8,
) -> mpegts_status {
    let h = check!(mux_ref(tables));
    if version.is_null() {
        return MPEGTS_ERR_NULL;
    }
    match h.tables.program(number) {
        Some(p) => {
            version.write(p.version);
            MPEGTS_OK
        }
        None => MPEGTS_ERR_NOT_FOUND,
    }
}

#[no_mangle]
pub unsafe extern "C" fn mpegts_program_tables_pat_section(
    tables: *mut mpegts_program_tables,
    out: *mut u8,
    capacity: usize,
    written: *mut usize,
) -> mpegts_status {
    let h = check!(mux_mut(tables));
    let buf = check!(output(out, capacity, written));
    mux_guard(h, |h| write_into(h, buf, written, |h, buf| h.tables.pat_section_into(buf)))
}

#[no_mangle]
pub unsafe extern "C" fn mpegts_program_tables_pmt_section(
    tables: *mut mpegts_program_tables,
    number: u16,
    out: *mut u8,
    capacity: usize,
    written: *mut usize,
) -> mpegts_status {
    let h = check!(mux_mut(tables));
    let buf = check!(output(out, capacity, written));
    mux_guard(h, |h| write_into(h, buf, written, |h, buf| h.tables.pmt_section_into(number, buf)))
}

#[no_mangle]
pub unsafe extern "C" fn mpegts_program_tables_write_pat(
    tables: *mut mpegts_program_tables,
    out: *mut u8,
    capacity: usize,
    written: *mut usize,
) -> mpegts_status {
    let h = check!(mux_mut(tables));
    let buf = check!(output(out, capacity, written));
    mux_guard(h, |h| write_into(h, buf, written, |h, buf| packets(buf, |s| h.tables.write_pat(s))))
}

#[no_mangle]
pub unsafe extern "C" fn mpegts_program_tables_write_pmt(
    tables: *mut mpegts_program_tables,
    number: u16,
    out: *mut u8,
    capacity: usize,
    written: *mut usize,
) -> mpegts_status {
    let h = check!(mux_mut(tables));
    let buf = check!(output(out, capacity, written));
    mux_guard(h, |h| write_into(h, buf, written, |h, buf| packets(buf, |s| h.tables.write_pmt(number, s))))
}

#[no_mangle]
pub unsafe extern "C" fn mpegts_program_tables_write_all(
    tables: *mut mpegts_program_tables,
    out: *mut u8,
    capacity: usize,
    written: *mut usize,
) -> mpegts_status {
    let h = check!(mux_mut(tables));
    let buf = check!(output(out, capacity, written));
    mux_guard(h, |h| write_into(h, buf, written, |h, buf| packets(buf, |s| h.tables.write_all(s))))
}

#[no_mangle]
pub unsafe extern "C" fn mpegts_program_tables_continuity_counter(
    tables: *const mpegts_program_tables,
    pid: u16,
    cc: *mut u8,
) -> mpegts_status {
    let h = check!(mux_ref(tables));
    if cc.is_null() {
        return MPEGTS_ERR_NULL;
    }
    match h.tables.writer(pid) {
        Some(w) => {
            cc.write(w.continuity_counter());
            MPEGTS_OK
        }
        None => MPEGTS_ERR_NOT_FOUND,
    }
}

#[no_mangle]
pub unsafe extern "C" fn mpegts_program_tables_set_continuity_counter(
    tables: *mut mpegts_program_tables,
    pid: u16,
    cc: u8,
) -> mpegts_status {
    let h = check!(mux_mut(tables));
    if cc > 15 {
        return MPEGTS_ERR_INVALID;
    }
    match h.tables.writer_mut(pid) {
        Some(w) => {
            w.set_continuity_counter(cc);
            MPEGTS_OK
        }
        None => MPEGTS_ERR_NOT_FOUND,
    }
}

// ---------------------------------------------------------------- PES writer

#[no_mangle]
pub unsafe extern "C" fn mpegts_pes_writer_new(
    pid: u16,
    spec: *const mpegts_stream_spec,
    out: *mut *mut mpegts_pes_writer,
) -> mpegts_status {
    if out.is_null() {
        return MPEGTS_ERR_NULL;
    }
    out.write(ptr::null_mut());
    let s = check!(read_in(spec, mpegts_stream_spec::default()));
    guard(|| match s.to_spec().and_then(|spec| PesWriter::new(pid, &spec)) {
        Ok(writer) => {
            out.write(Box::into_raw(Box::new(mpegts_pes_writer { writer, error: String::new(), poisoned: false })));
            MPEGTS_OK
        }
        Err(e) => status_of(&e),
    })
}

#[no_mangle]
pub unsafe extern "C" fn mpegts_pes_writer_free(writer: *mut mpegts_pes_writer) {
    if !writer.is_null() {
        let _ = catch_unwind(AssertUnwindSafe(|| drop(Box::from_raw(writer))));
    }
}

#[no_mangle]
pub unsafe extern "C" fn mpegts_pes_writer_error(
    writer: *const mpegts_pes_writer,
    message: *mut c_char,
    message_len: usize,
) -> mpegts_status {
    write_error(writer, message, message_len, |h| &h.error)
}

#[no_mangle]
pub unsafe extern "C" fn mpegts_pes_writer_write(
    writer: *mut mpegts_pes_writer,
    unit: *const mpegts_pes_unit,
    out: *mut u8,
    capacity: usize,
    written: *mut usize,
) -> mpegts_status {
    let h = check!(mux_mut(writer));
    let buf = check!(output(out, capacity, written));
    let unit = check!(read_unit(unit));
    mux_guard(h, |h| write_into(h, buf, written, |h, buf| packets(buf, |s| h.writer.write(&unit, s))))
}

#[no_mangle]
pub unsafe extern "C" fn mpegts_pes_writer_write_adaptation_only(
    writer: *mut mpegts_pes_writer,
    flags: u32,
    pcr: u64,
    out: *mut u8,
    capacity: usize,
    written: *mut usize,
) -> mpegts_status {
    let h = check!(mux_mut(writer));
    let buf = check!(output(out, capacity, written));
    if flags & !ADAPTATION_FLAGS != 0 {
        return MPEGTS_ERR_INVALID;
    }
    let a = adaptation(flags, pcr);
    mux_guard(h, |h| {
        write_into(h, buf, written, |h, buf| {
            // The same rule as for units: KLV PIDs carry no PCR or
            // random_access_indicator (DECISIONS section 2).
            if !a.is_empty() && !matches!(h.writer.carriage(), Carriage::Pes) {
                return Err(MuxError::InvalidUnit("KLV PIDs carry no PCR and no random_access_indicator"));
            }
            packets(buf, |s| h.writer.ts_mut().write_adaptation_only(&a, s))
        })
    })
}

#[no_mangle]
pub unsafe extern "C" fn mpegts_pes_writer_continuity_counter(
    writer: *const mpegts_pes_writer,
    cc: *mut u8,
) -> mpegts_status {
    let h = check!(mux_ref(writer));
    if cc.is_null() {
        return MPEGTS_ERR_NULL;
    }
    cc.write(h.writer.ts().continuity_counter());
    MPEGTS_OK
}

#[no_mangle]
pub unsafe extern "C" fn mpegts_pes_writer_set_continuity_counter(
    writer: *mut mpegts_pes_writer,
    cc: u8,
) -> mpegts_status {
    let h = check!(mux_mut(writer));
    if cc > 15 {
        return MPEGTS_ERR_INVALID;
    }
    h.writer.ts_mut().set_continuity_counter(cc);
    MPEGTS_OK
}

#[no_mangle]
pub unsafe extern "C" fn mpegts_pes_writer_mark_discontinuity(writer: *mut mpegts_pes_writer) -> mpegts_status {
    let h = check!(mux_mut(writer));
    h.writer.ts_mut().mark_discontinuity();
    MPEGTS_OK
}

#[no_mangle]
pub unsafe extern "C" fn mpegts_pes_writer_sequence_number(
    writer: *const mpegts_pes_writer,
    sequence: *mut u8,
) -> mpegts_status {
    let h = check!(mux_ref(writer));
    if sequence.is_null() {
        return MPEGTS_ERR_NULL;
    }
    sequence.write(h.writer.sequence_number());
    MPEGTS_OK
}

#[no_mangle]
pub unsafe extern "C" fn mpegts_pes_writer_set_sequence_number(
    writer: *mut mpegts_pes_writer,
    sequence: u8,
) -> mpegts_status {
    let h = check!(mux_mut(writer));
    h.writer.set_sequence_number(sequence);
    MPEGTS_OK
}

/// [`CellNumbering::PerAccessUnit`](crate::mux::CellNumbering::PerAccessUnit): each PES
/// packet numbers its cells from 0 (the default, DECISIONS X-8).
pub const MPEGTS_CELLS_PER_AU: u32 = 0;
/// [`CellNumbering::PerStream`](crate::mux::CellNumbering::PerStream).
pub const MPEGTS_CELLS_PER_STREAM: u32 = 1;

#[no_mangle]
pub unsafe extern "C" fn mpegts_pes_writer_set_cell_numbering(
    writer: *mut mpegts_pes_writer,
    numbering: u32,
) -> mpegts_status {
    let h = check!(mux_mut(writer));
    h.writer.set_cell_numbering(match numbering {
        MPEGTS_CELLS_PER_AU => crate::mux::CellNumbering::PerAccessUnit,
        MPEGTS_CELLS_PER_STREAM => crate::mux::CellNumbering::PerStream,
        _ => return MPEGTS_ERR_INVALID,
    });
    MPEGTS_OK
}

#[no_mangle]
pub unsafe extern "C" fn mpegts_null_packet(packet: *mut u8) -> mpegts_status {
    if packet.is_null() {
        return MPEGTS_ERR_NULL;
    }
    ptr::copy_nonoverlapping(NULL_PACKET.as_ptr(), packet, NULL_PACKET.len());
    MPEGTS_OK
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Codec, Demuxer, Select, Subscription, Unit};

    fn sized<T: Default>() -> T {
        let mut v = T::default();
        // SAFETY: every ABI struct starts with a u32 struct_size.
        unsafe { (ptr::addr_of_mut!(v) as *mut u32).write(mem::size_of::<T>() as u32) };
        v
    }

    fn error(t: *const mpegts_program_tables) -> String {
        let mut buf = [0 as c_char; 256];
        assert_eq!(unsafe { mpegts_program_tables_error(t, buf.as_mut_ptr(), buf.len()) }, MPEGTS_OK);
        unsafe { std::ffi::CStr::from_ptr(buf.as_ptr()) }.to_string_lossy().into_owned()
    }

    #[test]
    fn tables_and_writers_round_trip() {
        unsafe {
            let mut t = ptr::null_mut();
            assert_eq!(mpegts_program_tables_new(3, &mut t), MPEGTS_OK);
            assert_eq!(mpegts_program_tables_add_program(t, 1, 0x1000, 0x1FFF), MPEGTS_OK);
            let mut spec: mpegts_stream_spec = sized();
            assert_eq!(mpegts_stream_spec_klv_sync(&mut spec, 0), MPEGTS_OK);
            assert_eq!((spec.carriage, spec.stream_type, spec.stream_id), (MPEGTS_CARRIAGE_METADATA_CELLS, 0x15, 0xFC));
            assert_eq!((spec.input_leak_rate, spec.buffer_size, spec.output_leak_rate), (0x3FFFFF, 0x400, 0));
            assert_eq!(mpegts_program_tables_add_stream(t, 1, 0x101, &spec), MPEGTS_OK);
            assert_eq!(mpegts_program_tables_add_stream(t, 1, 0x101, &spec), MPEGTS_ERR_INVALID);
            assert!(error(t).contains("0x0101"), "{}", error(t));
            assert_eq!(mpegts_program_tables_add_stream(t, 9, 0x102, &spec), MPEGTS_ERR_NOT_FOUND);
            assert_eq!(mpegts_program_tables_remove_stream(t, 1, 0x555), MPEGTS_ERR_NOT_FOUND);

            // Size query, short buffer, then the real write.
            let mut written = 0usize;
            assert_eq!(mpegts_program_tables_write_all(t, ptr::null_mut(), 0, &mut written), MPEGTS_ERR_BUFFER);
            assert_eq!(written, 2 * 188);
            let mut ts = vec![0u8; written];
            assert_eq!(mpegts_program_tables_write_all(t, ts.as_mut_ptr(), 300, &mut written), MPEGTS_ERR_BUFFER);
            assert_eq!(written, 376);
            assert_eq!(mpegts_program_tables_write_all(t, ts.as_mut_ptr(), ts.len(), &mut written), MPEGTS_OK);
            assert_eq!(written, 376);
            assert_eq!(error(t), "");

            let mut w = ptr::null_mut();
            assert_eq!(mpegts_pes_writer_new(0x101, &spec, &mut w), MPEGTS_OK);
            let klv = [0x06u8, 0x0E, 0x2B, 0x34, 1, 2, 3];
            let mut unit: mpegts_pes_unit = sized();
            unit.data = klv.as_ptr();
            unit.data_len = klv.len() as u64;
            let mut buf = [0u8; 188];
            assert_eq!(mpegts_pes_writer_write(w, &unit, buf.as_mut_ptr(), buf.len(), &mut written), MPEGTS_ERR_INVALID);
            unit.flags = MPEGTS_PES_HAS_PTS;
            unit.pts = 90_000;
            assert_eq!(mpegts_pes_writer_write(w, &unit, buf.as_mut_ptr(), buf.len(), &mut written), MPEGTS_OK);
            ts.extend_from_slice(&buf[..written]);
            let mut seq = 9;
            assert_eq!(mpegts_pes_writer_sequence_number(w, &mut seq), MPEGTS_OK);
            assert_eq!(seq, 0, "cells numbered per access unit (DECISIONS X-8)");
            assert_eq!(mpegts_pes_writer_set_cell_numbering(w, 7), MPEGTS_ERR_INVALID);
            assert_eq!(mpegts_pes_writer_set_cell_numbering(w, MPEGTS_CELLS_PER_STREAM), MPEGTS_OK);
            assert_eq!(mpegts_pes_writer_sequence_number(w, &mut seq), MPEGTS_OK);
            assert_eq!(seq, 1, "the per-stream counter went on");
            assert_eq!(mpegts_pes_writer_set_cell_numbering(w, MPEGTS_CELLS_PER_AU), MPEGTS_OK);
            unit.flags = 0x80;
            assert_eq!(mpegts_pes_writer_write(w, &unit, buf.as_mut_ptr(), buf.len(), &mut written), MPEGTS_ERR_INVALID);
            assert_eq!(mpegts_pes_writer_write_adaptation_only(w, MPEGTS_PES_HAS_PCR, 0, buf.as_mut_ptr(), 188, &mut written), MPEGTS_ERR_INVALID);
            mpegts_pes_writer_free(w);

            let mut d = Demuxer::new();
            d.subscribe(Subscription::new(Select::Codec(Codec::KlvSync), Unit::Pes).strip_metadata_au()).unwrap();
            d.push(&ts);
            d.finish();
            let outs: Vec<_> = d.outputs().collect();
            assert_eq!(outs.len(), 1);
            assert_eq!(outs[0].payload(), klv);
            assert_eq!(outs[0].pts, Some(90_000));

            // Tables from the report keep the versions.
            let mut r = ptr::null_mut();
            assert_eq!(mpegts_analyze(ts.as_ptr(), ts.len(), &mut r), MPEGTS_OK);
            let mut t2 = ptr::null_mut();
            assert_eq!(mpegts_program_tables_from_report(r, &mut t2), MPEGTS_OK);
            let (mut a, mut b) = (0u8, 0u8);
            assert_eq!(mpegts_program_tables_pmt_version(t, 1, &mut a), MPEGTS_OK);
            assert_eq!(mpegts_program_tables_pmt_version(t2, 1, &mut b), MPEGTS_OK);
            assert_eq!(a, b);
            mpegts_report_free(r);
            mpegts_program_tables_free(t2);
            mpegts_program_tables_free(t);
        }
    }

    #[test]
    fn null_arguments() {
        unsafe {
            assert_eq!(mpegts_program_tables_new(1, ptr::null_mut()), MPEGTS_ERR_NULL);
            assert_eq!(mpegts_program_tables_add_program(ptr::null_mut(), 1, 0x20, 0x1FFF), MPEGTS_ERR_NULL);
            let mut t = ptr::null_mut();
            assert_eq!(mpegts_program_tables_new(1, &mut t), MPEGTS_OK);
            assert_eq!(mpegts_program_tables_write_all(t, ptr::null_mut(), 0, ptr::null_mut()), MPEGTS_ERR_NULL);
            let mut written = 0;
            assert_eq!(mpegts_program_tables_write_all(t, ptr::null_mut(), 10, &mut written), MPEGTS_ERR_NULL);
            assert_eq!(mpegts_program_tables_add_stream(t, 1, 0x20, ptr::null()), MPEGTS_ERR_NULL);
            assert_eq!(mpegts_program_tables_add_program_descriptor(t, 1, 5, ptr::null(), 4), MPEGTS_ERR_NULL);
            assert_eq!(mpegts_program_tables_error(t, ptr::null_mut(), 0), MPEGTS_OK);
            assert_eq!(mpegts_program_tables_error(t, ptr::null_mut(), 5), MPEGTS_ERR_NULL);
            assert_eq!(mpegts_null_packet(ptr::null_mut()), MPEGTS_ERR_NULL);
            let mut p = [0u8; 188];
            assert_eq!(mpegts_null_packet(p.as_mut_ptr()), MPEGTS_OK);
            assert_eq!(p, NULL_PACKET);
            let mut w = ptr::null_mut();
            assert_eq!(mpegts_pes_writer_new(0x20, ptr::null(), &mut w), MPEGTS_ERR_NULL);
            assert!(w.is_null());
            let mut spec: mpegts_stream_spec = sized();
            spec.carriage = 77;
            assert_eq!(mpegts_pes_writer_new(0x20, &spec, &mut w), MPEGTS_ERR_INVALID);
            assert!(w.is_null());
            mpegts_program_tables_free(t);
            mpegts_program_tables_free(ptr::null_mut());
            mpegts_pes_writer_free(ptr::null_mut());
        }
    }
}
