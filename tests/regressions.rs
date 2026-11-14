//! Regression tests for issues found in review.
mod common;

use common::*;
use mpegts::{analyze, Basis, Codec, Config, Demuxer, StreamKind};
use std::time::{Duration, Instant};

/// PMT long enough to need three packets.
fn three_packet_pmt() -> Vec<u8> {
    let streams: Vec<_> = (0..80).map(|i| (H264, 0x100 + i, vec![])).collect();
    let sec = pmt(1, 0, 0x100, &[], &streams);
    assert!(sec.len() > 183 + 184 && sec.len() < 183 + 184 * 2);
    sec
}

#[test]
fn duplicate_psi_packet_does_not_corrupt_section() {
    let sec = three_packet_pmt();
    let mut ts = Ts::new();
    ts.section(0, &pat(1, 0, &[(1, 0x1000)]));
    let start = ts.out.len();
    ts.section(0x1000, &sec);
    // Repeat the middle PMT packet (same CC), as the spec allows once.
    let middle = ts.out[start + 188..start + 376].to_vec();
    ts.out.splice(start + 376..start + 376, middle);
    let r = analyze(&ts.out);
    assert_eq!(r.stats.crc_errors, 0);
    assert_eq!(r.programs[0].streams.len(), 80);
    assert_eq!(r.pids.iter().find(|p| p.pid == 0x1000).unwrap().cc_errors, 0);
}

#[test]
fn lost_psi_packet_is_a_cc_error_not_a_crc_error() {
    let sec = three_packet_pmt();
    let mut ts = Ts::new();
    ts.section(0, &pat(1, 0, &[(1, 0x1000)]));
    let first = ts.out.len();
    ts.section(0x1000, &sec);
    ts.section(0x1000, &sec);
    // Lose the second packet and replace the third with a duplicate of the
    // first's continuation, so the byte count still completes the section.
    let second = first + 188;
    let third = ts.out[first + 376..first + 564].to_vec();
    ts.out.splice(second..second + 188, third.clone());
    let r = analyze(&ts.out);
    assert_eq!(r.stats.crc_errors, 0, "packet loss is not a CRC error");
    assert!(r.pids.iter().find(|p| p.pid == 0x1000).unwrap().cc_errors >= 1);
}

#[test]
fn scrambled_psi_packets_are_not_parsed() {
    let mut ts = Ts::new();
    ts.section(0, &pat(1, 0, &[(1, 0x1000)]));
    let sec = pmt(1, 0, 0x100, &[], &[(H264, 0x100, vec![])]);
    let mut payload = vec![0];
    // "Encrypted" body behind a plausible table header.
    payload.extend(sec.iter().enumerate().map(|(i, b)| if i < 8 { *b } else { b ^ 0x5A }));
    ts.packet_with(0x1000, Opts { pusi: true, scrambling: 2, ..Opts::default() }, &payload, true);
    let r = analyze(&ts.out);
    assert_eq!(r.stats.crc_errors, 0);
    assert_eq!(r.programs[0].version, None);
}

#[test]
fn repeated_duplicates_are_cc_errors() {
    let mut ts = Ts::new();
    ts.section(0, &pat(1, 0, &[(1, 0x1000)]));
    ts.section(0x1000, &pmt(1, 0, 0x100, &[], &[(H264, 0x100, vec![])]));
    let pl = [0u8; 184];
    for cc in [0, 1, 1, 1, 2] {
        ts.packet_with(0x100, Opts { cc: Some(cc), ..Opts::default() }, &pl, false);
    }
    let r = analyze(&ts.out);
    assert_eq!(r.stream(0x100).unwrap().cc_errors, 1, "a second duplicate is an error");
}

