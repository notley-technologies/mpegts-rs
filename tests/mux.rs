//! The write primitives, checked against the standard's packet rules and by
//! feeding everything they write back into the demuxer.
mod common;

use common::Rng;
use mpegts::mux::{
    klv_metadata_pointer_descriptor, AdaptationFlags, Carriage, CellFragment, CellHeader, MuxError, PesUnit, PesWriter,
    ProgramTables, SliceSink, StreamDef, StreamSpec, SyncKlvOptions, TsWriter, MAX_ASYNC_PES_DATA, MAX_CELL_DATA,
    NULL_PACKET,
};
use mpegts::{
    crc32_mpeg2, Codec, Demuxer, Output, PacketHeader, PcrTimeline, PesHeader, Report, Select, Subscription,
    SubscriptionId, Unit, PCR_WRAP, PTS_WRAP,
};
use std::collections::{HashMap, HashSet};

// ---------------------------------------------------------------- checks

/// Check every packet rule the writers promise (ISO/IEC 13818-1 2.4.3):
/// header bits, adaptation field lengths, continuity, PES packets that end
/// exactly at a packet end (stuffing only in the adaptation field), and
/// sections that start after pointer_field 0 and are followed by 0xFF.
fn check_packets(ts: &[u8], psi_pids: &[u16]) {
    assert_eq!(ts.len() % 188, 0);
    let mut last_cc: HashMap<u16, u8> = HashMap::new();
    // Bytes left in the PES packet or section in progress, per PID
    // (usize::MAX: a PES of unbounded length).
    let mut left: HashMap<u16, usize> = HashMap::new();
    for (i, chunk) in ts.chunks(188).enumerate() {
        let p: &[u8; 188] = chunk.try_into().unwrap();
        let h = PacketHeader::parse(p).unwrap_or_else(|e| panic!("packet {i}: {e}"));
        assert!(!h.transport_error && !h.transport_priority && h.scrambling == 0, "packet {i}");
        assert_ne!(h.adaptation_field_control, 0, "packet {i}: AFC '00'");
        if h.is_null() {
            assert_eq!(p, &NULL_PACKET);
            continue;
        }
        if let Some(af) = h.adaptation_field {
            match h.adaptation_field_control {
                0b11 => assert!(af.length <= 182, "packet {i}: AF length {}", af.length),
                _ => assert_eq!(af.length, 183, "packet {i}"),
            }
            assert!(af.opcr.is_none() && af.splice_countdown.is_none() && af.private_data_length.is_none());
            assert!(!af.extension);
            // Stuffing after the fields is 0xFF.
            let fields = if af.length == 0 { 0 } else { 1 + if af.pcr.is_some() { 6 } else { 0 } };
            assert!(p[5 + fields..5 + usize::from(af.length)].iter().all(|&b| b == 0xFF), "packet {i}: stuffing");
        }
        let has_payload = h.adaptation_field_control & 1 != 0;
        match last_cc.get(&h.pid) {
            Some(&prev) if has_payload && !h.discontinuity() => {
                assert_eq!(h.continuity_counter, (prev + 1) & 15, "packet {i}: CC on PID {:#x}", h.pid)
            }
            Some(&prev) if !has_payload && !h.discontinuity() => {
                assert_eq!(h.continuity_counter, prev, "packet {i}: CC without payload")
            }
            _ => {}
        }
        // A packet without payload repeats the counter; with a
        // discontinuity_indicator it sets the base the next one continues.
        if has_payload || h.discontinuity() {
            last_cc.insert(h.pid, h.continuity_counter);
        }
        if !has_payload {
            continue;
        }
        let mut payload = h.payload(p);
        let rest = left.entry(h.pid).or_insert(0);
        if h.payload_unit_start {
            // An unbounded (video) PES ends where the next one starts.
            assert!(*rest == 0 || *rest == usize::MAX, "packet {i}: unit on PID {:#x} started before the previous ended", h.pid);
            if psi_pids.contains(&h.pid) {
                assert_eq!(payload[0], 0, "pointer_field");
                payload = &payload[1..];
                *rest = 3 + ((usize::from(payload[1] & 0x0F) << 8) | usize::from(payload[2]));
            } else {
                let pes = PesHeader::parse(payload).expect("PES header in the first packet");
                *rest = pes.total_length().unwrap_or(usize::MAX);
            }
        }
        if *rest == usize::MAX {
            continue;
        }
        let n = payload.len().min(*rest);
        *rest -= n;
        if psi_pids.contains(&h.pid) {
            assert!(payload[n..].iter().all(|&b| b == 0xFF), "packet {i}: 0xFF after the section");
        } else {
            assert_eq!(n, payload.len(), "packet {i}: payload bytes after the PES packet's end");
        }
    }
    assert!(left.values().all(|&n| n == 0 || n == usize::MAX), "a unit was cut short");
}

struct Demuxed {
    report: Report,
    outputs: Vec<Output>,
}

impl Demuxed {
    fn of(&self, id: SubscriptionId) -> Vec<&Output> {
        self.outputs.iter().filter(|o| o.subscription == id).collect()
    }
}

