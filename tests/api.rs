//! The programmatic query API on `Report`, `Program` and `Stream`.
mod common;

use common::*;
use mpegts::{analyze, Basis, Codec, Demuxer, StreamKind};
use std::time::Duration;

fn two_programs() -> Vec<u8> {
    let mut ts = Ts::new();
    ts.section(0, &pat(1, 0, &[(1, 0x1000), (2, 0x1001)]));
    ts.section(
        0x1000,
        &pmt(
            1,
            0,
            0x100,
            &[],
            &[
                (H264, 0x100, vec![]),
                (AAC, 0x101, language(b"eng")),
                (PRIVATE, 0x102, registration(b"KLVA")),
                (METADATA, 0x103, metadata_descriptor(b"KLVA")),
            ],
        ),
    );
    ts.section(0x1001, &pmt(2, 0, 0x200, &[], &[(H265, 0x200, vec![])]));
    ts.out
}

#[test]
fn query_helpers() {
    let r = analyze(&two_programs());
    assert_eq!(r.programs.len(), 2);
    assert_eq!(r.streams().count(), 5);

    let video: Vec<_> = r.video().map(|s| (s.pid, s.codec)).collect();
    assert_eq!(video, [(0x100, Codec::H264), (0x200, Codec::H265)]);
    assert_eq!(r.audio().count(), 1);
    assert_eq!(r.metadata().count(), 2);
    assert_eq!(r.count(StreamKind::Subtitle), 0);

    let klv: Vec<_> = r.klv().map(|s| (s.pid, s.codec)).collect();
    assert_eq!(klv, [(0x102, Codec::KlvAsync), (0x103, Codec::KlvSync)]);
    assert!(r.has(Codec::H265));
    assert!(!r.has(Codec::Ac3));

    let aac = r.stream(0x101).expect("stream by pid");
    assert_eq!(aac.language.unwrap().as_str(), "eng");
    assert_eq!(aac.kind(), StreamKind::Audio);
    assert!(r.stream(0x999).is_none());

    let p2 = r.program(2).expect("program by number");
    assert_eq!(p2.pmt_pid, 0x1001);
    assert_eq!(p2.stream(0x200).unwrap().codec, Codec::H265);
    assert_eq!(p2.streams_of(StreamKind::Video).count(), 1);
    assert!(r.program(3).is_none());

    let sync = r.stream(0x103).unwrap();
    assert!(sync.is_klv());
    assert_eq!(sync.metadata_format.unwrap(), b"KLVA");
    assert_eq!(sync.basis, Basis::Descriptor);
}

#[test]
fn codec_and_kind_metadata() {
    assert_eq!(Codec::H264.kind(), StreamKind::Video);
    assert_eq!(Codec::KlvSync.kind(), StreamKind::Metadata);
    assert_eq!(Codec::DvbSubtitle.kind(), StreamKind::Subtitle);
    assert_eq!(Codec::PrivateData.kind(), StreamKind::Data);
    assert_eq!(Codec::Unknown.kind(), StreamKind::Unknown);
    assert_eq!(Codec::H265.to_string(), "H.265/HEVC");
    assert_eq!(StreamKind::Metadata.to_string(), "metadata");
    assert!(Codec::KlvAsync.is_klv() && !Codec::Id3.is_klv());
}

#[test]
fn descriptors_preserved_verbatim() {
    let unknown = vec![0xF0, 3, 0xAA, 0xBB, 0xCC];
    let mut es_desc = registration(b"KLVA");
    es_desc.extend(&unknown);
    let mut ts = Ts::new();
    ts.section(0, &pat(1, 0, &[(1, 0x1000)]));
    ts.section(0x1000, &pmt(1, 0, 0x100, &unknown, &[(PRIVATE, 0x100, es_desc)]));
    let r = analyze(&ts.out);
    let p = &r.programs[0];
    assert_eq!(p.descriptors.len(), 1);
    assert_eq!((p.descriptors[0].tag, &p.descriptors[0].data[..]), (0xF0, &[0xAA, 0xBB, 0xCC][..]));
    let s = &p.streams[0];
    assert_eq!(s.descriptors.len(), 2);
    assert_eq!(s.descriptor(0x05).unwrap().data, b"KLVA");
    assert_eq!(s.descriptor(0xF0).unwrap().data, [0xAA, 0xBB, 0xCC]);
    assert!(s.descriptor(0x0A).is_none());
}

