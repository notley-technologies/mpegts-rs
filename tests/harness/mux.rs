//! The write primitives under random use (feature `mux`): random tables,
//! streams, units, sinks and counter changes. Whatever is accepted must
//! obey the packet rules and read back unchanged through the demuxer;
//! whatever is refused must be refused with an error, never a panic.

use super::Input;
use mpegts::mux::{AdaptationFlags, Carriage, MuxError, PesUnit, PesWriter, ProgramTables, SliceSink, StreamSpec};
use mpegts::{crc32_mpeg2, Config, Demuxer, PacketHeader, Select, Subscription, Unit};
use std::collections::HashMap;

/// Elementary PIDs for writers; table PIDs never collide with them.
const ES_PIDS: [u16; 4] = [0x100, 0x101, 0x102, 0x103];
/// PIDs offered to the tables, valid and not. None is a writer PID, so the
/// demuxer never reads PES packets as sections; collisions with
/// elementary streams are still tried, as streams on these PIDs.
const TABLE_PIDS: [u16; 8] = [0x1000, 0x1001, 0x0010, 0x1FFE, 0x0005, 0x1FFF, 0x0000, 0x1FF0];
/// Bytes of unit data one run may write, to keep runs fast.
const MAX_DATA: usize = 600_000;

fn spec(kind: u8, input: &mut Input<'_>) -> StreamSpec {
    match kind % 6 {
        0 => StreamSpec::klv_sync(input.u8() % 4),
        1 => StreamSpec::klv_async(),
        2 => StreamSpec::h264(),
        3 => StreamSpec::aac_adts(),
        4 => StreamSpec::new(0x06, 0xBD),
        // private_stream_2: no optional PES header.
        _ => StreamSpec::new(0x06, 0xBF),
    }
}

fn data_len(input: &mut Input<'_>) -> usize {
    match input.u8() % 16 {
        0 => 60_000 + usize::from(input.u16()) * 2,
        1 => 65_500 + usize::from(input.u8() % 64),
        _ => usize::from(input.u16()) % 1500,
    }
}

/// Every section the tables can write is a valid PSI section.
fn check_tables(t: &ProgramTables) {
    let pat = t.pat_section();
    assert!(pat.len() <= 1024 && crc32_mpeg2(&pat) == 0);
    for p in t.programs() {
        let s = t.pmt_section(p.number).unwrap();
        assert!(s.len() <= 1024 && crc32_mpeg2(&s) == 0);
        assert_eq!(s[5], 0xC1 | p.version << 1);
    }
}

pub fn mux(data: &[u8]) {
    mux_run(data);
}

/// What one run of [`mux`] did, so a test can check that random inputs
/// reach the interesting paths.
#[derive(Debug, Default, Clone, Copy)]
pub struct MuxRun {
    /// Units written.
    pub units: usize,
    /// Units that took more than one PES packet.
    pub split: usize,
    /// Changes the tables accepted.
    pub table_changes: usize,
}