fn demux(ts: &[u8], subs: &[Subscription]) -> (Demuxed, Vec<SubscriptionId>) {
    let mut config = mpegts::Config::default();
    config.max_pes_bytes = 4 << 20;
    let mut d = Demuxer::with_config(config).unwrap();
    let ids = subs.iter().map(|s| d.subscribe(*s).unwrap()).collect();
    for chunk in ts.chunks(1316) {
        d.push(chunk);
    }
    d.finish();
    let outputs: Vec<Output> = d.outputs().collect();
    assert!(outputs.iter().all(|o| !o.damaged && !o.truncated));
    let report = d.report();
    assert_eq!(report.stats.crc_errors, 0);
    assert_eq!(report.stats.sync_losses, 0);
    assert_eq!(report.stats.bytes_skipped, 0);
    assert!(report.pids.iter().all(|p| p.cc_errors == 0 && p.transport_errors == 0));
    (Demuxed { report, outputs }, ids)
}

/// Tables for one program with the given streams, PMT on 0x1000.
fn tables(streams: &[(u16, &StreamSpec)], pcr_pid: Option<u16>) -> ProgramTables {
    let mut t = ProgramTables::new(1);
    t.add_program(1, 0x1000, pcr_pid).unwrap();
    for (pid, spec) in streams {
        t.add_stream(1, *pid, spec).unwrap();
    }
    t
}

fn bytes(len: usize, seed: u64) -> Vec<u8> {
    let mut rng = Rng(seed | 1);
    (0..len).map(|_| rng.next() as u8).collect()
}

// ---------------------------------------------------------------- KLV

#[test]
fn sync_klv_access_units_of_every_size_round_trip() {
    let spec = StreamSpec::klv_sync(0);
    let mut t = tables(&[(0x101, &spec)], None);
    let mut w = PesWriter::new(0x101, &spec).unwrap();
    let mut ts = Vec::new();
    t.write_all(&mut ts).unwrap();
    let mut sizes: Vec<usize> = (1..=400).collect();
    sizes.extend([1000, 4096, 65_000, MAX_CELL_DATA - 1, MAX_CELL_DATA, MAX_CELL_DATA + 1, 65_535, 65_536]);
    sizes.extend([2 * MAX_CELL_DATA, 2 * MAX_CELL_DATA + 1, 200_000, 1_000_000]);
    let mut units = Vec::new();
    let mut cells = 0usize;
    for (i, &len) in sizes.iter().enumerate() {
        let au = bytes(len, i as u64);
        let pts = (i as u64 * 3003 + PTS_WRAP - 50_000) % PTS_WRAP; // crosses the PTS wrap
        let unit = PesUnit::new(&au).with_pts(pts);
        let needed = w.packets_needed(&unit).unwrap();
        assert_eq!(w.write(&unit, &mut ts).unwrap(), needed);
        cells += (len + MAX_CELL_DATA - 1) / MAX_CELL_DATA;
        units.push((pts, au));
    }
    // Each access unit numbers its cells from 0 (X-8).
    assert_eq!(w.sequence_number(), 0);
    check_packets(&ts, &[0, 0x1000]);

    let (d, ids) = demux(
        &ts,
        &[
            Subscription::new(Select::Codec(Codec::KlvSync), Unit::Pes).strip_metadata_au(),
            Subscription::new(Select::Pid(0x101), Unit::Pes),
        ],
    );
    let s = d.report.stream(0x101).unwrap();
    assert_eq!((s.codec, s.stream_type, s.pes_stream_id), (Codec::KlvSync, 0x15, Some(0xFC)));
    assert_eq!(s.metadata_format.unwrap(), b"KLVA");
    assert_eq!(s.registration, None, "no registration descriptor by default (DECISIONS X-1)");

    // strip_metadata_au() gives back exactly the access units, fragments in
    // order, every PES packet of an access unit with its PTS.
    let stripped = d.of(ids[0]);
    let raw = d.of(ids[1]);
    assert_eq!(stripped.len(), cells);
    assert_eq!(raw.len(), cells);
    let mut k = 0;
    for (pts, au) in &units {
        let count = (au.len() + MAX_CELL_DATA - 1) / MAX_CELL_DATA;
        let mut joined = Vec::new();
        for c in 0..count {
            let (o, r) = (stripped[k], raw[k]);
            assert_eq!(o.pts, Some(*pts));
            assert_eq!(o.dts, None);
            joined.extend_from_slice(o.payload());
            // Header: 85 80 05 (81 80 05 on continuation fragments, DECISIONS
            // X-6), real PES_packet_length (st1402-time.md 3.1).
            let h = PesHeader::parse(&r.data).unwrap();
            assert_eq!(h.data_alignment, c == 0, "data_alignment_indicator only where the AU begins (X-6)");
            assert!(h.original && !h.copyright && !h.priority && h.markers_ok);
            assert_eq!((h.header_data_length, h.total_length()), (5, Some(r.data.len())));
            // One cell per PES packet (R-ISO13818-133, R-ISO13818-135).
            let cell = CellHeader::parse(r.payload()).unwrap();
            assert_eq!(usize::from(cell.data_length) + 5, r.payload().len());
            assert_eq!(cell.fragment, CellFragment::of(c, count));
            assert_eq!(cell.random_access, c == 0, "RAI on complete and first cells only (R-ST1402TIME-32)");
            assert_eq!((cell.service_id, cell.sequence_number, cell.decoder_config), (0, c as u8, false));
            assert_eq!(r.payload()[2] & 0x0F, 0x0F, "reserved '1111'");
            k += 1;
        }
        assert_eq!(&joined, au);
    }
}

