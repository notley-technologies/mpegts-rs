//! PSI edge cases: section reassembly, versioning, malformed tables.
mod common;

use common::*;
use mpegts::{analyze, Codec, Demuxer};

#[test]
fn multi_section_pat() {
    let s0 = section_full(0x00, 1, 0, true, 0, 1, &pat_body(&[(1, 0x1000)]));
    let s1 = section_full(0x00, 1, 0, true, 1, 1, &pat_body(&[(2, 0x1001)]));
    let mut first = Ts::new();
    first.section(0, &s0);
    let r = analyze(&first.out);
    assert!(r.programs.is_empty(), "incomplete PAT is not applied");

    first.section(0, &s1);
    let r = analyze(&first.out);
    let nums: Vec<_> = r.programs.iter().map(|p| p.number).collect();
    assert_eq!(nums, [1, 2]);
}

#[test]
fn network_pid_from_program_zero() {
    let mut ts = Ts::new();
    ts.section(0, &pat(1, 0, &[(0, 0x0010), (1, 0x1000)]));
    let r = analyze(&ts.out);
    assert_eq!(r.network_pid, Some(0x0010));
    assert_eq!(r.programs.len(), 1);
}

#[test]
fn two_pmts_share_a_pid_and_a_packet() {
    let a = pmt(1, 0, 0x100, &[], &[(H264, 0x100, vec![])]);
    let b = pmt(2, 0, 0x200, &[], &[(H265, 0x200, vec![])]);
    let mut payload = vec![0];
    payload.extend(&a);
    payload.extend(&b);
    let mut ts = Ts::new();
    ts.section(0, &pat(1, 0, &[(1, 0x1000), (2, 0x1000)]));
    ts.psi_raw(0x1000, true, &payload);
    let r = analyze(&ts.out);
    assert_eq!(r.program(1).unwrap().streams[0].codec, Codec::H264);
    assert_eq!(r.program(2).unwrap().streams[0].codec, Codec::H265);
}

#[test]
fn pointer_field_finishes_previous_section() {
    // PMT v0 spans two packets; the second packet also starts PMT v1.
    let streams: Vec<_> = (0..40).map(|i| (H264, 0x100 + i, vec![])).collect();
    let v0 = pmt(1, 0, 0x100, &[], &streams);
    let v1 = pmt(1, 1, 0x200, &[], &[(H265, 0x200, vec![])]);
    assert!(v0.len() > 183 && v0.len() - 183 < 100);
    let mut ts = Ts::new();
    ts.section(0, &pat(1, 0, &[(1, 0x1000)]));
    let mut p1 = vec![0];
    p1.extend(&v0[..183]);
    ts.psi_raw(0x1000, true, &p1);
    let rest = &v0[183..];
    let mut p2 = vec![rest.len() as u8];
    p2.extend(rest);
    p2.extend(&v1);
    ts.psi_raw(0x1000, true, &p2);

    let mut d = Demuxer::new();
    d.push(&ts.out);
    d.finish();
    let r = d.report();
    assert_eq!(r.revision, 3, "PAT, PMT v0 and PMT v1 each applied");
    assert_eq!(r.programs[0].version, Some(1));
    assert_eq!(r.programs[0].streams[0].codec, Codec::H265);
}

#[test]
fn continuation_without_start_is_ignored() {
    let sec = pmt(1, 0, 0x100, &[], &[(H264, 0x100, vec![])]);
    let mut ts = Ts::new();
    ts.section(0, &pat(1, 0, &[(1, 0x1000)]));
    ts.psi_raw(0x1000, false, &sec);
    let r = analyze(&ts.out);
    assert_eq!(r.programs[0].version, None);
}

#[test]
fn next_not_current_tables_are_ignored() {
    let body = {
        let full = pmt(1, 0, 0x100, &[], &[(H264, 0x100, vec![])]);
        full[8..full.len() - 4].to_vec()
    };
    let mut ts = Ts::new();
    ts.section(0, &pat(1, 0, &[(1, 0x1000)]));
    ts.section(0x1000, &section_full(0x02, 1, 0, false, 0, 0, &body));
    assert_eq!(analyze(&ts.out).programs[0].version, None);
}

#[test]
fn wrong_table_or_program_is_ignored() {
    let mut ts = Ts::new();
    ts.section(0, &pat(1, 0, &[(1, 0x1000)]));
    ts.section(0x1000, &pmt(9, 0, 0x100, &[], &[(H264, 0x100, vec![])])); // other program
    let mut bad_tid = pmt(1, 0, 0x100, &[], &[(H264, 0x100, vec![])]);
    bad_tid[0] = 0x42;
    ts.section(0x1000, &bad_tid);
    let r = analyze(&ts.out);
    assert_eq!(r.programs[0].version, None);
    assert_eq!(r.stats.crc_errors, 0);
}

