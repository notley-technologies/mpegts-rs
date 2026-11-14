//! Extracting packets, PES packets and sections.
mod common;

use common::*;
use mpegts::{
    Codec, Config, Demuxer, Error, Output, Overflow, Select, StreamKind, Subscription, Unit,
};

fn demux_with(config: Config) -> Demuxer {
    Demuxer::with_config(config).unwrap()
}

fn sub(d: &mut Demuxer, select: Select, unit: Unit) {
    d.subscribe(Subscription::new(select, unit)).unwrap();
}

fn run(d: &mut Demuxer, data: &[u8]) -> Vec<Output> {
    d.push(data);
    d.finish();
    d.outputs().collect()
}

fn klv(n: u8) -> Vec<u8> {
    let mut k = KLV_KEY.to_vec();
    k.push(3);
    k.extend([n, n, n]);
    k
}

/// One program: H.264 on 0x100 (PCR), AAC 0x101, async KLV 0x102, sync KLV 0x103.
fn tables(ts: &mut Ts) {
    ts.section(0, &pat(1, 0, &[(1, 0x1000)]));
    ts.section(
        0x1000,
        &pmt(
            1,
            0,
            0x100,
            &[],
            &[
                (H264, 0x100, vec![]),
                (AAC, 0x101, vec![]),
                (PRIVATE, 0x102, registration(b"KLVA")),
                (METADATA, 0x103, metadata_descriptor(b"KLVA")),
            ],
        ),
    );
}

#[test]
fn async_klv_pes_payloads() {
    let mut ts = Ts::new();
    tables(&mut ts);
    for i in 0..3u8 {
        ts.pes(0x100, 0xE0, Some(u64::from(i) * 3003), &[0; 300]);
        ts.pes(0x102, 0xBD, Some(1000 + u64::from(i)), &klv(i));
    }
    let mut d = Demuxer::new();
    sub(&mut d, Select::Codec(Codec::KlvAsync), Unit::Pes);
    let out = run(&mut d, &ts.out);
    assert_eq!(out.len(), 3);
    for (i, o) in out.iter().enumerate() {
        assert_eq!((o.unit, o.pid, o.stream_id), (Unit::Pes, 0x102, Some(0xBD)));
        assert_eq!(o.pts, Some(1000 + i as u64));
        assert_eq!(o.payload(), &klv(i as u8)[..]);
        assert!(!o.damaged && !o.truncated);
    }
}

#[test]
fn bounded_pes_is_delivered_without_waiting_for_the_next() {
    let mut ts = Ts::new();
    tables(&mut ts);
    let big = vec![7u8; 1000]; // spans several packets, declares its length
    ts.pes(0x102, 0xBD, Some(5), &[&KLV_KEY[..], &big].concat());
    let mut d = Demuxer::new();
    sub(&mut d, Select::Pid(0x102), Unit::Pes);
    d.push(&ts.out);
    // No finish(), no following PES: it is already complete.
    let out: Vec<_> = d.outputs().collect();
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].payload().len(), 16 + 1000);
}

#[test]
fn unbounded_video_pes_ends_at_the_next_start_or_finish() {
    let mut ts = Ts::new();
    tables(&mut ts);
    // Video PES with PES_packet_length 0 (unbounded).
    for i in 0..2u64 {
        let mut pes = pes_bytes(0xE0, Some(i * 3003), Some(i * 3003 - i), &[i as u8; 500]);
        pes[4] = 0;
        pes[5] = 0;
        for (k, chunk) in pes.chunks(184).enumerate() {
            ts.packet_with(0x100, Opts { pusi: k == 0, ..Opts::default() }, chunk, false);
        }
    }
    let mut d = Demuxer::new();
    sub(&mut d, Select::Kind(StreamKind::Video), Unit::Pes);
    d.push(&ts.out);
    let first: Vec<_> = d.outputs().collect();
    assert_eq!(first.len(), 1, "the second PES is still open");
    assert_eq!(first[0].payload(), &[0u8; 500][..]);
    d.finish();
    let second: Vec<_> = d.outputs().collect();
    assert_eq!(second.len(), 1);
    assert_eq!((second[0].pts, second[0].dts), (Some(3003), Some(3002)));
    assert!(!second[0].damaged);
}