#[test]
fn async_klv_items_round_trip_and_split_only_above_65532() {
    let spec = StreamSpec::klv_async();
    let mut t = tables(&[(0x102, &spec)], None);
    let mut w = PesWriter::new(0x102, &spec).unwrap();
    let mut ts = Vec::new();
    t.write_all(&mut ts).unwrap();
    let mut sizes: Vec<usize> = (1..=300).step_by(3).collect();
    sizes.extend([MAX_ASYNC_PES_DATA - 1, MAX_ASYNC_PES_DATA, MAX_ASYNC_PES_DATA + 1, 2 * MAX_ASYNC_PES_DATA + 5]);
    for (i, &len) in sizes.iter().enumerate() {
        w.write(&PesUnit::new(&bytes(len, 100 + i as u64)), &mut ts).unwrap();
    }
    check_packets(&ts, &[0, 0x1000]);
    let (d, ids) = demux(&ts, &[Subscription::new(Select::Codec(Codec::KlvAsync), Unit::Pes)]);
    let s = d.report.stream(0x102).unwrap();
    assert_eq!((s.codec, s.stream_type, s.pes_stream_id), (Codec::KlvAsync, 0x06, Some(0xBD)));
    assert_eq!(s.registration.unwrap(), b"KLVA");
    let outs = d.of(ids[0]);
    let mut k = 0;
    for (i, &len) in sizes.iter().enumerate() {
        let item = bytes(len, 100 + i as u64);
        let pieces = (len + MAX_ASYNC_PES_DATA - 1) / MAX_ASYNC_PES_DATA;
        let mut joined = Vec::new();
        for piece in 0..pieces {
            let o = outs[k];
            let h = PesHeader::parse(&o.data).unwrap();
            // 00 00 01 BD LL LL 85|81 00 00 (DECISIONS section 2).
            assert_eq!(o.data[6..9], [if piece == 0 { 0x85 } else { 0x81 }, 0x00, 0x00]);
            assert_eq!((h.pts, h.data_alignment, h.total_length()), (None, piece == 0, Some(o.data.len())));
            joined.extend_from_slice(o.payload());
            k += 1;
        }
        assert_eq!(joined, item);
    }
    assert_eq!(k, outs.len());
}

#[test]
fn fragmented_access_units_above_65535_bytes() {
    // ISO/IEC 13818-1 2.12.4: an access unit larger than a PES packet is
    // carried as fragment cells, one per PES packet, every one with the
    // access unit's PTS (DECISIONS section 2, R-ST1402TIME-33).
    let mut w = PesWriter::klv_sync(0x0101, 0).unwrap();
    let au = bytes(140_000, 7);
    let mut ts = Vec::new();
    tables(&[(0x101, &StreamSpec::klv_sync(0))], None).write_all(&mut ts).unwrap();
    w.write(&PesUnit::new(&au).with_pts(123_456), &mut ts).unwrap();
    check_packets(&ts, &[0, 0x1000]);
    let (d, ids) = demux(
        &ts,
        &[
            Subscription::new(Select::Pid(0x101), Unit::Pes),
            Subscription::new(Select::Pid(0x101), Unit::Pes).strip_metadata_au(),
        ],
    );
    let raw = d.of(ids[0]);
    let flags: Vec<u8> = raw.iter().map(|o| o.payload()[2]).collect();
    assert_eq!(flags, [0x9F, 0x0F, 0x4F]);
    assert!(raw.iter().all(|o| o.pts == Some(123_456)));
    let lens: Vec<usize> = raw.iter().map(|o| o.data.len()).collect();
    assert_eq!(lens, [65_541, 65_541, 140_000 - 2 * MAX_CELL_DATA + 19]);
    let joined: Vec<u8> = d.of(ids[1]).iter().flat_map(|o| o.payload().to_vec()).collect();
    assert_eq!(joined, au);
}

#[test]
fn several_services_share_one_sync_stream() {
    // ST 1402-13: more synchronous metadata goes into the existing stream as
    // another service; cells are numbered per access unit (X-8), or with
    // per-stream numbering by one counter over both services.
    let mut spec = StreamSpec::klv_sync(0);
    spec.add_metadata_service(5).unwrap();
    let t = tables(&[(0x101, &spec)], None);
    assert!(t.clone().add_stream(1, 0x102, &StreamSpec::klv_sync(5)).is_err(), "service 5 is taken");
    let mut w = PesWriter::new(0x101, &spec).unwrap();
    let mut ts = Vec::new();
    t.clone().write_all(&mut ts).unwrap();
    w.write(&PesUnit::new(b"service zero").with_pts(1000), &mut ts).unwrap();
    w.write(&PesUnit::new(b"service five").with_pts(1000).with_service(5), &mut ts).unwrap();
    let (d, ids) = demux(&ts, &[Subscription::new(Select::Pid(0x101), Unit::Pes)]);
    let cells: Vec<(u8, u8)> = d.of(ids[0]).iter().map(|o| (o.payload()[0], o.payload()[1])).collect();
    assert_eq!(cells, [(0, 0), (5, 0)]);
    w.set_cell_numbering(mpegts::mux::CellNumbering::PerStream);
    let mut ts2 = Vec::new();
    t.clone().write_all(&mut ts2).unwrap();
    w.write(&PesUnit::new(b"service zero").with_pts(1000), &mut ts2).unwrap();
    w.write(&PesUnit::new(b"service five").with_pts(1000).with_service(5), &mut ts2).unwrap();
    let (d2, ids2) = demux(&ts2, &[Subscription::new(Select::Pid(0x101), Unit::Pes)]);
    let cells: Vec<(u8, u8)> = d2.of(ids2[0]).iter().map(|o| (o.payload()[0], o.payload()[1])).collect();
    assert_eq!(cells, [(0, 2), (5, 3)], "the per-stream counter ran on under the default too");
    let tags: Vec<u8> = d.report.stream(0x101).unwrap().descriptors.iter().map(|d| d.tag).collect();
    assert_eq!(tags, [0x26, 0x26, 0x27]);
}