#[test]
fn multi_section_cat_does_not_churn_revision() {
    let s0 = section_full(0x01, 0xFFFF, 0, true, 0, 1, &[0x09, 4, 0x0B, 0x00, 0xE1, 0x00]);
    let s1 = section_full(0x01, 0xFFFF, 0, true, 1, 1, &[0x09, 4, 0x18, 0x00, 0xE1, 0x01]);
    let mut d = Demuxer::new();
    let mut ts = Ts::new();
    ts.section(1, &s0);
    ts.section(1, &s1);
    ts.null();
    ts.null();
    d.push(&ts.out);
    let rev = d.revision();
    assert_eq!(d.report().ca_systems.len(), 2, "both sections contribute");
    let mut more = Ts::new();
    for _ in 0..5 {
        more.section(1, &s0);
        more.section(1, &s1);
    }
    more.null();
    more.null();
    d.push(&more.out);
    assert_eq!(d.revision(), rev);
}

#[test]
fn es_pid_shared_by_two_programs() {
    let mut ts = Ts::new();
    ts.section(0, &pat(1, 0, &[(1, 0x1000), (2, 0x1001)]));
    ts.section(0x1000, &pmt(1, 0, 0x100, &[], &[(H264, 0x100, vec![]), (AAC, 0x300, vec![])]));
    ts.section(0x1001, &pmt(2, 0, 0x200, &[], &[(H264, 0x200, vec![]), (AAC, 0x300, vec![])]));
    ts.pes(0x300, 0xC0, Some(1000), &[0; 20]);
    ts.pes(0x300, 0xC0, Some(2000), &[0; 20]);
    let r = analyze(&ts.out);
    for n in [1, 2] {
        let a = r.program(n).unwrap().stream(0x300).unwrap();
        assert_eq!((a.first_pts, a.last_pts, a.pes_stream_id), (Some(1000), Some(2000), Some(0xC0)), "program {n}");
        assert_eq!(a.packets, 2);
    }
}

#[test]
fn pmt_update_without_pcr_keeps_pts_timeline() {
    let mut ts = Ts::new();
    ts.section(0, &pat(1, 0, &[(1, 0x1000)]));
    ts.section(0x1000, &pmt(1, 0, 0x1FFF, &[], &[(H264, 0x100, vec![])]));
    ts.pes(0x100, 0xE0, Some(0), &[0; 20]);
    ts.pes(0x100, 0xE0, Some(90_000), &[0; 20]);
    ts.section(0x1000, &pmt(1, 1, 0x1FFF, &[], &[(H264, 0x100, vec![]), (AAC, 0x101, vec![])]));
    ts.pes(0x100, 0xE0, Some(180_000), &[0; 20]);
    let r = analyze(&ts.out);
    assert_eq!(r.programs[0].pcr_pid, None);
    assert_eq!(r.stream(0x100).unwrap().pts_span, Some(180_000));
}

#[test]
fn pts_flag_without_room_in_header_is_ignored() {
    let mut ts = Ts::new();
    ts.section(0, &pat(1, 0, &[(1, 0x1000)]));
    ts.section(0x1000, &pmt(1, 0, 0x100, &[], &[(H264, 0x100, vec![])]));
    // PTS_DTS_flags = '10' but PES_header_data_length = 0.
    let mut pes = vec![0, 0, 1, 0xE0, 0, 0, 0x80, 0x80, 0];
    pes.extend([0x21, 0, 1, 0, 1, 9, 9, 9, 9, 9]);
    ts.packet_with(0x100, Opts { pusi: true, ..Opts::default() }, &pes, false);
    let r = analyze(&ts.out);
    assert_eq!(r.stream(0x100).unwrap().last_pts, None);
}

#[test]
fn private_stream_with_klv_metadata_descriptor() {
    let mut ts = Ts::new();
    ts.section(0, &pat(1, 0, &[(1, 0x1000)]));
    ts.section(0x1000, &pmt(1, 0, 0x100, &[], &[(PRIVATE, 0x101, metadata_descriptor(b"KLVA"))]));
    let r = analyze(&ts.out);
    let s = r.stream(0x101).unwrap();
    assert_eq!((s.codec, s.basis), (Codec::KlvAsync, Basis::Descriptor));
}

