//! Framing, continuity and hostile-input behaviour.
mod common;

use common::*;
use mpegts::{analyze, Codec, Demuxer, Report};

fn sample() -> Vec<u8> {
    let mut ts = Ts::new();
    for i in 0..20u64 {
        ts.section(0, &pat(1, 0, &[(1, 0x1000)]));
        ts.section(
            0x1000,
            &pmt(
                1,
                0,
                0x100,
                &[],
                &[(H264, 0x100, vec![]), (AAC, 0x101, vec![]), (PRIVATE, 0x102, registration(b"KLVA"))],
            ),
        );
        ts.pcr(0x100, i * 2_700_000);
        ts.pes(0x100, 0xE0, Some(i * 3003), &[0x11; 400]);
        ts.pes(0x101, 0xC0, Some(i * 1920), &[0x22; 50]);
        let mut klv = KLV_KEY.to_vec();
        klv.extend([3, 1, 2, 3]);
        ts.pes(0x102, 0xBD, Some(i * 3003), &klv);
        ts.null();
    }
    ts.out
}

fn codecs(r: &Report) -> Vec<Codec> {
    r.streams().map(|s| s.codec).collect()
}

fn push_in_random_chunks(data: &[u8], rng: &mut Rng) -> Report {
    let mut d = Demuxer::new();
    let mut rest = data;
    while !rest.is_empty() {
        let n = (1 + rng.below(700)).min(rest.len());
        d.push(&rest[..n]);
        rest = &rest[n..];
    }
    d.finish();
    d.report()
}

#[test]
fn random_chunking_matches_whole_buffer() {
    let data = sample();
    let whole = analyze(&data);
    assert_eq!(codecs(&whole), [Codec::H264, Codec::AacAdts, Codec::KlvAsync]);
    let mut rng = Rng(0x1234_5678);
    for _ in 0..200 {
        assert_eq!(push_in_random_chunks(&data, &mut rng), whole);
    }
}

#[test]
fn random_chunking_with_m2ts_and_rs_framing() {
    let data = sample();
    for (pre, post) in [(4usize, 0usize), (0, 16)] {
        let mut framed = Vec::new();
        for p in data.chunks(188) {
            framed.extend(std::iter::repeat(0xAB).take(pre));
            framed.extend_from_slice(p);
            framed.extend(std::iter::repeat(0xCD).take(post));
        }
        let whole = analyze(&framed);
        assert_eq!(whole.stats.packet_size, Some(188 + pre + post));
        assert_eq!(whole.stats.sync_losses, 0);
        let mut rng = Rng(99);
        for _ in 0..50 {
            assert_eq!(push_in_random_chunks(&framed, &mut rng), whole);
        }
    }
}

#[test]
fn empty_pushes_are_harmless() {
    let data = sample();
    let mut d = Demuxer::new();
    for c in data.chunks(1000) {
        d.push(&[]);
        d.push(c);
    }
    d.finish();
    assert_eq!(d.report(), analyze(&data));
}

#[test]
fn finish_then_continue() {
    let data = sample();
    let mut d = Demuxer::new();
    let half = 188 * 50 + 77;
    d.push(&data[..half]);
    d.finish();
    let mid = d.report();
    assert_eq!(codecs(&mid), [Codec::H264, Codec::AacAdts, Codec::KlvAsync]);
    d.push(&data[half..]);
    d.finish();
    let r = d.report();
    assert_eq!(codecs(&r), codecs(&mid));
    // The packet cut by `finish` is lost, everything else is counted.
    assert_eq!(r.stats.packets, (data.len() / 188) as u64 - 1);
}

#[test]
fn garbage_between_packets_is_skipped() {
    let data = sample();
    let mut rng = Rng(7);
    let mut dirty = Vec::new();
    let mut inserted = 0;
    for (i, p) in data.chunks(188).enumerate() {
        if i % 37 == 5 {
            // Garbage with no sync bytes.
            let n = 1 + rng.below(300);
            dirty.extend(std::iter::repeat(0x00).take(n));
            inserted += n;
        }
        dirty.extend_from_slice(p);
    }
    let r = analyze(&dirty);
    assert_eq!(codecs(&r), [Codec::H264, Codec::AacAdts, Codec::KlvAsync]);
    assert_eq!(r.stats.bytes_skipped, inserted as u64);
    assert_eq!(r.stats.packets, (data.len() / 188) as u64);
    assert!(r.stats.sync_losses > 0);
}