#[test]
fn sync_registration_option_and_std_values() {
    let mut options = SyncKlvOptions::new(2);
    options.sync_registration_descriptor = true;
    options.buffer_size = 0x80;
    let spec = StreamSpec::klv_sync_with(&options).unwrap();
    let mut ts = Vec::new();
    tables(&[(0x101, &spec)], None).write_all(&mut ts).unwrap();
    let (d, _) = demux(&ts, &[]);
    let s = d.report.stream(0x101).unwrap();
    assert_eq!(s.registration.unwrap(), b"KLVA");
    assert_eq!(s.descriptor(0x27).unwrap().data, [0xFF, 0xFF, 0xFF, 0xC0, 0x00, 0x80, 0xC0, 0x00, 0x00]);
    assert_eq!(s.descriptor(0x26).unwrap().data[7], 2, "service id");
}

// ---------------------------------------------------------------- generic PES

#[test]
fn generic_pes_lengths_timestamps_and_splitting() {
    let video = StreamSpec::h264();
    let audio = StreamSpec::aac_adts();
    let mut t = tables(&[(0x100, &video), (0x110, &audio)], Some(0x100));
    let mut v = PesWriter::new(0x100, &video).unwrap();
    let mut a = PesWriter::new(0x110, &audio).unwrap();
    let mut ts = Vec::new();
    t.write_all(&mut ts).unwrap();
    let mut video_sizes: Vec<usize> = (0..3000).step_by(7).collect();
    video_sizes.extend([65_524, 65_525, 65_526, 100_000]);
    let mut expected_video = Vec::new();
    for (i, &len) in video_sizes.iter().enumerate() {
        let data = bytes(len, 1000 + i as u64);
        let dts = i as u64 * 3003;
        let pts = if i % 3 == 0 { dts } else { dts + 6006 }; // equal: no DTS written
        let unit = PesUnit::new(&data).with_pts(pts).with_dts(dts).with_data_alignment().with_random_access();
        let unit = if i % 10 == 0 { unit.with_pcr(dts * 300) } else { unit };
        v.write(&unit, &mut ts).unwrap();
        expected_video.push((pts, (pts != dts).then_some(dts), data));
    }
    let audio_sizes = [0usize, 1, 500, 65_527, 65_528, 140_000];
    for (i, &len) in audio_sizes.iter().enumerate() {
        a.write(&PesUnit::new(&bytes(len, 2000 + i as u64)).with_pts(i as u64 * 1920), &mut ts).unwrap();
    }
    check_packets(&ts, &[0, 0x1000]);
    let (d, ids) = demux(
        &ts,
        &[Subscription::new(Select::Pid(0x100), Unit::Pes), Subscription::new(Select::Pid(0x110), Unit::Pes)],
    );
    let vo = d.of(ids[0]);
    assert_eq!(vo.len(), expected_video.len());
    for (o, (pts, dts, data)) in vo.iter().zip(&expected_video) {
        assert_eq!((o.pts, o.dts), (Some(*pts), *dts));
        assert_eq!(o.payload(), &data[..]);
        let h = PesHeader::parse(&o.data).unwrap();
        assert!(h.data_alignment && h.original && h.markers_ok);
        // PES_packet_length 0 only for video that does not fit
        // (R-ISO13818-051).
        let fits = data.len() + h.payload_offset - 6 <= 65_535;
        assert_eq!(h.packet_length == 0, !fits, "{}", data.len());
    }
    // Audio above the limit is split; continuations carry no PTS and no
    // data_alignment_indicator; every PES_packet_length is real.
    let ao = d.of(ids[1]);
    let mut k = 0;
    for (i, &len) in audio_sizes.iter().enumerate() {
        let pieces = if len <= 65_527 { 1 } else { 1 + (len - 65_527 + 65_531) / 65_532 };
        let mut joined = Vec::new();
        for piece in 0..pieces {
            let o = ao[k];
            let h = PesHeader::parse(&o.data).unwrap();
            assert_eq!(h.total_length(), Some(o.data.len()));
            assert_eq!(o.pts, (piece == 0).then_some(i as u64 * 1920));
            assert!(!h.data_alignment);
            joined.extend_from_slice(o.payload());
            k += 1;
        }
        assert_eq!(joined, bytes(len, 2000 + i as u64));
    }
    assert_eq!(k, ao.len());
}