#[test]
fn sync_klv_access_units() {
    let mut ts = Ts::new();
    tables(&mut ts);
    let mut au = vec![0x00, 0x00, 0x0F, 0x00, 20];
    au.extend(klv(9));
    ts.pes(0x103, 0xFC, Some(42), &au);
    let mut d = Demuxer::new();
    sub(&mut d, Select::Codec(Codec::KlvSync), Unit::Pes);
    let out = run(&mut d, &ts.out);
    assert_eq!(out.len(), 1);
    assert_eq!((out[0].stream_id, out[0].pts), (Some(0xFC), Some(42)));
    assert_eq!(out[0].payload(), &au[..], "cell header and KLV, untouched");
}

/// A metadata access unit cell: service id, sequence number, flags, length.
fn cell(seq: u8, data: &[u8]) -> Vec<u8> {
    let mut c = vec![0x00, seq, 0xDF];
    c.extend_from_slice(&(data.len() as u16).to_be_bytes());
    c.extend_from_slice(data);
    c
}

#[test]
fn sync_klv_without_access_unit_headers_on_request() {
    let mut ts = Ts::new();
    tables(&mut ts);
    let au = [cell(0, &klv(1)), cell(1, &klv(2))].concat();
    ts.pes(0x103, 0xFC, Some(42), &au);
    ts.pes(0x102, 0xBD, Some(43), &klv(3));

    let mut d = Demuxer::new();
    d.subscribe(Subscription::new(Select::Kind(StreamKind::Metadata), Unit::Pes).strip_metadata_au()).unwrap();
    d.subscribe(Subscription::new(Select::Kind(StreamKind::Metadata), Unit::Pes)).unwrap();
    let out = run(&mut d, &ts.out);
    let (stripped, kept): (Vec<_>, Vec<_>) = out.iter().partition(|o| o.subscription.get() == 1);

    let sync = stripped.iter().find(|o| o.pid == 0x103).unwrap();
    assert_eq!(sync.payload(), &[klv(1), klv(2)].concat()[..], "cells joined, headers gone");
    assert_eq!((sync.pts, sync.damaged), (Some(42), false));
    assert_eq!(&sync.data[..3], &[0, 0, 1], "PES header kept");
    let async_klv = stripped.iter().find(|o| o.pid == 0x102).unwrap();
    assert_eq!(async_klv.payload(), &klv(3)[..], "async KLV has no cells to strip");
    assert_eq!(kept.iter().find(|o| o.pid == 0x103).unwrap().payload(), &au[..], "the default keeps them");
}

#[test]
fn malformed_access_unit_cells_are_flagged() {
    // Second cell claims more data than the PES holds, then stray bytes.
    let mut short = cell(0, &klv(1));
    short.extend([0x00, 0x01, 0xDF, 0x00, 50]);
    short.extend(&klv(2));
    let mut stray = cell(0, &klv(1));
    stray.extend([0x00, 0x01]);
    for (payload, kept) in [(short, [klv(1), klv(2)].concat()), (stray, klv(1))] {
        let mut ts = Ts::new();
        tables(&mut ts);
        ts.pes(0x103, 0xFC, Some(7), &payload);
        let mut d = Demuxer::new();
        d.subscribe(Subscription::new(Select::Codec(Codec::KlvSync), Unit::Pes).strip_metadata_au()).unwrap();
        let out = run(&mut d, &ts.out);
        assert_eq!(out.len(), 1);
        assert!(out[0].damaged);
        assert_eq!(out[0].payload(), &kept[..], "data that is present is kept");
    }
}