#[test]
fn corrupt_pat_is_rejected_and_counted() {
    let mut sec = pat(1, 0, &[(1, 0x1000)]);
    let last = sec.len() - 1;
    sec[last] ^= 1;
    let mut ts = Ts::new();
    ts.section(0, &sec);
    let r = analyze(&ts.out);
    assert!(r.programs.is_empty());
    assert_eq!(r.stats.crc_errors, 1);
}

#[test]
fn transport_error_packets_are_skipped() {
    let sec = pmt(1, 0, 0x100, &[], &[(H264, 0x100, vec![])]);
    let mut payload = vec![0];
    payload.extend(&sec);
    let mut ts = Ts::new();
    ts.section(0, &pat(1, 0, &[(1, 0x1000)]));
    ts.packet_with(0x1000, Opts { pusi: true, tei: true, ..Opts::default() }, &payload, true);
    let r = analyze(&ts.out);
    assert_eq!(r.programs[0].version, None);
    let pid = r.pids.iter().find(|p| p.pid == 0x1000).unwrap();
    assert_eq!((pid.packets, pid.transport_errors), (1, 1));
}

#[test]
fn pat_update_moves_pmt_pid() {
    let mut ts = Ts::new();
    ts.section(0, &pat(1, 0, &[(1, 0x1000)]));
    ts.section(0x1000, &pmt(1, 0, 0x100, &[], &[(H264, 0x100, vec![])]));
    ts.section(0, &pat(1, 1, &[(1, 0x1100)]));
    let r = analyze(&ts.out);
    assert_eq!(r.programs[0].pmt_pid, 0x1100);
    assert_eq!(r.programs[0].version, None, "PMT must be re-acquired on the new PID");

    ts.section(0x1100, &pmt(1, 3, 0x100, &[], &[(H265, 0x100, vec![])]));
    ts.section(0x1000, &pmt(1, 4, 0x100, &[], &[(H264, 0x100, vec![])])); // stale PID
    let r = analyze(&ts.out);
    assert_eq!(r.programs[0].version, Some(3));
    assert_eq!(r.programs[0].streams[0].codec, Codec::H265);
}

#[test]
fn transport_stream_id_change_is_reported() {
    let mut d = Demuxer::new();
    let mut ts = Ts::new();
    ts.section(0, &pat(1, 0, &[(1, 0x1000)]));
    ts.section(0, &pat(2, 0, &[(1, 0x1000)]));
    ts.null();
    ts.null();
    d.push(&ts.out);
    assert_eq!(d.report().transport_stream_id, Some(2));
    assert_eq!(d.revision(), 2);
}

#[test]
fn stream_counters_survive_pmt_changes() {
    let mut ts = Ts::new();
    ts.section(0, &pat(1, 0, &[(1, 0x1000)]));
    ts.section(0x1000, &pmt(1, 0, 0x100, &[], &[(H264, 0x100, vec![])]));
    ts.pes(0x100, 0xE0, Some(100), &[0; 10]);
    ts.section(0x1000, &pmt(1, 1, 0x100, &[], &[(H264, 0x100, vec![]), (AAC, 0x101, vec![])]));
    ts.pes(0x100, 0xE0, Some(200), &[0; 10]);
    let r = analyze(&ts.out);
    let v = r.stream(0x100).unwrap();
    assert_eq!(v.packets, 2);
    assert_eq!((v.first_pts, v.last_pts, v.pts_span), (Some(100), Some(200), Some(100)));
    assert_eq!(r.stream(0x101).unwrap().packets, 0);
}

#[test]
fn malformed_pmt_loops_do_not_panic() {
    let full = pmt(1, 0, 0x100, &[], &[(H264, 0x100, vec![])]);
    let body = full[8..full.len() - 4].to_vec();
    let cases: Vec<Vec<u8>> = vec![
        vec![],                                     // empty body
        vec![0xE1, 0x00],                           // truncated header
        vec![0xE1, 0x00, 0xF0, 0xFF],               // program_info_length overruns
        [&body[..], &[0x1B, 0xE2, 0x00, 0xFF]].concat(), // truncated ES entry
        [&body[..], &[0x1B, 0xE2, 0x00, 0xF0, 0x50]].concat(), // es_info overruns
        vec![0xE1, 0x00, 0xF0, 0x03, 0x05, 0x09, 0x00], // descriptor overruns
    ];
    for body in cases {
        let mut ts = Ts::new();
        ts.section(0, &pat(1, 0, &[(1, 0x1000)]));
        ts.section(0x1000, &section(0x02, 1, 0, &body));
        let _ = analyze(&ts.out);
    }
}

#[test]
fn pmt_on_pat_pid_does_not_hijack_pat() {
    let mut ts = Ts::new();
    ts.section(0, &pat(1, 0, &[(1, 0x0000), (2, 0x1001)]));
    ts.section(0x1001, &pmt(2, 0, 0x200, &[], &[(H265, 0x200, vec![]), (H264, 0x1001, vec![])]));
    ts.section(0, &pat(1, 0, &[(1, 0x0000), (2, 0x1001)]));
    let r = analyze(&ts.out);
    assert_eq!(r.programs.len(), 2);
    assert_eq!(r.program(2).unwrap().version, Some(0));
}