pub fn mux_run(data: &[u8]) -> MuxRun {
    let mut run = MuxRun::default();
    let mut input = Input::new(data);

    // Tables: random changes; refusals are fine and change nothing.
    let mut tables = ProgramTables::new(input.u16());
    for _ in 0..input.u8() % 12 {
        let before = (tables.pat_section(), tables.programs().to_vec(), tables.pat_version());
        let number = u16::from(input.u8() % 4);
        let pid = TABLE_PIDS[usize::from(input.u8()) % TABLE_PIDS.len()];
        let r = match input.u8() % 8 {
            0 | 1 => tables.add_program(number, pid, (input.bool()).then_some(ES_PIDS[0])),
            2 => tables.remove_program(number),
            3 => tables.set_network_pid(input.bool().then_some(pid)),
            4 => {
                let n = usize::from(input.u16() % 300);
                tables.add_program_descriptor(number, input.u8(), &input.filled(n))
            }
            5 => tables.set_pcr_pid(number, Some(pid)),
            _ => {
                let es = if input.bool() { ES_PIDS[usize::from(input.u8()) % 4] } else { pid };
                let kind = input.u8();
                tables.add_stream(number, es, &spec(kind, &mut input))
            }
        };
        if r.is_err() {
            assert_eq!((tables.pat_section(), tables.programs().to_vec(), tables.pat_version()), before, "{r:?}");
        } else {
            run.table_changes += 1;
        }
        check_tables(&tables);
    }

    // Writers on distinct PIDs.
    let mut writers: Vec<(PesWriter, Vec<u8>)> = Vec::new();
    for (i, &pid) in ES_PIDS.iter().enumerate().take(1 + usize::from(input.u8() % 4)) {
        let kind = if i == 0 { input.u8() % 4 } else { input.u8() };
        let w = PesWriter::new(pid, &spec(kind, &mut input)).expect("presets are valid");
        writers.push((w, Vec::new()));
    }

    let mut ts = Vec::new();
    tables.write_all(&mut ts).unwrap();
    let mut budget = MAX_DATA;
    for _ in 0..input.u8() % 40 {
        if input.is_empty() {
            break;
        }
        let wi = usize::from(input.u8()) % writers.len();
        match input.u8() % 16 {
            0 => {
                let w = &mut writers[wi].0;
                w.ts_mut().set_continuity_counter(input.u8());
                w.ts_mut().mark_discontinuity();
                continue;
            }
            1 => {
                tables.write_all(&mut ts).unwrap();
                continue;
            }
            2 => {
                let w = &mut writers[wi].0;
                let cc = w.ts().continuity_counter();
                let flags = AdaptationFlags::new().with_pcr(input.u64());
                if w.carriage() == Carriage::Pes {
                    w.ts_mut().write_adaptation_only(&flags, &mut ts).unwrap();
                    assert_eq!(w.ts().continuity_counter(), cc);
                }
                continue;
            }
            _ => {}
        }
        let len = data_len(&mut input).min(budget);
        budget -= len;
        let payload = input.filled(len);
        let mut unit = PesUnit::new(&payload);
        let flags = input.u8();
        if flags & 1 != 0 {
            unit = unit.with_pts(input.u64());
        }
        if flags & 2 != 0 {
            unit = unit.with_dts(input.u64());
        }
        if flags & 4 != 0 {
            unit = unit.with_data_alignment();
        }
        if flags & 8 != 0 {
            unit = unit.with_pcr(input.u64());
        }
        if flags & 16 != 0 {
            unit = unit.with_random_access();
        }
        if flags & 32 != 0 {
            unit = unit.with_service(input.u8() % 4);
        }
        let (w, expected) = &mut writers[wi];
        let needed = match w.packets_needed(&unit) {
            Ok(n) => n,
            Err(e) => {
                assert!(matches!(e, MuxError::InvalidUnit(_)), "{e:?}");
                assert_eq!(w.write(&unit, &mut ts), Err(e));
                continue;
            }
        };
        // Now and then a sink that is too small: refused, nothing changes.
        if flags & 64 != 0 {
            let before = w.clone();
            let mut buf = vec![0u8; 188 * (needed - 1)];
            let mut short = SliceSink::new(&mut buf);
            assert_eq!(w.write(&unit, &mut short), Err(MuxError::BufferTooSmall { needed: needed * 188 }));
            assert!(short.is_empty());
            assert_eq!(*w, before);
        }
        let start = ts.len();
        assert_eq!(w.write(&unit, &mut ts), Ok(needed));
        assert_eq!(ts.len() - start, needed * 188);
        expected.extend_from_slice(&payload);
        run.units += 1;
        run.split += usize::from(payload.len() > 65_000);
    }

    // Packet rules.
    let mut last_cc: HashMap<u16, u8> = HashMap::new();
    for chunk in ts.chunks(188) {
        let p: &[u8; 188] = chunk.try_into().unwrap();
        let h = PacketHeader::parse(p).expect("written packets parse");
        assert!(!h.transport_error && h.scrambling == 0 && h.adaptation_field_control != 0);
        if let Some(af) = h.adaptation_field {
            assert!(if h.adaptation_field_control == 0b10 { af.length == 183 } else { af.length <= 182 });
        }
        let has_payload = h.adaptation_field_control & 1 != 0;
        // A discontinuity_indicator allows any counter (R-ISO13818-013).
        match last_cc.get(&h.pid) {
            Some(&prev) if !h.discontinuity() => {
                assert_eq!(h.continuity_counter, if has_payload { (prev + 1) & 15 } else { prev });
            }
            _ => {}
        }
        // A packet without payload repeats the counter; with a
        // discontinuity_indicator it sets the base the next one continues.
        if has_payload || h.discontinuity() {
            last_cc.insert(h.pid, h.continuity_counter);
        }
    }

    // Read back: every PID's payloads, joined, are what went in.
    let mut config = Config::default();
    config.max_pes_bytes = 1 << 20;
    let mut d = Demuxer::with_config(config).unwrap();
    let ids: Vec<_> = writers
        .iter()
        .map(|(w, _)| d.subscribe(Subscription::new(Select::Pid(w.pid()), Unit::Pes).strip_metadata_au()).unwrap())
        .collect();
    d.push(&ts);
    d.finish();
    let report = d.report();
    assert_eq!(report.stats.crc_errors, 0);
    assert!(report.pids.iter().all(|p| p.cc_errors == 0), "{:?}", report.pids);
    let outputs: Vec<_> = d.outputs().collect();
    for ((_, expected), id) in writers.iter().zip(ids) {
        let mut joined = Vec::new();
        for o in outputs.iter().filter(|o| o.subscription == id) {
            assert!(!o.damaged && !o.truncated);
            joined.extend_from_slice(o.payload());
        }
        assert_eq!(&joined, expected);
    }
    run
}