#[test]
fn table_sections_with_crc_check() {
    let mut ts = Ts::new();
    tables(&mut ts);
    ts.section(1, &section(0x01, 0xFFFF, 0, &[0x09, 4, 0x0B, 0x00, 0xE1, 0x23]));
    let mut bad = pat(1, 0, &[(1, 0x1000)]);
    let last = bad.len() - 1;
    bad[last] ^= 0xFF;
    ts.section(0, &bad);
    let mut d = Demuxer::new();
    sub(&mut d, Select::Psi, Unit::Sections);
    let out = run(&mut d, &ts.out);
    let got: Vec<_> = out.iter().map(|o| (o.pid, o.table_id, o.crc_ok)).collect();
    assert_eq!(
        got,
        [
            (0, Some(0x00), Some(true)),
            (0x1000, Some(0x02), Some(true)),
            (1, Some(0x01), Some(true)),
            (0, Some(0x00), Some(false)),
        ]
    );
    assert_eq!(out[1].data, pmt(1, 0, 0x100, &[], &[
        (H264, 0x100, vec![]),
        (AAC, 0x101, vec![]),
        (PRIVATE, 0x102, registration(b"KLVA")),
        (METADATA, 0x103, metadata_descriptor(b"KLVA")),
    ]));
}

#[test]
fn private_sections_on_any_pid() {
    // DVB SDT-like private section of 1500 bytes on PID 0x11.
    let body = vec![0x5A; 1500];
    let sdt = section(0x42, 1, 0, &body);
    let mut ts = Ts::new();
    ts.section(0x11, &sdt);
    ts.null();
    let mut d = Demuxer::new();
    sub(&mut d, Select::Psi, Unit::Sections);
    let out = run(&mut d, &ts.out);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].data, sdt);
    assert_eq!((out[0].table_id, out[0].crc_ok), (Some(0x42), Some(true)));
}

#[test]
fn raw_packets_including_errors_and_duplicates() {
    let mut ts = Ts::new();
    tables(&mut ts);
    let pl = [0u8; 184];
    ts.packet_with(0x101, Opts { cc: Some(0), ..Opts::default() }, &pl, false);
    ts.packet_with(0x101, Opts { cc: Some(0), ..Opts::default() }, &pl, false); // duplicate
    ts.packet_with(0x101, Opts { cc: Some(1), tei: true, ..Opts::default() }, &pl, false);
    ts.packet_with(0x101, Opts { cc: Some(2), scrambling: 3, ..Opts::default() }, &pl, false);
    ts.null();
    let mut d = Demuxer::new();
    sub(&mut d, Select::Pid(0x101), Unit::Packets);
    let out = run(&mut d, &ts.out);
    assert_eq!(out.len(), 4);
    let input: Vec<_> = ts.out.chunks(188).filter(|p| (u16::from(p[1] & 0x1F) << 8 | u16::from(p[2])) == 0x101).collect();
    for (o, p) in out.iter().zip(input) {
        assert_eq!(o.data, p);
    }

    let mut all = Demuxer::new();
    sub(&mut all, Select::All, Unit::Packets);
    let out = run(&mut all, &ts.out);
    assert_eq!(out.len(), ts.out.len() / 188, "every packet, null and TEI included");
    let packets: Vec<_> = out.iter().map(|o| o.packet).collect();
    assert_eq!(packets, (1..=out.len() as u64).collect::<Vec<_>>());
}