#[test]
fn stream_types_from_recent_amendments() {
    let types: &[(u8, Codec)] = &[
        (0x22, Codec::Mpeg2Video), // stereoscopic additional view
        (0x23, Codec::H264),       // AVC additional view
        (0x26, Codec::H264),       // MVCD
        (0x27, Codec::Temi),       // Annex U, not video
        (0x28, Codec::H265),
        (0x29, Codec::H265),
        (0x2A, Codec::H265),
        (0x2B, Codec::H265), // MV-HEVC
        (0x31, Codec::H265), // HEVC tiles
        (0x32, Codec::JpegXs),
        (0x35, Codec::Evc),
        (0x36, Codec::Lcevc),
    ];
    let streams: Vec<_> = types.iter().map(|&(t, _)| (t, 0x100 + u16::from(t), vec![])).collect();
    let mut ts = Ts::new();
    ts.section(0, &pat(1, 0, &[(1, 0x1000)]));
    ts.section(0x1000, &pmt(1, 0, 0x100, &[], &streams));
    let r = analyze(&ts.out);
    let got: Vec<_> = r.streams().map(|s| (s.stream_type, s.codec)).collect();
    assert_eq!(got, types);
    assert_eq!(r.count(StreamKind::Metadata), 1, "TEMI is metadata, not video");
}

#[test]
fn scrambled_pes_payload_is_not_sniffed() {
    let mut ts = Ts::new();
    ts.section(0, &pat(1, 0, &[(1, 0x1000)]));
    ts.section(0x1000, &pmt(1, 0, 0x100, &[], &[(PRIVATE, 0x101, vec![])]));
    // TS header clear, PES_scrambling_control = '10'.
    let mut pes = vec![0, 0, 1, 0xBD, 0, 0, 0xA0, 0x00, 0];
    pes.extend(KLV_KEY);
    ts.packet_with(0x101, Opts { pusi: true, ..Opts::default() }, &pes, false);
    let r = analyze(&ts.out);
    assert_eq!(r.stream(0x101).unwrap().codec, Codec::PrivateData);
}

#[test]
fn hostile_pat_is_handled_quickly() {
    // 256 sections x 253 programs: the largest PAT the syntax allows.
    let sections = |version: u8| -> Vec<Vec<u8>> {
        (0..=255u16)
            .map(|n| {
                let progs: Vec<_> = (0..253u16).map(|i| (1 + n * 253 + i, 0x20 + (n * 253 + i) % 0x1F00)).collect();
                section_full(0x00, 1, version, true, n as u8, 255, &pat_body(&progs))
            })
            .collect()
    };
    let mut ts = Ts::new();
    for version in [0, 1, 2, 3] {
        for sec in sections(version) {
            ts.section(0, &sec);
        }
    }
    // PCR packets on a PID that many programs could reference.
    for i in 0..20_000u64 {
        ts.pcr(0x100, i * 2_700_000);
    }
    let start = Instant::now();
    let r = analyze(&ts.out);
    let took = start.elapsed();
    assert_eq!(r.programs.len(), 256 * 253);
    assert_eq!(r.pat_version, Some(3));
    assert!(took < Duration::from_secs(20), "took {took:?}");
}

#[test]
fn oversized_sections_are_rejected() {
    // A PMT body longer than the 1021-byte limit, with a valid CRC.
    let streams: Vec<_> = (0..250).map(|i| (H264, 0x100 + i, vec![])).collect();
    let sec = pmt(1, 0, 0x100, &[], &streams);
    assert!(sec.len() > 1024);
    let mut ts = Ts::new();
    ts.section(0, &pat(1, 0, &[(1, 0x1000)]));
    ts.section(0x1000, &sec);
    let r = analyze(&ts.out);
    assert_eq!(r.programs[0].version, None);
    assert!(r.programs[0].streams.is_empty());
}