#[test]
fn timestamps_and_durations() {
    let mut ts = Ts::new();
    ts.section(0, &pat(1, 0, &[(1, 0x1000)]));
    ts.section(0x1000, &pmt(1, 0, 0x100, &[], &[(H264, 0x100, vec![])]));
    ts.pcr(0x100, 1_000);
    ts.pes_with(0x100, 0xE0, Some(10_000), Some(7_000), Opts::default(), &[0; 20]);
    ts.pes_with(0x100, 0xE0, Some(19_000), Some(13_000), Opts::default(), &[0; 20]);
    ts.pcr(0x100, 1_000 + 27_000_000 * 2);
    let r = analyze(&ts.out);
    let p = &r.programs[0];
    assert_eq!((p.first_pcr, p.last_pcr), (Some(1_000), Some(1_000 + 54_000_000)));
    assert_eq!(p.duration(), Some(Duration::from_secs(2)));
    assert_eq!(r.duration(), Some(Duration::from_secs(2)));
    let s = &p.streams[0];
    assert_eq!((s.first_pts, s.last_pts, s.last_dts), (Some(10_000), Some(19_000), Some(13_000)));
    assert_eq!(s.pts_span, Some(9_000));
    assert_eq!(s.duration(), Some(Duration::from_millis(100)));
}

#[test]
fn pts_wraparound_and_reordering() {
    let top = 1u64 << 33;
    let mut ts = Ts::new();
    ts.section(0, &pat(1, 0, &[(1, 0x1000)]));
    ts.section(0x1000, &pmt(1, 0, 0x100, &[], &[(H264, 0x100, vec![])]));
    // B-frame style reordering near the wrap point, then past it.
    for pts in [top - 9_000, top - 3_000, top - 6_000, 3_000, 0] {
        ts.pes(0x100, 0xE0, Some(pts), &[0; 20]);
    }
    let r = analyze(&ts.out);
    let s = &r.programs[0].streams[0];
    assert_eq!(s.first_pts, Some(top - 9_000));
    assert_eq!(s.last_pts, Some(0));
    assert_eq!(s.pts_span, Some(12_000));
}

#[test]
fn pcr_wraparound() {
    let top = (1u64 << 33) * 300;
    let mut ts = Ts::new();
    ts.section(0, &pat(1, 0, &[(1, 0x1000)]));
    ts.section(0x1000, &pmt(1, 0, 0x100, &[], &[(H264, 0x100, vec![])]));
    ts.pcr(0x100, top - 27_000_000);
    ts.pcr(0x100, 27_000_000);
    let r = analyze(&ts.out);
    assert_eq!(r.programs[0].pcr_span, Some(54_000_000));
    assert_eq!(r.programs[0].duration(), Some(Duration::from_secs(2)));
}

#[test]
fn active_streams_reflect_recent_traffic() {
    let mut ts = Ts::new();
    ts.section(0, &pat(1, 0, &[(1, 0x1000)]));
    ts.section(0x1000, &pmt(1, 0, 0x100, &[], &[(H264, 0x100, vec![]), (AAC, 0x101, vec![])]));
    ts.pes(0x101, 0xC0, Some(0), &[0; 20]);
    for _ in 0..1000 {
        ts.pes(0x100, 0xE0, None, &[0; 20]);
    }
    let r = analyze(&ts.out);
    let active: Vec<_> = r.active_streams(500).map(|s| s.pid).collect();
    assert_eq!(active, [0x100]);
    assert_eq!(r.active_streams(10_000).count(), 2);
    assert_eq!(r.stream(0x100).unwrap().last_packet, Some(r.stats.packets));
}

#[test]
fn scrambled_streams() {
    let mut ts = Ts::new();
    ts.section(0, &pat(1, 0, &[(1, 0x1000)]));
    ts.section(0x1000, &pmt(1, 0, 0x100, &[], &[(PRIVATE, 0x100, vec![])]));
    let o = Opts { scrambling: 2, ..Opts::default() };
    ts.pes_with(0x100, 0xBD, Some(5), None, o, &KLV_KEY);
    let r = analyze(&ts.out);
    let s = r.stream(0x100).unwrap();
    assert!(s.is_scrambled());
    assert_eq!(s.scrambled_packets, 1);
    // Scrambled payloads are neither sniffed nor timestamped.
    assert_eq!(s.codec, Codec::PrivateData);
    assert_eq!(s.last_pts, None);
}