#[test]
fn timestamps_wrap_at_33_bits() {
    let mut v = PesWriter::new(0x100, &StreamSpec::h264()).unwrap();
    let mut ts = Vec::new();
    v.write(&PesUnit::new(&[1, 2, 3]).with_pts(PTS_WRAP + 5).with_dts(PTS_WRAP - 1), &mut ts).unwrap();
    let p: &[u8; 188] = ts[..188].try_into().unwrap();
    let h = PacketHeader::parse(p).unwrap();
    let pes = PesHeader::parse(h.payload(p)).unwrap();
    assert_eq!((pes.pts, pes.dts), (Some(5), Some(PTS_WRAP - 1)));
}

// ---------------------------------------------------------------- packets

#[test]
fn many_pids_interleaved_keep_their_counters() {
    let mut rng = Rng(99);
    let mut writers: Vec<PesWriter> =
        (0..60u16).map(|i| PesWriter::with_carriage(0x200 + i, 0xBD, Carriage::Pes).unwrap()).collect();
    let mut counts = vec![0usize; writers.len()];
    let mut ts = Vec::new();
    for round in 0..1500 {
        let i = rng.below(writers.len());
        let len = rng.below(700);
        let n = writers[i].write(&PesUnit::new(&bytes(len, round)), &mut ts).unwrap();
        counts[i] += n;
    }
    check_packets(&ts, &[]);
    let (d, _) = demux(&ts, &[]);
    for (i, w) in writers.iter().enumerate() {
        let info = d.report.pids.iter().find(|p| p.pid == w.pid()).unwrap();
        assert_eq!(info.packets as usize, counts[i]);
        assert_eq!(usize::from(w.ts().continuity_counter()), counts[i] % 16, "wraps 15 -> 0");
    }
}

#[test]
fn pcr_packets_and_the_timeline() {
    // Video at 25 fps with a PCR every frame and an adaptation-field-only
    // PCR packet between frames; the timeline reads them back.
    let video = StreamSpec::h264();
    let mut t = tables(&[(0x100, &video)], Some(0x100));
    let mut v = PesWriter::new(0x100, &video).unwrap();
    let mut ts = Vec::new();
    t.write_all(&mut ts).unwrap();
    let mut pcrs = Vec::new();
    for i in 0..50u64 {
        let pcr = (PCR_WRAP - 27_000_000 + i * 1_080_000) % PCR_WRAP; // crosses the PCR wrap
        v.write(&PesUnit::new(&bytes(2000, i)).with_pts(i * 3600 + 3600).with_pcr(pcr), &mut ts).unwrap();
        let cc = v.ts().continuity_counter();
        let mid = (pcr + 540_000) % PCR_WRAP;
        v.ts_mut().write_adaptation_only(&AdaptationFlags::new().with_pcr(mid), &mut ts).unwrap();
        assert_eq!(v.ts().continuity_counter(), cc);
        pcrs.extend([pcr, mid]);
    }
    check_packets(&ts, &[0, 0x1000]);
    let mut timeline = PcrTimeline::new(None, 10_000_000);
    let mut timed = Vec::new();
    for p in ts.chunks(188) {
        timeline.push(p.try_into().unwrap(), &mut |t| timed.push(t));
    }
    timeline.flush(&mut |t| timed.push(t));
    assert_eq!(timed.len(), ts.len() / 188);
    let seen: Vec<u64> = timed
        .iter()
        .filter_map(|t| PacketHeader::parse(&t.packet).unwrap().pcr().map(|pcr| (pcr, t.clock)))
        .map(|(pcr, clock)| {
            assert_eq!(clock, Some(pcr));
            pcr
        })
        .collect();
    assert_eq!(seen, pcrs);
    assert!(timed.windows(2).all(|w| w[0].position <= w[1].position));
    let (d, _) = demux(&ts, &[]);
    let p = d.report.program(1).unwrap();
    assert_eq!(p.pcr_span, Some(99 * 540_000));
}

#[test]
fn short_sinks_change_nothing() {
    let au = bytes(70_000, 3);
    let mut w = PesWriter::klv_sync(0x101, 0).unwrap();
    w.set_cell_numbering(mpegts::mux::CellNumbering::PerStream);
    w.set_sequence_number(254);
    w.ts_mut().set_continuity_counter(14);
    let unit = PesUnit::new(&au).with_pts(0);
    let needed = w.packets_needed(&unit).unwrap();
    let mut buf = vec![0u8; needed * 188];
    let mut short = SliceSink::new(&mut buf[..needed * 188 - 1]);
    assert_eq!(w.write(&unit, &mut short), Err(MuxError::BufferTooSmall { needed: needed * 188 }));
    assert!(short.is_empty());
    assert_eq!((w.sequence_number(), w.ts().continuity_counter()), (254, 14));
    let mut sink = SliceSink::new(&mut buf);
    assert_eq!(w.write(&unit, &mut sink), Ok(needed));
    assert_eq!(sink.len(), needed * 188);
    assert_eq!(w.sequence_number(), 0, "two cells: 254, 255");

    let mut t = tables(&[(0x101, &StreamSpec::klv_sync(0))], None);
    let mut tiny = [0u8; 188];
    assert!(matches!(t.write_all(&mut SliceSink::new(&mut tiny)), Err(MuxError::BufferTooSmall { needed: 376 })));
    assert_eq!(t.writer(0).unwrap().continuity_counter(), 0);
    assert_eq!(t.write_pat(&mut SliceSink::new(&mut tiny)), Ok(1));
}