// ---------------------------------------------------------------- C API

#[cfg(feature = "capi")]
pub use c::capi_mux_session;

#[cfg(feature = "capi")]
mod c {
    use super::super::{expect, struct_size, touch, Input};
    use super::{ES_PIDS, TABLE_PIDS};
    use mpegts::capi::*;
    use mpegts::{crc32_mpeg2, PacketHeader};
    use std::os::raw::c_char;
    use std::{mem, ptr};

    unsafe fn check_packets(buf: &[u8], written: usize) {
        assert_eq!(written % 188, 0);
        for p in buf[..written].chunks(188) {
            PacketHeader::parse(p.try_into().unwrap()).expect("written packets parse");
        }
    }

    unsafe fn spec(ops: &mut Input<'_>) -> mpegts_stream_spec {
        let mut s: mpegts_stream_spec = mem::zeroed();
        s.struct_size = mem::size_of::<mpegts_stream_spec>() as u32;
        let status = match ops.u8() % 4 {
            0 => mpegts_stream_spec_klv_sync(&mut s, ops.u8() % 4),
            1 => mpegts_stream_spec_klv_async(&mut s),
            2 => mpegts_stream_spec_pes(&mut s, 0x1B, 0xE0),
            _ => mpegts_stream_spec_pes(&mut s, ops.u8(), ops.u8()),
        };
        expect(status, &[MPEGTS_OK], "stream_spec");
        match ops.u8() % 8 {
            0 => s.flags = ops.u32(),
            1 => s.carriage = u32::from(ops.u8() % 5),
            2 => s.input_leak_rate = ops.u32(),
            3 => s.struct_size = struct_size::<mpegts_stream_spec>(ops),
            _ => {}
        }
        s
    }