#[test]
fn conditional_access_table() {
    let mut ca = vec![0x09, 4, 0x0B, 0x00, 0xE1, 0x23]; // system 0x0B00, PID 0x123
    ca.extend([0x09, 5, 0x18, 0x10, 0xE0, 0x50, 0x00]); // with private byte
    let mut ts = Ts::new();
    ts.section(1, &section(0x01, 0xFFFF, 0, &ca));
    ts.section(0, &pat(1, 0, &[(1, 0x1000)]));
    let r = analyze(&ts.out);
    let got: Vec<_> = r.ca_systems.iter().map(|c| (c.system_id, c.pid)).collect();
    assert_eq!(got, [(0x0B00, 0x123), (0x1810, 0x050)]);
}

#[test]
fn pid_table_covers_unreferenced_pids() {
    let mut ts = Ts::new();
    ts.section(0, &pat(1, 0, &[(1, 0x1000)]));
    ts.null();
    ts.null();
    ts.pes(0x777, 0xE0, None, &[0; 10]);
    let r = analyze(&ts.out);
    let pids: Vec<_> = r.pids.iter().map(|p| (p.pid, p.packets)).collect();
    assert_eq!(pids, [(0, 1), (0x777, 1), (0x1FFF, 2)]);
    assert_eq!(r.streams().count(), 0);
    assert_eq!(r.programs[0].version, None, "PMT never arrived");
}

#[test]
fn revision_tracks_structural_changes_only() {
    let mut d = Demuxer::new();
    assert_eq!(d.revision(), 0);
    let mut ts = Ts::new();
    ts.section(0, &pat(1, 0, &[(1, 0x1000)]));
    ts.section(0x1000, &pmt(1, 0, 0x100, &[], &[(PRIVATE, 0x100, vec![])]));
    ts.null();
    ts.null();
    d.push(&ts.out);
    let after_tables = d.revision();
    assert!(after_tables >= 2);

    let mut more = Ts::new();
    for _ in 0..10 {
        more.section(0, &pat(1, 0, &[(1, 0x1000)]));
        more.section(0x1000, &pmt(1, 0, 0x100, &[], &[(PRIVATE, 0x100, vec![])]));
        more.pes(0x100, 0xBD, None, &[0; 20]); // not KLV: stays private
    }
    d.push(&more.out);
    assert_eq!(d.revision(), after_tables);

    let mut klv = Ts::new();
    klv.pes(0x100, 0xBD, None, &KLV_KEY);
    d.push(&klv.out);
    assert_eq!(d.revision(), after_tables, "only the first PES is sniffed");
}

#[test]
fn report_display_is_readable() {
    let text = analyze(&two_programs()).to_string();
    assert!(text.contains("Program 1"));
    assert!(text.contains("H.264/AVC"));
    assert!(text.contains("KLV (sync)"));
    assert!(text.contains("lang eng"));
    assert!(!text.contains('\u{2014}'), "no em-dashes");
}

#[cfg(feature = "serde")]
#[test]
fn serializes_to_json() {
    let r = analyze(&two_programs());
    let v = serde_json::to_value(r).unwrap();
    let s = &v["programs"][0]["streams"];
    assert_eq!(s[0]["codec"], "H264");
    assert_eq!(s[1]["language"], "eng");
    assert_eq!(s[2]["registration"], "KLVA");
    assert_eq!(s[3]["codec"], "KlvSync");
    assert_eq!(v["stats"]["packet_size"], 188);
}

fn one_video_program(ts: &mut Ts) {
    ts.section(0, &pat(1, 0, &[(1, 0x1000)]));
    ts.section(0x1000, &pmt(1, 0, 0x100, &[], &[(H264, 0x100, vec![])]));
}