#[test]
fn program_selection_covers_pmt_pcr_and_streams() {
    let mut ts = Ts::new();
    ts.section(0, &pat(1, 0, &[(1, 0x1000), (2, 0x2000)]));
    ts.section(0x1000, &pmt(1, 0, 0x1FE, &[], &[(H264, 0x100, vec![])])); // PCR on its own PID
    ts.section(0x2000, &pmt(2, 0, 0x200, &[], &[(H264, 0x200, vec![])]));
    ts.pcr(0x1FE, 0);
    ts.pes(0x100, 0xE0, None, &[0; 10]);
    ts.pes(0x200, 0xE0, None, &[0; 10]);
    ts.section(0x1000, &pmt(1, 0, 0x1FE, &[], &[(H264, 0x100, vec![])]));
    let mut d = Demuxer::new();
    sub(&mut d, Select::Program(1), Unit::Packets);
    let out = run(&mut d, &ts.out);
    let pids: Vec<_> = out.iter().map(|o| o.pid).collect();
    // The PAT comes first, so every program-1 packet is delivered,
    // including the first PMT.
    assert_eq!(pids, [0x1000, 0x1FE, 0x100, 0x1000]);
}

#[test]
fn selections_follow_table_changes() {
    let mut d = Demuxer::new();
    sub(&mut d, Select::Codec(Codec::KlvAsync), Unit::Pes);
    let mut ts = Ts::new();
    ts.section(0, &pat(1, 0, &[(1, 0x1000)]));
    ts.section(0x1000, &pmt(1, 0, 0x100, &[], &[(H264, 0x100, vec![])]));
    ts.pes(0x102, 0xBD, Some(1), &klv(1)); // not declared yet
    ts.section(0x1000, &pmt(1, 1, 0x100, &[], &[(H264, 0x100, vec![]), (PRIVATE, 0x102, registration(b"KLVA"))]));
    ts.pes(0x102, 0xBD, Some(2), &klv(2));
    ts.section(0x1000, &pmt(1, 2, 0x100, &[], &[(H264, 0x100, vec![])])); // KLV removed
    ts.pes(0x102, 0xBD, Some(3), &klv(3));
    let out = run(&mut d, &ts.out);
    let pts: Vec<_> = out.iter().map(|o| o.pts).collect();
    assert_eq!(pts, [Some(2)]);
}

#[test]
fn identified_from_payload_then_followed() {
    // Undeclared KLV: identified from its first PES, which is also delivered.
    let mut ts = Ts::new();
    ts.section(0, &pat(1, 0, &[(1, 0x1000)]));
    ts.section(0x1000, &pmt(1, 0, 0x100, &[], &[(PRIVATE, 0x102, vec![])]));
    ts.pes(0x102, 0xBD, Some(1), &klv(1));
    ts.pes(0x102, 0xBD, Some(2), &klv(2));
    let mut d = Demuxer::new();
    sub(&mut d, Select::Codec(Codec::KlvAsync), Unit::Pes);
    let out = run(&mut d, &ts.out);
    let pts: Vec<_> = out.iter().map(|o| o.pts).collect();
    assert_eq!(pts, [Some(1), Some(2)]);
}

#[test]
fn replay_from_history() {
    let mut ts = Ts::new();
    tables(&mut ts);
    ts.pes(0x102, 0xBD, Some(1), &klv(1));
    ts.pes(0x102, 0xBD, Some(2), &klv(2));
    ts.null();
    ts.null();

    let mut config = Config::default();
    config.history_packets = 100;
    let mut d = demux_with(config.clone());
    d.push(&ts.out);
    assert_eq!(d.recent_packets().count(), ts.out.len() / 188);
    d.subscribe(Subscription::new(Select::Codec(Codec::KlvAsync), Unit::Pes).with_replay()).unwrap();
    let pts: Vec<_> = d.outputs().map(|o| o.pts).collect();
    assert_eq!(pts, [Some(1), Some(2)]);

    let mut late = demux_with(config);
    late.push(&ts.out);
    late.subscribe(Subscription::new(Select::Codec(Codec::KlvAsync), Unit::Pes)).unwrap();
    assert_eq!(late.outputs().count(), 0, "no replay asked for");

    // History is bounded.
    let mut small = Config::default();
    small.history_packets = 3;
    let mut d = demux_with(small);
    d.push(&ts.out);
    d.finish();
    let kept: Vec<_> = d.recent_packets().map(|(n, _)| n).collect();
    let total = (ts.out.len() / 188) as u64;
    assert_eq!(kept, [total - 2, total - 1, total]);
}