#[test]
fn stream_entries_are_capped() {
    // 30 programs x 200 streams = 6000 entries, all within section limits.
    let mut ts = Ts::new();
    let progs: Vec<_> = (1..=30u16).map(|n| (n, 0x1000 + n)).collect();
    ts.section(0, &pat(1, 0, &progs));
    for &(n, pmt_pid) in &progs {
        let streams: Vec<_> = (0..200).map(|i| (H264, 0x100 + i, vec![])).collect();
        ts.section(pmt_pid, &pmt(n, 0, 0x100, &[], &streams));
    }
    let r = analyze(&ts.out);
    assert_eq!(r.streams().count(), Config::default().max_streams);
    assert_eq!(r.stats.streams_dropped, 6000 - Config::default().max_streams as u64);
    // Every one of those programs shares the same PIDs; PES still flows.
    let mut d = Demuxer::new();
    d.push(&ts.out);
    let mut more = Ts::new();
    more.pes(0x100, 0xE0, Some(0), &[0; 20]);
    more.null();
    more.null();
    d.push(&more.out);
    let r = d.report();
    assert!(r.streams().filter(|s| s.pid == 0x100).all(|s| s.pes_stream_id == Some(0xE0)));
}

#[test]
fn program_level_klva_only_covers_bare_private_streams() {
    let mut ts = Ts::new();
    ts.section(0, &pat(1, 0, &[(1, 0x1000)]));
    ts.section(
        0x1000,
        &pmt(
            1,
            0,
            0x100,
            &registration(b"KLVA"),
            &[
                (PRIVATE, 0x101, vec![]),                 // RP 217: KLV
                (PRIVATE, 0x102, registration(b"VANC")),  // ST 2038
                (PRIVATE, 0x103, registration(b"XYZW")),  // unknown, but not KLV
                (0x32, 0x104, vec![]),                    // JPEG XS
                (0x2B, 0x105, vec![]),                    // MV-HEVC
                (0x80, 0x106, vec![]),                    // user private
            ],
        ),
    );
    let r = analyze(&ts.out);
    let got: Vec<_> = r.streams().map(|s| s.codec).collect();
    assert_eq!(
        got,
        [Codec::KlvAsync, Codec::Smpte2038, Codec::PrivateData, Codec::JpegXs, Codec::H265, Codec::Unknown]
    );
}

#[test]
fn sixteen_lost_packets_are_not_a_duplicate() {
    let mut ts = Ts::new();
    ts.section(0, &pat(1, 0, &[(1, 0x1000)]));
    ts.section(0x1000, &pmt(1, 0, 0x100, &[], &[(H264, 0x100, vec![])]));
    // Same CC twice, different payloads: 16 packets lost, not a repeat.
    ts.packet_with(0x100, Opts { cc: Some(3), ..Opts::default() }, &[1; 184], false);
    let pes = pes_bytes(0xE0, Some(4500), None, &[0; 20]);
    ts.packet_with(0x100, Opts { cc: Some(3), pusi: true, ..Opts::default() }, &pes, false);
    let r = analyze(&ts.out);
    let s = r.stream(0x100).unwrap();
    assert_eq!(s.cc_errors, 1);
    assert_eq!(s.last_pts, Some(4500), "the payload was not discarded");
}

#[test]
fn stuck_continuity_counter_does_not_hide_table_changes() {
    let mut ts = Ts::new();
    ts.section(0, &pat(1, 0, &[(1, 0x1000)]));
    let o = Opts { pusi: true, cc: Some(0), ..Opts::default() };
    for (version, st) in [(0, H264), (1, H265)] {
        let mut payload = vec![0];
        payload.extend(pmt(1, version, 0x100, &[], &[(st, 0x100, vec![])]));
        ts.packet_with(0x1000, o, &payload, true);
    }
    let r = analyze(&ts.out);
    assert_eq!(r.programs[0].version, Some(1));
    assert_eq!(r.stream(0x100).unwrap().codec, Codec::H265);
}