#[test]
fn splice_with_discontinuity_indicator() {
    let mut ts = Ts::new();
    one_video_program(&mut ts);
    // Segment 1: PCR 10s..12s, PTS 1s..2s.
    ts.pcr(0x100, 270_000_000);
    ts.pes(0x100, 0xE0, Some(90_000), &[0; 10]);
    ts.pes(0x100, 0xE0, Some(180_000), &[0; 10]);
    ts.pcr(0x100, 324_000_000);
    // Segment 2 restarts the clock lower, flagged by the encoder.
    let flagged = Opts { discontinuity: true, pcr: Some(27_000_000), no_payload: true, ..Opts::default() };
    ts.packet_with(0x100, flagged, &[], false);
    ts.pes(0x100, 0xE0, Some(9_000), &[0; 10]);
    ts.pes(0x100, 0xE0, Some(99_000), &[0; 10]);
    ts.pcr(0x100, 54_000_000);
    let r = analyze(&ts.out);
    let p = &r.programs[0];
    assert_eq!(p.pcr_span, Some(54_000_000 + 27_000_000));
    assert_eq!(p.duration(), Some(Duration::from_secs(3)));
    assert_eq!(p.streams[0].pts_span, Some(90_000 + 90_000));
    assert_eq!(p.streams[0].cc_errors, 0, "discontinuity flag excuses the CC");
}

#[test]
fn splice_without_indicator_is_detected() {
    let mut ts = Ts::new();
    one_video_program(&mut ts);
    ts.pcr(0x100, 270_000_000);
    ts.pcr(0x100, 297_000_000);
    ts.pcr(0x100, 1_000); // backwards: new timeline
    ts.pcr(0x100, 27_001_000);
    ts.pcr(0x100, 27_001_000 + 15 * 27_000_000); // 15 s forward gap: new timeline
    ts.pcr(0x100, 27_001_000 + 16 * 27_000_000);
    let r = analyze(&ts.out);
    assert_eq!(r.programs[0].duration(), Some(Duration::from_secs(3)));
}

#[test]
fn replaced_stream_starts_fresh_counters() {
    let mut ts = Ts::new();
    one_video_program(&mut ts);
    for i in 0..10 {
        ts.pes(0x100, 0xE0, Some(i * 3000), &[0; 10]);
    }
    // Same PID now carries H.265.
    ts.section(0x1000, &pmt(1, 1, 0x100, &[], &[(H265, 0x100, vec![])]));
    for i in 0..4 {
        ts.pes(0x100, 0xE0, Some(500_000 + i * 3000), &[0; 10]);
    }
    let r = analyze(&ts.out);
    let s = r.stream(0x100).unwrap();
    assert_eq!(s.codec, Codec::H265);
    assert_eq!(s.packets, 4);
    assert_eq!(s.first_pts, Some(500_000), "timing restarts with the new stream");
    assert_eq!(s.last_pts, Some(509_000));
    assert_eq!(s.pts_span, Some(9_000));
    let pid = r.pids.iter().find(|p| p.pid == 0x100).unwrap();
    assert_eq!(pid.packets, 14, "PID counters stay feed-wide");
}

#[test]
fn bitrates() {
    let mut ts = Ts::new();
    one_video_program(&mut ts);
    ts.section(0x1000, &pmt(1, 0, 0x100, &[], &[(H264, 0x100, vec![]), (0x05, 0x101, vec![])]));
    ts.pcr(0x100, 0);
    // 101 video PES over exactly 1 s of PTS, plus the 2 PCR-only packets.
    for i in 0..=100u64 {
        ts.pes(0x100, 0xE0, Some(i * 900), &[0; 10]);
    }
    ts.pcr(0x100, 27_000_000 * 2);
    for _ in 0..50 {
        ts.psi_raw(0x101, true, &[0]); // no PTS: bitrate from PCR span
    }
    let r = analyze(&ts.out);
    let v = r.stream(0x100).unwrap();
    assert_eq!(v.pts_span, Some(90_000));
    assert_eq!(v.packets, 103);
    assert_eq!(v.bitrate, Some(103 * 188 * 8));
    let d = r.stream(0x101).unwrap();
    assert_eq!(d.bitrate, Some(50 * 188 * 8 / 2));
    assert_eq!(r.bitrate(), Some(r.stats.packets * 188 * 8 / 2));
}

#[test]
fn bitrate_absent_without_timing() {
    let mut ts = Ts::new();
    one_video_program(&mut ts);
    ts.pes(0x100, 0xE0, None, &[0; 10]);
    let r = analyze(&ts.out);
    assert_eq!(r.stream(0x100).unwrap().bitrate, None);
    assert_eq!(r.bitrate(), None);
    assert_eq!(r.duration(), None);
}