#[test]
fn damaged_pes_is_flagged() {
    let mut ts = Ts::new();
    tables(&mut ts);
    let data = vec![1u8; 600];
    let start = ts.out.len();
    ts.pes(0x102, 0xBD, Some(1), &[&KLV_KEY[..], &data].concat()); // 4 packets
    ts.out.drain(start + 188..start + 376); // lose the second
    ts.pes(0x102, 0xBD, Some(2), &klv(2));
    let mut d = Demuxer::new();
    sub(&mut d, Select::Pid(0x102), Unit::Pes);
    let out = run(&mut d, &ts.out);
    assert_eq!(out.len(), 2);
    assert!(out[0].damaged, "gap inside, and shorter than declared");
    assert!(out[0].data.len() < 6 + 3 + 5 + 616);
    assert!(!out[1].damaged);
    assert_eq!(d.output_stats().damaged, 1);
}

#[test]
fn oversized_pes_is_truncated() {
    let mut config = Config::default();
    config.max_pes_bytes = 1000;
    config.max_reassembly_bytes = 1000;
    let mut ts = Ts::new();
    tables(&mut ts);
    ts.pes(0x102, 0xBD, Some(1), &vec![9u8; 3000]);
    ts.pes(0x102, 0xBD, Some(2), &klv(2));
    let mut d = demux_with(config);
    sub(&mut d, Select::Pid(0x102), Unit::Pes);
    let out = run(&mut d, &ts.out);
    assert_eq!(out.len(), 2);
    assert!(out[0].truncated && !out[0].damaged);
    assert_eq!(out[0].data.len(), 1000);
    assert_eq!(out[0].pts, Some(1));
    assert!(!out[1].truncated);
    let stats = d.output_stats();
    assert_eq!((stats.truncated, stats.reassembly_bytes), (1, 0));
}

#[test]
fn reassembly_budget_is_shared() {
    let mut config = Config::default();
    config.max_pes_bytes = 1000;
    config.max_reassembly_bytes = 1000;
    let mut ts = Ts::new();
    tables(&mut ts);
    // Two unbounded PES in flight at once, 600 bytes each.
    for pid in [0x100, 0x101] {
        let mut pes = pes_bytes(0xE0, Some(1), None, &[1; 600]);
        pes[4] = 0;
        pes[5] = 0;
        for (k, chunk) in pes.chunks(184).enumerate() {
            ts.packet_with(pid, Opts { pusi: k == 0, ..Opts::default() }, chunk, false);
        }
    }
    let mut d = demux_with(config);
    sub(&mut d, Select::Pid(0x100), Unit::Pes);
    sub(&mut d, Select::Pid(0x101), Unit::Pes);
    d.push(&ts.out);
    assert!(d.output_stats().reassembly_bytes <= 1000);
    d.finish();
    let out: Vec<_> = d.outputs().collect();
    assert_eq!(out.len(), 2);
    assert!(!out[0].truncated && out[1].truncated);
    assert_eq!(d.output_stats().reassembly_bytes, 0);
}