#[test]
fn splice_with_audio_ahead_of_the_clock() {
    let mut ts = Ts::new();
    ts.section(0, &pat(1, 0, &[(1, 0x1000)]));
    ts.section(0x1000, &pmt(1, 0, 0x100, &[], &[(H264, 0x100, vec![]), (AAC, 0x101, vec![])]));
    let sec = |s: u64| s * 90_000;
    for t in 10..=20 {
        ts.pcr(0x100, sec(t) * 300);
        ts.pes(0x101, 0xC0, Some(sec(t)), &[0; 20]);
    }
    // The new segment's audio arrives before its first (flagged) PCR.
    ts.pes(0x101, 0xC0, Some(sec(200)), &[0; 20]);
    let flagged = Opts { discontinuity: true, pcr: Some(sec(200) * 300), no_payload: true, ..Opts::default() };
    ts.packet_with(0x100, flagged, &[], false);
    for t in 201..=210 {
        ts.pcr(0x100, sec(t) * 300);
        ts.pes(0x101, 0xC0, Some(sec(t)), &[0; 20]);
    }
    let r = analyze(&ts.out);
    assert_eq!(r.programs[0].duration(), Some(Duration::from_secs(20)));
    let audio = r.stream(0x101).unwrap().duration().unwrap();
    assert!(audio >= Duration::from_secs(19) && audio <= Duration::from_secs(20), "{audio:?}");
}

#[test]
fn sparse_stream_keeps_long_gaps_on_a_running_clock() {
    let mut ts = Ts::new();
    ts.section(0, &pat(1, 0, &[(1, 0x1000)]));
    ts.section(0x1000, &pmt(1, 0, 0x100, &[], &[(H264, 0x100, vec![]), (PRIVATE, 0x101, registration(b"KLVA"))]));
    // 18 minutes of PCR once a second, KLV every 6 minutes.
    for t in 0..=18 * 60u64 {
        ts.pcr(0x100, t * 27_000_000);
        if t % 360 == 0 {
            ts.pes(0x101, 0xBD, Some(t * 90_000), &KLV_KEY);
        }
    }
    let r = analyze(&ts.out);
    let klv = r.stream(0x101).unwrap();
    assert_eq!(klv.duration(), Some(Duration::from_secs(18 * 60)));
}

#[test]
fn pes_header_split_across_packets() {
    let mut ts = Ts::new();
    ts.section(0, &pat(1, 0, &[(1, 0x1000)]));
    ts.section(0x1000, &pmt(1, 0, 0x100, &[], &[(H264, 0x100, vec![]), (PRIVATE, 0x101, vec![])]));
    for (pid, sid, es) in [(0x100, 0xE0, vec![0u8; 40]), (0x101, 0xBD, KLV_KEY.to_vec())] {
        let pes = pes_bytes(sid, Some(12_345), Some(11_000), &es);
        // First packet carries only 8 bytes (the rest is stuffing).
        ts.packet_with(pid, Opts { pusi: true, ..Opts::default() }, &pes[..8], false);
        ts.packet_with(pid, Opts::default(), &pes[8..], false);
    }
    let r = analyze(&ts.out);
    let v = r.stream(0x100).unwrap();
    assert_eq!((v.first_pts, v.last_dts, v.pes_stream_id), (Some(12_345), Some(11_000), Some(0xE0)));
    let k = r.stream(0x101).unwrap();
    assert_eq!((k.codec, k.basis), (Codec::KlvAsync, Basis::Payload));
}

#[test]
fn split_pes_header_lost_mid_way_is_dropped() {
    let mut ts = Ts::new();
    ts.section(0, &pat(1, 0, &[(1, 0x1000)]));
    ts.section(0x1000, &pmt(1, 0, 0x100, &[], &[(H264, 0x100, vec![])]));
    let pes = pes_bytes(0xE0, Some(777), None, &[0; 40]);
    ts.packet_with(0x100, Opts { pusi: true, ..Opts::default() }, &pes[..8], false);
    // The continuation is lost: the next packet skips a CC.
    ts.packet_with(0x100, Opts { cc: Some(5), ..Opts::default() }, &[0x99; 184], false);
    let r = analyze(&ts.out);
    assert_eq!(r.stream(0x100).unwrap().last_pts, None, "no PTS read from the wrong bytes");
}