#[test]
fn no_panics_on_random_bytes() {
    let mut rng = Rng(42);
    for _ in 0..300 {
        let len = rng.below(5000);
        let mut data: Vec<u8> = (0..len).map(|_| rng.next() as u8).collect();
        // Sprinkle sync bytes on a 188 grid sometimes to get past framing.
        if rng.below(2) == 0 {
            for i in (0..data.len()).step_by(188) {
                data[i] = 0x47;
            }
        }
        let _ = analyze(&data);
        let _ = push_in_random_chunks(&data, &mut rng);
    }
}

#[test]
fn no_panics_on_bit_flips() {
    let base = sample();
    let mut rng = Rng(0xDEAD_BEEF);
    for _ in 0..300 {
        let mut data = base.clone();
        for _ in 0..1 + rng.below(40) {
            let i = rng.below(data.len());
            data[i] ^= 1 << rng.below(8);
        }
        let r = analyze(&data);
        assert!(r.stats.packets > 0);
        let _ = push_in_random_chunks(&data, &mut rng);
    }
}

#[test]
fn no_panics_on_any_truncation() {
    let base = sample();
    let small = &base[..188 * 12];
    for cut in 0..small.len() {
        let _ = analyze(&small[..cut]);
        let _ = analyze(&small[cut..]);
    }
}

#[test]
fn oversized_adaptation_field() {
    let mut ts = Ts::new();
    ts.section(0, &pat(1, 0, &[(1, 0x1000)]));
    let mut p = vec![0x47, 0x41, 0x00, 0x30, 200];
    p.resize(188, 0xFF);
    ts.out.extend(p);
    let r = analyze(&ts.out);
    assert_eq!(r.stats.packets, 2);
}

#[test]
fn continuity_counter_rules() {
    let mut ts = Ts::new();
    ts.section(0, &pat(1, 0, &[(1, 0x1000)]));
    ts.section(0x1000, &pmt(1, 0, 0x100, &[], &[(H264, 0x100, vec![])]));
    let pl = [0u8; 184];
    let o = |cc: u8| Opts { cc: Some(cc), ..Opts::default() };
    ts.packet_with(0x100, o(0), &pl, false);
    ts.packet_with(0x100, o(1), &pl, false);
    ts.packet_with(0x100, o(1), &pl, false); // duplicate: allowed
    ts.packet_with(0x100, o(2), &pl, false);
    ts.packet_with(0x100, Opts { cc: Some(2), no_payload: true, ..Opts::default() }, &[], false); // AF only: no increment
    ts.packet_with(0x100, o(3), &pl, false);
    ts.packet_with(0x100, o(15), &pl, false); // jump: error
    ts.packet_with(0x100, o(0), &pl, false); // wraps 15 -> 0
    ts.packet_with(0x100, Opts { cc: Some(9), discontinuity: true, ..Opts::default() }, &[0; 182], false); // flagged
    ts.packet_with(0x100, o(10), &pl, false);
    ts.packet_with(0x100, o(12), &pl, false); // skip: error
    for cc in [0, 5, 0, 9] {
        ts.packet_with(0x1FFF, o(cc), &pl, true); // null PID: never checked
    }
    let r = analyze(&ts.out);
    assert_eq!(r.stream(0x100).unwrap().cc_errors, 2);
    let null = r.pids.iter().find(|p| p.pid == 0x1FFF).unwrap();
    assert_eq!(null.cc_errors, 0);
}

#[test]
fn many_feeds_are_independent() {
    let data = sample();
    let mut feeds: Vec<Demuxer> = (0..8).map(|_| Demuxer::new()).collect();
    for c in data.chunks(1316) {
        for d in feeds.iter_mut().take(7) {
            d.push(c);
        }
        // A noisy, unrelated feed must not affect the others.
        feeds[7].push(&[0x47, 0x00, 0x13]);
    }
    let whole = analyze(&data);
    for d in feeds.iter_mut().take(7) {
        d.finish();
        assert_eq!(d.report(), whole);
    }
}