#[test]
fn queue_limits_and_overflow_policies() {
    let mut ts = Ts::new();
    tables(&mut ts);
    for i in 0..10u8 {
        ts.pes(0x102, 0xBD, Some(u64::from(i)), &klv(i));
    }
    for (policy, expect) in [(Overflow::DropNewest, [0u64, 1, 2]), (Overflow::DropOldest, [7, 8, 9])] {
        let mut config = Config::default();
        config.queue_max_items = 3;
        config.overflow = policy;
        let mut d = demux_with(config);
        sub(&mut d, Select::Pid(0x102), Unit::Pes);
        let out = run(&mut d, &ts.out);
        let pts: Vec<_> = out.iter().map(|o| o.pts.unwrap()).collect();
        assert_eq!(pts, expect);
        let stats = d.output_stats();
        assert_eq!((stats.dropped, stats.pending), (7, 0));
    }

    let mut config = Config::default();
    config.queue_max_bytes = 188 * 2;
    let mut d = demux_with(config);
    sub(&mut d, Select::All, Unit::Packets);
    d.push(&ts.out);
    let stats = d.output_stats();
    assert_eq!((stats.pending, stats.pending_bytes), (2, 376));
}

#[test]
fn unsubscribe_stops_delivery_and_frees_buffers() {
    let mut ts = Ts::new();
    tables(&mut ts);
    let mut pes = pes_bytes(0xE0, Some(1), None, &[1; 600]);
    pes[4] = 0;
    pes[5] = 0;
    for (k, chunk) in pes.chunks(184).enumerate() {
        ts.packet_with(0x100, Opts { pusi: k == 0, ..Opts::default() }, chunk, false);
    }
    let mut d = Demuxer::new();
    let id = d.subscribe(Subscription::new(Select::Pid(0x100), Unit::Pes)).unwrap();
    d.push(&ts.out);
    assert!(d.output_stats().reassembly_bytes > 0);
    assert!(d.unsubscribe(id));
    assert!(!d.unsubscribe(id));
    let stats = d.output_stats();
    assert_eq!((stats.reassembly_bytes, stats.subscriptions), (0, 0));
    d.finish();
    assert_eq!(d.outputs().count(), 0);
}

#[test]
fn units_only_apply_where_they_fit() {
    let mut ts = Ts::new();
    ts.section(0, &pat(1, 0, &[(1, 0x1000)]));
    ts.section(0x1000, &pmt(1, 0, 0x100, &[], &[(H264, 0x100, vec![]), (0x86, 0x101, vec![])]));
    ts.pes(0x100, 0xE0, Some(1), &[0; 20]);
    let splice = section(0xFC, 0, 0, &[0; 20]); // SCTE-35 sections
    ts.section(0x101, &splice);
    ts.null();
    let mut d = Demuxer::new();
    sub(&mut d, Select::Program(1), Unit::Sections);
    let mut pes = Demuxer::new();
    sub(&mut pes, Select::Program(1), Unit::Pes);
    let sections: Vec<_> = run(&mut d, &ts.out).iter().map(|o| (o.pid, o.table_id)).collect();
    assert_eq!(sections, [(0x1000, Some(0x02)), (0x101, Some(0xFC))]);
    let pes: Vec<_> = run(&mut pes, &ts.out).iter().map(|o| o.pid).collect();
    assert_eq!(pes, [0x100]);
}

#[test]
fn shared_pid_is_delivered_once_per_subscription() {
    let mut ts = Ts::new();
    ts.section(0, &pat(1, 0, &[(1, 0x1000), (2, 0x1001)]));
    ts.section(0x1000, &pmt(1, 0, 0x100, &[], &[(AAC, 0x300, vec![])]));
    ts.section(0x1001, &pmt(2, 0, 0x100, &[], &[(AAC, 0x300, vec![])]));
    ts.pes(0x300, 0xC0, Some(1), &[0; 20]);
    let mut d = Demuxer::new();
    sub(&mut d, Select::Kind(StreamKind::Audio), Unit::Pes);
    assert_eq!(run(&mut d, &ts.out).len(), 1);
}