    /// Random calls on program tables and PES writers.
    pub unsafe fn capi_mux_session(ops: &mut Input<'_>) {
        const OK: &[i32] = &[MPEGTS_OK];
        let mut t = ptr::null_mut();
        expect(mpegts_program_tables_new(ops.u16(), &mut t), OK, "tables_new");
        let mut writers: Vec<*mut mpegts_pes_writer> = Vec::new();
        let mut buf = vec![0u8; 188 * 800];
        let mut produced: Vec<u8> = Vec::new();
        for _ in 0..200 {
            if ops.is_empty() {
                break;
            }
            let number = u16::from(ops.u8() % 4);
            let pid = TABLE_PIDS[usize::from(ops.u8()) % TABLE_PIDS.len()];
            let capacity = match ops.u8() % 4 {
                0 => 0,
                1 => usize::from(ops.u16()) % buf.len(),
                _ => buf.len(),
            };
            let out = if capacity == 0 && ops.bool() { ptr::null_mut() } else { buf.as_mut_ptr() };
            let mut written = 0usize;
            match ops.u8() % 14 {
                0 => expect(
                    mpegts_program_tables_add_program(t, number, pid, ops.u16() & 0x1FFF),
                    &[MPEGTS_OK, MPEGTS_ERR_INVALID],
                    "add_program",
                ),
                1 => expect(mpegts_program_tables_remove_program(t, number), &[MPEGTS_OK, MPEGTS_ERR_NOT_FOUND], "remove_program"),
                2 => {
                    let s = spec(ops);
                    let es = ES_PIDS[usize::from(ops.u8()) % 4];
                    let status = mpegts_program_tables_add_stream(t, number, es, &s);
                    expect(status, &[MPEGTS_OK, MPEGTS_ERR_INVALID, MPEGTS_ERR_NOT_FOUND], "add_stream");
                }
                3 => {
                    let n = usize::from(ops.u16() % 300);
                    let data = ops.filled(n);
                    let (ptr, len) = if ops.u8() % 8 == 0 { (ptr::null(), 4) } else { (data.as_ptr(), data.len()) };
                    let status = if ops.bool() {
                        mpegts_program_tables_add_program_descriptor(t, number, ops.u8(), ptr, len)
                    } else {
                        mpegts_program_tables_add_stream_descriptor(t, number, ES_PIDS[0], ops.u8(), ptr, len)
                    };
                    expect(status, &[MPEGTS_OK, MPEGTS_ERR_INVALID, MPEGTS_ERR_NOT_FOUND, MPEGTS_ERR_NULL], "add_descriptor");
                }
                4 => {
                    let status = match ops.u8() % 3 {
                        0 => mpegts_program_tables_write_all(t, out, capacity, &mut written),
                        1 => mpegts_program_tables_write_pat(t, out, capacity, &mut written),
                        _ => mpegts_program_tables_write_pmt(t, number, out, capacity, &mut written),
                    };
                    expect(status, &[MPEGTS_OK, MPEGTS_ERR_BUFFER, MPEGTS_ERR_NOT_FOUND, MPEGTS_ERR_NULL], "write tables");
                    if status == MPEGTS_OK {
                        assert!(written <= capacity);
                        check_packets(&buf, written);
                        produced.extend_from_slice(&buf[..written]);
                    } else if status == MPEGTS_ERR_BUFFER {
                        assert!(written > capacity && written % 188 == 0);
                    }
                }
                5 => {
                    let status = if ops.bool() {
                        mpegts_program_tables_pat_section(t, out, capacity, &mut written)
                    } else {
                        mpegts_program_tables_pmt_section(t, number, out, capacity, &mut written)
                    };
                    expect(status, &[MPEGTS_OK, MPEGTS_ERR_BUFFER, MPEGTS_ERR_NOT_FOUND, MPEGTS_ERR_NULL], "section");
                    if status == MPEGTS_OK {
                        assert_eq!(crc32_mpeg2(&buf[..written]), 0);
                    }
                }
                6 => {
                    let s = spec(ops);
                    let mut w = ptr::null_mut();
                    let status = mpegts_pes_writer_new(ES_PIDS[usize::from(ops.u8()) % 4], &s, &mut w);
                    expect(status, &[MPEGTS_OK, MPEGTS_ERR_INVALID], "pes_writer_new");
                    assert_eq!(status == MPEGTS_OK, !w.is_null());
                    if status == MPEGTS_OK {
                        writers.push(w);
                    }
                }
                7 if !writers.is_empty() => {
                    let w = writers[usize::from(ops.u8()) % writers.len()];
                    let n = usize::from(ops.u16()) % 70_000;
                    let data = ops.filled(n);
                    let mut unit: mpegts_pes_unit = mem::zeroed();
                    unit.struct_size = struct_size::<mpegts_pes_unit>(ops);
                    unit.flags = u32::from(ops.u8());
                    unit.pts = ops.u64();
                    unit.dts = ops.u64();
                    unit.pcr = ops.u64();
                    unit.service_id = ops.u8();
                    (unit.data, unit.data_len) = match ops.u8() % 8 {
                        0 => (ptr::null(), 5),
                        _ => (data.as_ptr(), data.len() as u64),
                    };
                    let status = mpegts_pes_writer_write(w, &unit, out, capacity, &mut written);
                    expect(
                        status,
                        &[MPEGTS_OK, MPEGTS_ERR_BUFFER, MPEGTS_ERR_INVALID, MPEGTS_ERR_NULL],
                        "pes_writer_write",
                    );
                    if status == MPEGTS_OK {
                        assert!(written <= capacity);
                        check_packets(&buf, written);
                    } else if status == MPEGTS_ERR_BUFFER {
                        assert!(written > capacity && written % 188 == 0);
                    }
                }
                8 if !writers.is_empty() => {
                    let w = writers[usize::from(ops.u8()) % writers.len()];
                    let (mut cc, mut seq) = (0u8, 0u8);
                    expect(mpegts_pes_writer_continuity_counter(w, &mut cc), OK, "cc");
                    assert!(cc < 16);
                    expect(mpegts_pes_writer_set_continuity_counter(w, ops.u8() % 20), &[MPEGTS_OK, MPEGTS_ERR_INVALID], "set_cc");
                    expect(mpegts_pes_writer_mark_discontinuity(w), OK, "mark_discontinuity");
                    expect(mpegts_pes_writer_sequence_number(w, &mut seq), OK, "sequence");
                    expect(mpegts_pes_writer_set_sequence_number(w, ops.u8()), OK, "set_sequence");
                    let status = mpegts_pes_writer_write_adaptation_only(w, u32::from(ops.u8() % 64), ops.u64(), out, capacity, &mut written);
                    expect(status, &[MPEGTS_OK, MPEGTS_ERR_BUFFER, MPEGTS_ERR_INVALID, MPEGTS_ERR_NULL], "adaptation_only");
                }
                9 => {
                    let mut message = [0 as c_char; 64];
                    let len = usize::from(ops.u8() % 65);
                    expect(mpegts_program_tables_error(t, message.as_mut_ptr(), len), OK, "tables_error");
                    if len > 0 {
                        touch(message.as_ptr() as *const u8, std::ffi::CStr::from_ptr(message.as_ptr()).to_bytes().len());
                    }
                    if let Some(&w) = writers.first() {
                        expect(mpegts_pes_writer_error(w, message.as_mut_ptr(), len), OK, "writer_error");
                    }
                }
                10 => {
                    let mut v = 0u8;
                    expect(mpegts_program_tables_pat_version(t, &mut v), OK, "pat_version");
                    assert!(v < 32);
                    expect(mpegts_program_tables_pmt_version(t, number, &mut v), &[MPEGTS_OK, MPEGTS_ERR_NOT_FOUND], "pmt_version");
                    expect(mpegts_program_tables_set_pat_version(t, ops.u8() % 40), &[MPEGTS_OK, MPEGTS_ERR_INVALID], "set_pat_version");
                    expect(
                        mpegts_program_tables_set_pmt_version(t, number, ops.u8() % 40),
                        &[MPEGTS_OK, MPEGTS_ERR_INVALID, MPEGTS_ERR_NOT_FOUND],
                        "set_pmt_version",
                    );
                    let mut cc = 0u8;
                    expect(mpegts_program_tables_continuity_counter(t, pid, &mut cc), &[MPEGTS_OK, MPEGTS_ERR_NOT_FOUND], "tables_cc");
                    expect(
                        mpegts_program_tables_set_continuity_counter(t, pid, ops.u8() % 20),
                        &[MPEGTS_OK, MPEGTS_ERR_NOT_FOUND, MPEGTS_ERR_INVALID],
                        "set_tables_cc",
                    );
                }
                11 => {
                    let mut r = ptr::null_mut();
                    expect(mpegts_analyze(produced.as_ptr(), produced.len(), &mut r), OK, "analyze");
                    let mut copy = ptr::null_mut();
                    let status = mpegts_program_tables_from_report(r, &mut copy);
                    expect(status, &[MPEGTS_OK, MPEGTS_ERR_INVALID], "from_report");
                    assert_eq!(status == MPEGTS_OK, !copy.is_null());
                    mpegts_program_tables_free(copy);
                    mpegts_report_free(r);
                }
                12 => {
                    expect(mpegts_null_packet(buf.as_mut_ptr()), OK, "null_packet");
                    expect(mpegts_null_packet(ptr::null_mut()), &[MPEGTS_ERR_NULL], "null_packet(NULL)");
                    expect(mpegts_program_tables_write_all(t, out, capacity, ptr::null_mut()), &[MPEGTS_ERR_NULL], "write(NULL)");
                    expect(mpegts_program_tables_add_stream(t, 1, 0x100, ptr::null()), &[MPEGTS_ERR_NULL], "add_stream(NULL)");
                }
                _ => expect(
                    mpegts_program_tables_set_network_pid(t, pid),
                    &[MPEGTS_OK, MPEGTS_ERR_INVALID],
                    "set_network_pid",
                ),
            }
        }
        for w in writers {
            mpegts_pes_writer_free(w);
        }
        mpegts_program_tables_free(t);
    }
}