#[test]
fn discontinuity_lets_the_counter_jump() {
    let mut w = PesWriter::with_carriage(0x300, 0xC0, Carriage::Pes).unwrap();
    let mut ts = Vec::new();
    w.write(&PesUnit::new(&[1; 10]).with_pts(0), &mut ts).unwrap();
    w.ts_mut().set_continuity_counter(9);
    w.ts_mut().mark_discontinuity();
    w.write(&PesUnit::new(&[2; 10]).with_pts(1), &mut ts).unwrap();
    check_packets(&ts, &[]);
    let (d, _) = demux(&ts, &[]);
    assert_eq!(d.report.pids[0].cc_errors, 0);
}

// ---------------------------------------------------------------- tables

#[test]
fn tables_round_trip_through_the_demuxer() {
    let mut t = ProgramTables::new(0x0BAD);
    t.set_network_pid(Some(0x0010)).unwrap();
    t.add_program(1, 0x1000, Some(0x100)).unwrap();
    t.add_program(2, 0x1000, Some(0x200)).unwrap(); // PMTs sharing a PID
    t.add_program(3, 0x1003, None).unwrap();
    t.add_program_descriptor(1, 0x25, &klv_metadata_pointer_descriptor(0, 1)[2..]).unwrap();
    t.add_stream(1, 0x100, &StreamSpec::h264()).unwrap();
    t.add_stream(1, 0x101, &StreamSpec::klv_sync(0)).unwrap();
    t.add_stream(1, 0x102, &StreamSpec::klv_async()).unwrap();
    t.add_stream(2, 0x200, &StreamSpec::h265()).unwrap();
    t.add_stream(2, 0x201, &StreamSpec::aac_adts().with_descriptor(0x0A, b"eng\0")).unwrap();
    t.add_stream(2, 0x202, &StreamSpec::klv_sync(1)).unwrap();
    t.add_stream(3, 0x100, &StreamSpec::h264()).unwrap(); // shared elementary stream
    t.add_stream_def(3, StreamDef::new(0x86, 0x300).with_descriptor(0x05, b"CUEI")).unwrap();
    let mut ts = Vec::new();
    for _ in 0..3 {
        t.write_all(&mut ts).unwrap();
    }
    check_packets(&ts, &[0, 0x1000, 0x1003]);
    let (d, ids) = demux(&ts, &[Subscription::new(Select::Psi, Unit::Sections)]);
    let sections = d.of(ids[0]);
    assert_eq!(sections.len(), 3 * 4);
    for s in &sections {
        assert_eq!(s.crc_ok, Some(true));
        assert_eq!(crc32_mpeg2(&s.data), 0);
    }
    let r = &d.report;
    assert_eq!((r.transport_stream_id, r.network_pid, r.pat_version), (Some(0x0BAD), Some(0x10), Some(t.pat_version())));
    for p in t.programs() {
        let rp = r.program(p.number).unwrap();
        assert_eq!((rp.pmt_pid, rp.version), (p.pmt_pid, Some(p.version)));
        assert_eq!(rp.pcr_pid.unwrap_or(0x1FFF), p.pcr_pid);
        assert_eq!(rp.descriptors, p.descriptors);
        let streams: Vec<StreamDef> = rp.streams.iter().map(StreamDef::from_stream).collect();
        assert_eq!(streams, p.streams);
    }
    assert_eq!(r.stream(0x201).unwrap().language.unwrap().as_str(), "eng");
    assert_eq!(r.program(2).unwrap().stream(0x202).unwrap().codec, Codec::KlvSync);

    // Tables rebuilt from the report write the same sections, and a change
    // to them is one version step past the input.
    let mut copy = ProgramTables::from_report(r).unwrap();
    assert_eq!(copy.pat_section(), t.pat_section());
    for p in t.programs() {
        assert_eq!(copy.pmt_section(p.number).unwrap(), t.pmt_section(p.number).unwrap());
    }
    copy.add_stream(1, 0x103, &StreamSpec::klv_sync(2)).unwrap();
    assert_eq!(copy.program(1).unwrap().version, (t.program(1).unwrap().version + 1) % 32);
    assert_eq!(copy.pat_version(), t.pat_version());
}

#[test]
fn from_report_needs_complete_tables() {
    let r = mpegts::analyze(&[]);
    assert!(ProgramTables::from_report(&r).is_err(), "no PAT");
    let mut t = ProgramTables::new(1);
    t.add_program(1, 0x1000, None).unwrap();
    let mut ts = Vec::new();
    t.write_pat(&mut ts).unwrap(); // no PMT
    let r = mpegts::analyze(&ts);
    assert!(matches!(ProgramTables::from_report(&r), Err(MuxError::InvalidProgram { number: 1, .. })));
    t.write_pmt(1, &mut ts).unwrap();
    assert!(ProgramTables::from_report(&mpegts::analyze(&ts)).is_ok());
}

