mod common;

use common::*;
use mpegts::{analyze, Basis, Codec, Demuxer, StreamKind};

fn h264_aac_stream() -> Vec<u8> {
    let mut ts = Ts::new();
    for i in 0..5u64 {
        ts.section(0, &pat(7, 0, &[(1, 0x1000)]));
        ts.section(
            0x1000,
            &pmt(1, 0, 0x100, &[], &[(H264, 0x100, vec![]), (AAC, 0x101, language(b"eng"))]),
        );
        ts.pes(0x100, 0xE0, Some(90_000 * i), &[0, 0, 0, 1, 0x67, 1, 2, 3, 4, 5]);
        ts.pes(0x101, 0xC0, Some(90_000 * i), &[0xFF, 0xF1, 0, 0, 0, 0, 0, 0, 0]);
        ts.null();
    }
    ts.out
}

#[test]
fn identifies_h264_and_aac() {
    let r = analyze(&h264_aac_stream());
    assert_eq!(r.transport_stream_id, Some(7));
    assert_eq!(r.programs.len(), 1);
    let p = &r.programs[0];
    assert_eq!((p.number, p.pmt_pid, p.version, p.pcr_pid), (1, 0x1000, Some(0), Some(0x100)));
    assert_eq!(p.streams.len(), 2);
    assert_eq!(p.streams[0].codec, Codec::H264);
    assert_eq!(p.streams[0].kind(), StreamKind::Video);
    assert_eq!(p.streams[0].last_pts, Some(90_000 * 4));
    assert_eq!(p.streams[0].pes_stream_id, Some(0xE0));
    assert_eq!(p.streams[1].codec, Codec::AacAdts);
    assert_eq!(p.streams[1].language.unwrap().to_string(), "eng");
    assert_eq!(r.count(StreamKind::Video), 1);
    assert_eq!(r.count(StreamKind::Audio), 1);
    assert_eq!(r.stats.packet_size, Some(188));
    assert_eq!(r.stats.bytes_skipped, 0);
    assert_eq!(r.stats.crc_errors, 0);
    assert!(r.pids.iter().all(|p| p.cc_errors == 0));
}

#[test]
fn live_report_follows_pmt_changes() {
    let mut d = Demuxer::new();

    // T0..T1: one H.264 video stream.
    let mut ts = Ts::new();
    ts.section(0, &pat(1, 0, &[(1, 0x1000)]));
    ts.section(0x1000, &pmt(1, 0, 0x100, &[], &[(H264, 0x100, vec![])]));
    ts.pes(0x100, 0xE0, Some(0), &[0; 32]);
    d.push(&ts.out);
    let r = d.report();
    let codecs: Vec<_> = r.streams().map(|s| s.codec).collect();
    assert_eq!(codecs, [Codec::H264]);
    let rev = d.revision();

    // Repeats of the same tables change nothing.
    let mut again = Ts::new();
    again.section(0, &pat(1, 0, &[(1, 0x1000)]));
    again.section(0x1000, &pmt(1, 0, 0x100, &[], &[(H264, 0x100, vec![])]));
    d.push(&again.out);
    assert_eq!(d.revision(), rev);

    // T1..T2: H.265 video and asynchronous KLV.
    let mut ts = Ts::new();
    ts.section(
        0x1000,
        &pmt(1, 1, 0x200, &[], &[(H265, 0x200, vec![]), (PRIVATE, 0x201, registration(b"KLVA"))]),
    );
    ts.pes(0x200, 0xE0, Some(0), &[0; 32]);
    ts.null(); // lets the final packet's sync be confirmed
    ts.null();
    d.push(&ts.out);
    assert!(d.revision() > rev);
    let r = d.report();
    let codecs: Vec<_> = r.streams().map(|s| (s.pid, s.codec)).collect();
    assert_eq!(codecs, [(0x200, Codec::H265), (0x201, Codec::KlvAsync)]);
    assert_eq!(r.programs[0].version, Some(1));
}

#[test]
fn pat_change_removes_program() {
    let mut ts = Ts::new();
    ts.section(0, &pat(1, 0, &[(1, 0x1000), (2, 0x1001)]));
    ts.section(0x1000, &pmt(1, 0, 0x100, &[], &[(H264, 0x100, vec![])]));
    ts.section(0x1001, &pmt(2, 0, 0x200, &[], &[(H265, 0x200, vec![])]));
    let mut d = Demuxer::new();
    d.push(&ts.out);
    d.push(&[]);
    let first = ts.out.len();
    // Same builder, so continuity counters carry on as a real mux's would.
    ts.section(0, &pat(1, 1, &[(2, 0x1001)]));
    ts.null();
    ts.null();
    d.push(&ts.out[first..]);
    let r = d.report();
    assert_eq!(r.programs.len(), 1);
    assert_eq!(r.programs[0].number, 2);
    assert_eq!(r.programs[0].streams[0].codec, Codec::H265, "surviving program keeps its PMT");
}