#[test]
fn split_pes_header_is_reassembled_for_output() {
    let mut ts = Ts::new();
    tables(&mut ts);
    let pes = pes_bytes(0xBD, Some(77), None, &klv(4));
    ts.packet_with(0x102, Opts { pusi: true, ..Opts::default() }, &pes[..5], false);
    ts.packet_with(0x102, Opts::default(), &pes[5..], false);
    let mut d = Demuxer::new();
    sub(&mut d, Select::Codec(Codec::KlvAsync), Unit::Pes);
    let out = run(&mut d, &ts.out);
    assert_eq!(out.len(), 1);
    assert_eq!((out[0].pts, out[0].payload()), (Some(77), &klv(4)[..]));
}

#[test]
fn chunking_does_not_change_outputs() {
    let mut ts = Ts::new();
    tables(&mut ts);
    for i in 0..20u8 {
        ts.pcr(0x100, u64::from(i) * 2_700_000);
        ts.pes(0x100, 0xE0, Some(u64::from(i) * 3003), &vec![i; 700]);
        ts.pes(0x102, 0xBD, Some(u64::from(i)), &klv(i));
    }
    let collect = |chunks: &mut dyn FnMut(&mut Demuxer)| {
        let mut d = Demuxer::new();
        sub(&mut d, Select::All, Unit::Packets);
        sub(&mut d, Select::All, Unit::Pes);
        sub(&mut d, Select::Psi, Unit::Sections);
        chunks(&mut d);
        d.finish();
        d.outputs().collect::<Vec<_>>()
    };
    let whole = collect(&mut |d| d.push(&ts.out));
    let mut rng = Rng(5);
    for _ in 0..30 {
        let got = collect(&mut |d| {
            let mut rest = &ts.out[..];
            while !rest.is_empty() {
                let n = (1 + rng.below(500)).min(rest.len());
                d.push(&rest[..n]);
                rest = &rest[n..];
            }
        });
        assert_eq!(got, whole);
    }
}

#[test]
fn recycled_buffers_are_reused() {
    let mut ts = Ts::new();
    tables(&mut ts);
    let mut d = Demuxer::new();
    sub(&mut d, Select::All, Unit::Packets);
    d.push(&ts.out);
    while let Some(out) = d.pop_output() {
        d.recycle(out);
    }
    d.push(&ts.out);
    assert!(d.outputs().all(|o| o.data.len() == 188));
}

#[test]
fn config_validation() {
    let check = |f: fn(&mut Config), field: &str| {
        let mut c = Config::default();
        f(&mut c);
        match Demuxer::with_config(c) {
            Err(Error::InvalidConfig { field: got, .. }) => assert_eq!(got, field),
            other => panic!("{field}: {other:?}"),
        }
    };
    check(|c| c.sync_confirmations = 0, "sync_confirmations");
    check(|c| c.sync_confirmations = 9, "sync_confirmations");
    check(|c| c.max_streams = 0, "max_streams");
    check(|c| c.history_packets = usize::MAX, "history_packets");
    check(|c| c.queue_max_items = 0, "queue_max_items");
    check(|c| c.queue_max_bytes = 100, "queue_max_bytes");
    check(|c| c.max_pes_bytes = 10, "max_pes_bytes");
    check(|c| c.max_reassembly_bytes = 200, "max_reassembly_bytes");
    assert!(Demuxer::with_config(Config::default()).is_ok());
    assert!(Error::InvalidSubscription("x").to_string().contains("subscription"));
}

#[test]
fn subscription_validation() {
    let mut d = Demuxer::new();
    assert!(d.subscribe(Subscription::new(Select::Pid(0x2000), Unit::Packets)).is_err());
    for _ in 0..256 {
        d.subscribe(Subscription::new(Select::Pid(0), Unit::Sections)).unwrap();
    }
    assert!(d.subscribe(Subscription::new(Select::Pid(0), Unit::Sections)).is_err());
}