#[test]
fn version_changes_are_seen_by_the_demuxer() {
    let mut t = tables(&[(0x100, &StreamSpec::h264())], Some(0x100));
    let mut ts = Vec::new();
    let mut versions = Vec::new();
    for i in 0..40u16 {
        if i > 0 {
            if i % 2 == 0 {
                t.add_stream(1, 0x100 + i, &StreamSpec::h264()).unwrap();
            } else {
                t.remove_stream(1, 0x100 + i - 1).unwrap_or(());
                t.add_program_descriptor(1, 0x80, &[i as u8]).unwrap();
            }
        }
        t.write_all(&mut ts).unwrap();
        versions.push(t.program(1).unwrap().version);
        let (d, _) = demux(&ts, &[]);
        assert_eq!(d.report.program(1).unwrap().version, Some(t.program(1).unwrap().version));
        assert_eq!(d.report.program(1).unwrap().streams.len(), t.program(1).unwrap().streams.len());
    }
    // One step per change, modulo 32.
    for w in versions.windows(2) {
        assert_ne!(w[0], w[1]);
    }
    assert!(versions.contains(&31) && versions.windows(2).any(|w| w[1] < w[0]), "wrapped: {versions:?}");
    check_packets(&ts, &[0, 0x1000]);
}

#[test]
fn psi_crc_holds_for_random_tables() {
    let mut rng = Rng(5);
    for round in 0..200u64 {
        let mut t = ProgramTables::new(rng.next() as u16);
        let programs = 1 + rng.below(6);
        let mut used = HashSet::new();
        for n in 0..programs {
            let pmt = 0x1000 + n as u16;
            t.add_program(1 + n as u16, pmt, Some(0x1FF0)).unwrap();
            for _ in 0..rng.below(8) {
                let pid = 0x20 + rng.below(0x100) as u16;
                if !used.insert(pid) {
                    continue;
                }
                let desc = bytes(rng.below(40), round);
                let spec = StreamSpec::new(rng.next() as u8, 0xBD).with_descriptor(0x80, &desc);
                t.add_stream(1 + n as u16, pid, &spec).unwrap();
            }
        }
        let pat = t.pat_section();
        assert_eq!(crc32_mpeg2(&pat), 0);
        for p in t.programs() {
            let s = t.pmt_section(p.number).unwrap();
            assert_eq!(crc32_mpeg2(&s), 0);
            assert_eq!(s[5] & 0x01, 1, "current_next_indicator");
            assert_eq!(s[5] & 0xC0, 0xC0, "reserved bits");
            assert_eq!((s[6], s[7]), (0, 0));
        }
    }
}

#[test]
fn example_in_the_module_docs_decodes() {
    // A whole small stream: tables every frame, video with PCR, both KLV
    // forms, null packets for padding.
    let video = StreamSpec::h264();
    let sync = StreamSpec::klv_sync(0);
    let asynchronous = StreamSpec::klv_async();
    let mut t = tables(&[(0x100, &video), (0x101, &sync), (0x102, &asynchronous)], Some(0x100));
    let mut v = PesWriter::new(0x100, &video).unwrap();
    let mut s = PesWriter::new(0x101, &sync).unwrap();
    let mut a = PesWriter::new(0x102, &asynchronous).unwrap();
    let mut ts = Vec::new();
    for i in 0..30u64 {
        t.write_all(&mut ts).unwrap();
        let pts = 90_000 + i * 3003;
        v.write(&PesUnit::new(&bytes(5000, i)).with_pts(pts).with_pcr((pts - 9000) * 300).with_random_access(), &mut ts)
            .unwrap();
        s.write(&PesUnit::new(&bytes(300, 50 + i)).with_pts(pts), &mut ts).unwrap();
        a.write(&PesUnit::new(&bytes(120, 80 + i)), &mut ts).unwrap();
        ts.extend_from_slice(&NULL_PACKET);
    }
    check_packets(&ts, &[0, 0x1000]);
    let (d, ids) = demux(&ts, &[Subscription::new(Select::Codec(Codec::KlvSync), Unit::Pes).strip_metadata_au()]);
    assert_eq!(d.report.klv().count(), 2);
    let klv = d.of(ids[0]);
    assert_eq!(klv.len(), 30);
    for (i, o) in klv.iter().enumerate() {
        assert_eq!(o.payload(), bytes(300, 50 + i as u64));
        assert_eq!(o.pts, Some(90_000 + i as u64 * 3003));
    }
    let _ = TsWriter::new(0x10).unwrap();
}

// ---------------------------------------------------------------- example and external oracles

#[path = "../examples/mux.rs"]
#[allow(dead_code)]
mod example;

/// The KLV bytes the example writes into each stream.
fn example_klv(options: &example::Options) -> (Vec<u8>, Vec<u8>) {
    let (mut sync, mut asynchronous) = (Vec::new(), Vec::new());
    for frame in 0..options.frames {
        let klv = example::st0601_packet(example::misp_time(frame));
        sync.extend_from_slice(&klv);
        if let Some((at, len)) = options.oversized {
            if at == frame {
                sync.extend(example::noise(len));
            }
        }
        if frame % 3 == 0 {
            asynchronous.extend_from_slice(&klv);
        }
    }
    (sync, asynchronous)
}