#[test]
fn chunking_does_not_change_result() {
    let data = h264_aac_stream();
    let whole = analyze(&data);
    for chunk in [1, 7, 187, 188, 189, 1316] {
        let mut d = Demuxer::new();
        for c in data.chunks(chunk) {
            d.push(c);
        }
        d.finish();
        assert_eq!(d.report(), whole, "chunk size {chunk}");
    }
}

#[test]
fn m2ts_and_204_byte_packets() {
    let data = h264_aac_stream();
    for (stride, prefix) in [(192usize, true), (204, false)] {
        let mut framed = Vec::new();
        for p in data.chunks(188) {
            if prefix {
                framed.extend_from_slice(&[0x12, 0x34, 0x56, 0x78]);
            }
            framed.extend_from_slice(p);
            if !prefix {
                framed.extend_from_slice(&[0xAA; 16]);
            }
        }
        let r = analyze(&framed);
        assert_eq!(r.stats.packet_size, Some(stride));
        let codecs: Vec<_> = r.streams().map(|s| s.codec).collect();
        assert_eq!(codecs, [Codec::H264, Codec::AacAdts]);
        assert_eq!(r.stats.packets, (data.len() / 188) as u64);
        let mut d = Demuxer::new();
        for c in framed.chunks(100) {
            d.push(c);
        }
        d.finish();
        assert_eq!(d.report(), r, "stride {stride} chunked");
    }
}

#[test]
fn resyncs_after_garbage() {
    let data = h264_aac_stream();
    let mut bad = vec![0x47, 0x00, 0x47, 0x13];
    bad.extend_from_slice(&data[..188 * 10]);
    bad.extend_from_slice(&[0xDE, 0xAD, 0x47, 0xBE, 0xEF]);
    bad.extend_from_slice(&data[188 * 10..]);
    let r = analyze(&bad);
    assert_eq!(r.stats.packets, (data.len() / 188) as u64);
    assert_eq!(r.stats.sync_losses, 1);
    assert_eq!(r.stats.bytes_skipped, 9);
    assert_eq!(r.streams().count(), 2);
}

#[test]
fn pmt_spanning_several_packets() {
    // Enough descriptors to push the PMT past one packet.
    let streams: Vec<_> = (0..20).map(|i| (AAC, 0x300 + i, language(b"fra").repeat(3))).collect();
    let sec = pmt(1, 0, 0x300, &[], &streams);
    assert!(sec.len() > 184 * 2);
    let mut ts = Ts::new();
    ts.section(0, &pat(1, 0, &[(1, 0x1000)]));
    ts.section(0x1000, &sec);
    let r = analyze(&ts.out);
    assert_eq!(r.programs[0].streams.len(), 20);
    assert!(r.streams().all(|s| s.codec == Codec::AacAdts));
}

#[test]
fn corrupt_pmt_is_rejected() {
    let mut sec = pmt(1, 0, 0x100, &[], &[(H264, 0x100, vec![])]);
    sec[14] ^= 0xFF;
    let mut ts = Ts::new();
    ts.section(0, &pat(1, 0, &[(1, 0x1000)]));
    ts.section(0x1000, &sec);
    let r = analyze(&ts.out);
    assert_eq!(r.stats.crc_errors, 1);
    assert_eq!(r.programs[0].version, None);
    assert!(r.programs[0].streams.is_empty());
}