#[test]
fn transport_error_is_not_also_a_cc_error() {
    let sec = three_packet_pmt();
    let mut ts = Ts::new();
    ts.section(0, &pat(1, 0, &[(1, 0x1000)]));
    let start = ts.out.len();
    ts.section(0x1000, &sec);
    ts.section(0x1000, &sec);
    ts.out[start + 188 + 1] |= 0x80; // TEI on the middle packet of the first copy
    let r = analyze(&ts.out);
    let pmt = r.pids.iter().find(|p| p.pid == 0x1000).unwrap();
    assert_eq!((pmt.transport_errors, pmt.cc_errors), (1, 0));
    assert_eq!(r.stats.crc_errors, 0, "the damaged section was dropped, not parsed");
    assert_eq!(r.programs[0].streams.len(), 80, "the second copy got through");
}

#[test]
fn trailing_junk_is_not_a_packet() {
    let mut ts = Ts::new();
    ts.section(0, &pat(1, 0, &[(1, 0x1000)]));
    ts.null();
    ts.null();
    let good = ts.out.len() / 188;
    let mut data = ts.out.clone();
    data.extend([0x00, 0x11, 0x22]);
    let mut junk = vec![0x47];
    junk.extend([0x5A; 187]);
    data.extend(junk);
    let r = analyze(&data);
    assert_eq!(r.stats.packets, good as u64);
    // A single packet on its own is still accepted.
    let mut one = Ts::new();
    one.section(0, &pat(1, 0, &[(1, 0x1000)]));
    assert_eq!(analyze(&one.out).stats.packets, 1);
}

#[test]
fn api_exposes_names_program_and_current_scrambling() {
    use mpegts::{CaSystem, Descriptor, PCR_HZ, PTS_HZ};
    fn takes(_: &Descriptor, _: &CaSystem) {}
    let _ = takes;
    assert_eq!((PTS_HZ, PCR_HZ), (90_000, 27_000_000));

    let mut ts = Ts::new();
    ts.section(0, &pat(1, 0, &[(7, 0x1000)]));
    ts.section(0x1000, &pmt(7, 0, 0x1FFF, &[], &[(H264, 0x100, vec![])]));
    ts.pes_with(0x100, 0xE0, Some(0), None, Opts { scrambling: 2, ..Opts::default() }, &[0; 20]);
    let mut d = Demuxer::new();
    d.push(&ts.out);
    d.finish();
    assert!(d.report().stream(0x100).unwrap().is_scrambled());

    let mut clear = Ts::new();
    clear.section(0, &pat(1, 0, &[(7, 0x1000)]));
    // Continue the CC sequence of the first part.
    for i in 0..3u64 {
        clear.pes_with(0x100, 0xE0, Some(i * 90_000), None, Opts { cc: Some(1 + i as u8), ..Opts::default() }, &[0; 20]);
    }
    d.push(&clear.out);
    d.finish();
    let r = d.report();
    let s = r.stream(0x100).unwrap();
    assert!(!s.is_scrambled(), "scrambling ended");
    assert_eq!(s.scrambled_packets, 1);
    assert_eq!(s.program, 7);
    // No PCR in this program: duration falls back to PTS.
    assert_eq!(r.programs[0].pcr_pid, None);
    assert_eq!(r.duration(), Some(Duration::from_secs(2)));
}

#[test]
fn descriptor_coverage() {
    let ext = |tag: u8| vec![0x7F, 2, tag, 0];
    let mut ts = Ts::new();
    ts.section(0, &pat(1, 0, &[(1, 0x1000)]));
    ts.section(
        0x1000,
        &pmt(
            1,
            0,
            0x100,
            &[],
            &[
                (PRIVATE, 0x101, ext(0x20)),           // DVB TTML
                (PRIVATE, 0x102, ext(0x0E)),           // DTS-HD
                (PRIVATE, 0x103, ext(0x21)),           // DTS-UHD
                (PRIVATE, 0x104, vec![0x46, 0]),       // VBI teletext
                (PRIVATE, 0x105, ext(0x15)),           // AC-4
            ],
        ),
    );
    let r = analyze(&ts.out);
    let got: Vec<_> = r.streams().map(|s| s.codec).collect();
    assert_eq!(got, [Codec::TtmlSubtitle, Codec::Dts, Codec::Dts, Codec::Teletext, Codec::Ac4]);
}