#[test]
fn stricter_sync_needs_more_evidence() {
    let mut ts = Ts::new();
    tables(&mut ts);
    for _ in 0..10 {
        ts.null();
    }
    let mut config = Config::default();
    config.sync_confirmations = 8;
    let mut d = demux_with(config);
    d.push(&ts.out[..188 * 5]);
    assert_eq!(d.report().stats.packets, 0, "5 packets cannot confirm 8");
    d.push(&ts.out[188 * 5..]);
    d.push(&[0x47; 0]);
    d.finish();
    assert_eq!(d.report().stats.packets, (ts.out.len() / 188) as u64);
}

#[test]
fn probe_depth_is_configurable() {
    // The first PES of an undeclared stream is not KLV; the second is.
    let mut ts = Ts::new();
    ts.section(0, &pat(1, 0, &[(1, 0x1000)]));
    ts.section(0x1000, &pmt(1, 0, 0x100, &[], &[(PRIVATE, 0x102, vec![])]));
    ts.pes(0x102, 0xBD, None, &[0xAB; 20]);
    ts.pes(0x102, 0xBD, None, &klv(1));
    let mut one = Config::default();
    one.probe_pes = 1;
    let mut d = demux_with(one);
    d.push(&ts.out);
    d.finish();
    assert_eq!(d.report().stream(0x102).unwrap().codec, Codec::PrivateData);
    let mut d = Demuxer::new(); // default probes 4
    d.push(&ts.out);
    d.finish();
    assert_eq!(d.report().stream(0x102).unwrap().codec, Codec::KlvAsync);
}

#[test]
fn no_panics_and_bounded_under_hostile_input() {
    let mut base = Ts::new();
    tables(&mut base);
    for i in 0..30u8 {
        base.pcr(0x100, u64::from(i) * 2_700_000);
        base.pes(0x100, 0xE0, Some(u64::from(i) * 3003), &vec![i; 900]);
        base.pes(0x102, 0xBD, Some(u64::from(i)), &klv(i));
        base.section(0x11, &section(0x42, 1, 0, &[i; 300]));
    }
    let mut config = Config::default();
    config.history_packets = 50;
    config.queue_max_items = 40;
    config.queue_max_bytes = 20_000;
    config.max_pes_bytes = 2000;
    config.max_reassembly_bytes = 3000;
    config.overflow = Overflow::DropOldest;
    let mut rng = Rng(0xFEED);
    for round in 0..200 {
        let mut data = base.out.clone();
        for _ in 0..rng.below(60) {
            let i = rng.below(data.len());
            data[i] ^= 1 << rng.below(8);
        }
        if round % 3 == 0 {
            let cut = rng.below(data.len());
            data.truncate(cut);
        }
        let mut d = Demuxer::with_config(config.clone()).unwrap();
        for (select, unit) in [
            (Select::All, Unit::Packets),
            (Select::All, Unit::Pes),
            (Select::All, Unit::Sections),
            (Select::Psi, Unit::Sections),
            (Select::Pid(0x11), Unit::Pes),
        ] {
            d.subscribe(Subscription::new(select, unit).with_replay()).unwrap();
        }
        let mut rest = &data[..];
        while !rest.is_empty() {
            let n = (1 + rng.below(2000)).min(rest.len());
            d.push(&rest[..n]);
            rest = &rest[n..];
            let s = d.output_stats();
            assert!(s.pending <= 40 && s.pending_bytes <= 20_000 && s.reassembly_bytes <= 3000);
            if rng.below(4) == 0 {
                let _ = d.pop_output();
            }
        }
        // A late subscription replaying damaged history must not panic either.
        d.subscribe(Subscription::new(Select::All, Unit::Pes).with_replay()).unwrap();
        d.finish();
        assert_eq!(d.output_stats().reassembly_bytes, 0);
        for o in d.outputs() {
            assert!(o.data.len() <= 4096 || o.unit == Unit::Pes);
            assert!(o.payload_offset <= o.data.len());
        }
    }
}

#[test]
fn types_are_send() {
    fn send<T: Send + Sync>() {}
    send::<Demuxer>();
    send::<Output>();
    send::<mpegts::Report>();
}