#[test]
fn klv_variants() {
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
                (METADATA, 0x101, metadata_descriptor(b"KLVA")), // MISB ST 1402 sync
                (PRIVATE, 0x102, registration(b"KLVA")),         // RP 217 async
                (PRIVATE, 0x103, vec![]),                        // undeclared, sniffed
                (METADATA, 0x104, metadata_descriptor(b"ID3 ")),
            ],
        ),
    );
    // Program-level KLVA registration (RP 217) on a bare 0x06 stream.
    ts.section(0x1001, &pmt(2, 0, 0x200, &registration(b"KLVA"), &[(PRIVATE, 0x200, vec![])]));
    let mut klv = KLV_KEY.to_vec();
    klv.extend_from_slice(&[0x02, 0xAB, 0xCD]);
    ts.pes(0x103, 0xBD, None, &klv);

    let r = analyze(&ts.out);
    let got: Vec<_> = r.streams().map(|s| (s.pid, s.codec, s.basis)).collect();
    assert_eq!(
        got,
        [
            (0x100, Codec::H264, Basis::StreamType),
            (0x101, Codec::KlvSync, Basis::Descriptor),
            (0x102, Codec::KlvAsync, Basis::Descriptor),
            (0x103, Codec::KlvAsync, Basis::Payload),
            (0x104, Codec::Id3, Basis::Descriptor),
            (0x200, Codec::KlvAsync, Basis::Descriptor),
        ]
    );
    assert_eq!(r.count(StreamKind::Metadata), 5);
}

#[test]
fn sync_klv_sniffed_through_access_unit_header() {
    let mut ts = Ts::new();
    ts.section(0, &pat(1, 0, &[(1, 0x1000)]));
    ts.section(0x1000, &pmt(1, 0, 0x100, &[], &[(METADATA, 0x101, vec![])]));
    let mut au = vec![0x00, 0x00, 0x0F, 0x00, 19]; // cell header
    au.extend_from_slice(&KLV_KEY);
    au.extend_from_slice(&[0x02, 0xAB, 0xCD]);
    ts.pes(0x101, 0xFC, Some(1234), &au);
    let r = analyze(&ts.out);
    let s = r.streams().next().unwrap();
    assert_eq!((s.codec, s.basis), (Codec::KlvSync, Basis::Payload));
    assert_eq!(s.last_pts, Some(1234));
}

#[test]
fn sniffed_identity_survives_pmt_update() {
    let mut ts = Ts::new();
    ts.section(0, &pat(1, 0, &[(1, 0x1000)]));
    ts.section(0x1000, &pmt(1, 0, 0x100, &[], &[(PRIVATE, 0x103, vec![])]));
    ts.pes(0x103, 0xBD, None, &KLV_KEY);
    ts.section(0x1000, &pmt(1, 1, 0x100, &[], &[(H264, 0x100, vec![]), (PRIVATE, 0x103, vec![])]));
    let r = analyze(&ts.out);
    let got: Vec<_> = r.streams().map(|s| s.codec).collect();
    assert_eq!(got, [Codec::H264, Codec::KlvAsync]);
}

#[test]
fn counts_continuity_errors_and_pcr() {
    let mut ts = Ts::new();
    ts.section(0, &pat(1, 0, &[(1, 0x1000)]));
    ts.section(0x1000, &pmt(1, 0, 0x100, &[], &[(H264, 0x100, vec![])]));
    ts.pcr(0x100, 27_000_000 * 5 + 17);
    for _ in 0..3 {
        ts.pes(0x100, 0xE0, None, &[0; 10]);
    }
    // Drop the second video PES packet.
    let mut data = ts.out.clone();
    let second_video = 188 * 4;
    data.drain(second_video..second_video + 188);
    let r = analyze(&data);
    let s = r.streams().next().unwrap();
    assert_eq!(s.cc_errors, 1);
    assert_eq!(r.programs[0].last_pcr, Some(27_000_000 * 5 + 17));
}

#[test]
fn private_types_identified_by_descriptor() {
    let ac3 = vec![0x6A, 1, 0];
    let sub = vec![0x59, 8, b'e', b'n', b'g', 0x10, 0, 1, 0, 1];
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
                (PRIVATE, 0x101, ac3),
                (PRIVATE, 0x102, sub),
                (PRIVATE, 0x103, registration(b"Opus")),
                (0x86, 0x104, vec![]),
                (PRIVATE, 0x105, vec![]),
                (0xC3, 0x106, vec![]),
            ],
        ),
    );
    let r = analyze(&ts.out);
    let got: Vec<_> = r.streams().map(|s| s.codec).collect();
    assert_eq!(
        got,
        [Codec::Ac3, Codec::DvbSubtitle, Codec::Opus, Codec::Scte35, Codec::PrivateData, Codec::Unknown]
    );
}

#[test]
fn empty_and_garbage_input() {
    let r = analyze(&[]);
    assert!(r.programs.is_empty());
    let r = analyze(&[0x55; 10_000]);
    assert_eq!(r.stats.packets, 0);
    assert_eq!(r.stats.bytes_skipped, 10_000);
}