#[test]
fn example_stream_reads_back() {
    let options = example::Options { frames: 40, oversized: Some((10, 150_000)) };
    let ts = example::synthetic_stream(&options).unwrap();
    check_packets(&ts, &[0, example::PMT_PID]);
    let (d, ids) = demux(
        &ts,
        &[
            Subscription::new(Select::Codec(Codec::KlvSync), Unit::Pes).strip_metadata_au(),
            Subscription::new(Select::Codec(Codec::KlvAsync), Unit::Pes),
            Subscription::new(Select::Codec(Codec::H264), Unit::Pes),
        ],
    );
    let (sync, asynchronous) = example_klv(&options);
    let joined = |id| d.of(id).iter().flat_map(|o| o.payload().to_vec()).collect::<Vec<u8>>();
    assert_eq!(joined(ids[0]), sync);
    assert_eq!(joined(ids[1]), asynchronous);
    let video = d.of(ids[2]);
    assert_eq!(video.len(), 40);
    // ST 0601 checksum of the first packet (ST 0601 tag 1).
    let first = example::st0601_packet(example::misp_time(0));
    let sum = first[..first.len() - 2]
        .iter()
        .enumerate()
        .fold(0u16, |s, (i, &b)| s.wrapping_add(u16::from(b) << (8 * ((i + 1) % 2))));
    assert_eq!(first[first.len() - 2..], sum.to_be_bytes());
    // Sync KLV PTS equal the frame PTS.
    for (o, v) in d.of(ids[0]).iter().filter(|o| o.payload().len() < 100).zip(&video) {
        assert_eq!(o.pts, v.pts);
    }
}

/// With `MPEGTS_FFPROBE` naming an ffprobe built with the KLV patch series
/// (ffmpeg-klv-fix), check that it reads the example's stream: three
/// streams, the KLV profiles, every packet with its size and PTS, and a
/// fragmented access unit joined back into one packet. With
/// `MPEGTS_FFMPEG` too, also extract both KLV streams with `-c copy -f data`
/// and compare the bytes. Skipped otherwise.
#[test]
fn ffprobe_reads_written_streams() {
    let Ok(ffprobe) = std::env::var("MPEGTS_FFPROBE") else {
        eprintln!("skipping: set MPEGTS_FFPROBE to a patched ffprobe");
        return;
    };
    let options = example::Options { frames: 30, oversized: Some((10, 70_000)) };
    let ts = example::synthetic_stream(&options).unwrap();
    let dir = std::env::temp_dir().join(format!("mpegts-ffprobe-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("written.ts");
    std::fs::write(&path, ts).unwrap();
    let out = std::process::Command::new(ffprobe)
        .args(["-v", "error", "-of", "json", "-show_streams", "-show_packets"])
        .arg(&path)
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(out.stderr.is_empty(), "ffprobe complained: {}", String::from_utf8_lossy(&out.stderr));
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let streams = json["streams"].as_array().unwrap();
    let by_pid = |pid: u16| streams.iter().find(|s| s["id"].as_str() == Some(&format!("0x{pid:x}"))).unwrap();
    assert_eq!(by_pid(example::VIDEO_PID)["codec_name"], "h264");
    assert_eq!(by_pid(example::VIDEO_PID)["width"], 16);
    assert_eq!(by_pid(example::SYNC_KLV_PID)["codec_name"], "klv");
    assert_eq!(by_pid(example::SYNC_KLV_PID)["profile"], "0", "AV_PROFILE_KLVA_SYNC");
    assert_eq!(by_pid(example::ASYNC_KLV_PID)["codec_name"], "klv");
    assert_eq!(by_pid(example::ASYNC_KLV_PID)["profile"], "1", "AV_PROFILE_KLVA_ASYNC");
    let index = |pid| by_pid(pid)["index"].as_u64().unwrap();
    let packets = json["packets"].as_array().unwrap();
    let of = |pid| packets.iter().filter(move |p| p["stream_index"].as_u64() == Some(index(pid))).collect::<Vec<_>>();
    let size = |p: &serde_json::Value| p["size"].as_str().unwrap().parse::<usize>().unwrap();
    let pts = |p: &serde_json::Value| p["pts"].as_u64();
    let frame_pts = |f: u64| example::FIRST_PTS + f * example::FRAME_TICKS;
    // Video: one packet per frame with its PTS.
    let video = of(example::VIDEO_PID);
    assert_eq!(video.iter().map(|p| pts(p)).collect::<Vec<_>>(), (0..30).map(|f| Some(frame_pts(f))).collect::<Vec<_>>());
    // Sync KLV: one packet per access unit with the frame's PTS; the
    // oversized one arrives whole although it took two PES packets.
    let st0601 = example::st0601_packet(0).len();
    let mut want: Vec<(usize, Option<u64>)> = (0..30).map(|f| (st0601, Some(frame_pts(f)))).collect();
    want.insert(11, (70_000, Some(frame_pts(10))));
    assert_eq!(of(example::SYNC_KLV_PID).iter().map(|p| (size(p), pts(p))).collect::<Vec<_>>(), want);
    // Async KLV: no PTS.
    let asynchronous = of(example::ASYNC_KLV_PID);
    assert_eq!(asynchronous.len(), 10);
    assert!(asynchronous.iter().all(|p| size(p) == st0601 && pts(p).is_none()));

    if let Ok(ffmpeg) = std::env::var("MPEGTS_FFMPEG") {
        let (sync, async_klv) = example_klv(&options);
        for (map, want) in [("0:1", sync), ("0:2", async_klv)] {
            let klv = dir.join("out.klv");
            let status = std::process::Command::new(&ffmpeg)
                .args(["-v", "error", "-y", "-i"])
                .arg(&path)
                .args(["-map", map, "-c", "copy", "-f", "data"])
                .arg(&klv)
                .status()
                .unwrap();
            assert!(status.success());
            assert_eq!(std::fs::read(&klv).unwrap(), want, "stream {map}");
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}
