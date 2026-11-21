//! The write primitives do not allocate while writing (MUXING_PLAN.md
//! section 3: write into caller-provided sinks without heap allocation).
//! A counting allocator watches the writing calls; configuration (tables,
//! stream specs) happens before it starts counting.

use mpegts::mux::{
    write_pat_section, write_pmt_section, AdaptationFlags, CellFragment, CellHeader, PatEntry, PesUnit, PesWriter,
    PmtEntry, ProgramTables, SliceSink, StreamSpec, TsWriter, KLVA_REGISTRATION_DESCRIPTOR,
};
use mpegts::{PacketHeader, PesHeader};
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering};

struct Counting;

static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);

thread_local! {
    static COUNTING: Cell<bool> = const { Cell::new(false) };
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if COUNTING.with(|c| c.get()) {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        }
        System.alloc(layout)
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        System.dealloc(ptr, layout)
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if COUNTING.with(|c| c.get()) {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        }
        System.realloc(ptr, layout, new_size)
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

/// Allocations made by `f` on this thread.
fn allocations(f: impl FnOnce()) -> usize {
    let before = ALLOCATIONS.load(Ordering::Relaxed);
    COUNTING.with(|c| c.set(true));
    f();
    COUNTING.with(|c| c.set(false));
    ALLOCATIONS.load(Ordering::Relaxed) - before
}

#[test]
fn writing_does_not_allocate() {
    // Configuration allocates; that is allowed.
    let video = StreamSpec::h264();
    let sync = StreamSpec::klv_sync(0);
    let mut tables = ProgramTables::new(1);
    tables.add_program(1, 0x1000, Some(0x100)).unwrap();
    tables.add_stream(1, 0x100, &video).unwrap();
    tables.add_stream(1, 0x101, &sync).unwrap();
    tables.add_stream(1, 0x102, &StreamSpec::klv_async()).unwrap();
    let big = vec![0x5Au8; 200_000];
    let mut buf = vec![0u8; 188 * 4000];

    let n = allocations(|| {
        let mut video_writer = PesWriter::new(0x100, &video).unwrap();
        let mut sync_writer = PesWriter::klv_sync(0x101, 0).unwrap();
        let mut async_writer = PesWriter::klv_async(0x102).unwrap();
        let mut sink = SliceSink::new(&mut buf);
        tables.write_all(&mut sink).unwrap();
        tables.write_pat(&mut sink).unwrap();
        tables.write_pmt(1, &mut sink).unwrap();
        let unit = PesUnit::new(&big[..5000]).with_pts(900_000).with_dts(897_000).with_pcr(1).with_random_access();
        video_writer.packets_needed(&unit).unwrap();
        video_writer.write(&unit, &mut sink).unwrap();
        video_writer.write(&PesUnit::new(&big[..100_000]).with_pts(903_003), &mut sink).unwrap();
        video_writer.ts_mut().write_adaptation_only(&AdaptationFlags::new().with_pcr(27_000_000), &mut sink).unwrap();
        // A fragmented access unit (four cells) and a split async item.
        sync_writer.write(&PesUnit::new(&big).with_pts(900_000), &mut sink).unwrap();
        async_writer.write(&PesUnit::new(&big[..70_000]), &mut sink).unwrap();
        // A refusal does not allocate either.
        assert!(sync_writer.write(&PesUnit::new(&big), &mut SliceSink::new(&mut [0u8; 188])).is_err());

        let mut ts = TsWriter::new(0x200).unwrap();
        let mut count = 0;
        ts.write_pes(&[&[0, 0, 1, 0xBD, 0, 3, 0x81, 0, 0]], &AdaptationFlags::new(), &mut |_p: &[u8; 188]| count += 1)
            .unwrap();

        // Sections into stack buffers.
        let mut section = [0u8; 1024];
        write_pat_section(&mut section, 1, 0, &[PatEntry { program_number: 1, pid: 0x1000 }]).unwrap();
        let entry = PmtEntry { stream_type: 0x06, pid: 0x102, descriptors: &KLVA_REGISTRATION_DESCRIPTOR };
        write_pmt_section(&mut section, 1, 0, 0x1FFF, &[], &[entry]).unwrap();
        tables.pmt_section_into(1, &mut section).unwrap();
        let _ = CellHeader::new(0, 1, CellFragment::Complete, 10).to_bytes();

        // The read-side helpers.
        let written = sink.written();
        for p in written.chunks(188) {
            let p: &[u8; 188] = p.try_into().unwrap();
            let h = PacketHeader::parse(p).unwrap();
            if h.payload_unit_start && (0x100..0x200).contains(&h.pid) {
                PesHeader::parse(h.payload(p)).unwrap();
            }
        }
        assert!(count == 1 && !written.is_empty());
    });
    assert_eq!(n, 0, "the write path allocated {n} times");
}

/// A stream for the injector: PAT and PMT every third frame, H.264 video
/// with a PCR every frame, null padding to 60 packets a frame.
fn injector_input(frames: u64) -> Vec<u8> {
    let video = StreamSpec::h264();
    let mut tables = ProgramTables::new(1);
    tables.add_program(1, 0x1000, Some(0x100)).unwrap();
    tables.add_stream(1, 0x100, &video).unwrap();
    let mut writer = PesWriter::new(0x100, &video).unwrap();
    let frame = vec![0x5Au8; 6000];
    let mut ts = Vec::new();
    for i in 0..frames {
        let start = ts.len();
        if i % 3 == 0 {
            tables.write_all(&mut ts).unwrap();
        }
        let pts = 900_000 + i * 3003;
        writer.write(&PesUnit::new(&frame).with_pts(pts).with_pcr((pts - 36_000) * 300), &mut ts).unwrap();
        while ts.len() - start < 60 * 188 {
            ts.extend_from_slice(&mpegts::mux::NULL_PACKET);
        }
    }
    ts
}

#[test]
fn injector_steady_state_does_not_allocate() {
    use mpegts::mux::{Frame, InjectConfig, Injector, InsertUnit, OnOverflow, RateMode, Units};
    let ts = injector_input(300);
    let half = 150 * 60 * 188;
    for cfg in [InjectConfig::default(), InjectConfig::default().with_rate(RateMode::Cbr { on_overflow: OnOverflow::Fail })]
    {
        let mut inj = Injector::new(cfg).unwrap();
        let mut out = Vec::with_capacity(ts.len() * 2);
        // The caller's units are made before counting starts.
        let mut units: Vec<Vec<u8>> = (0..300).map(|i| vec![i as u8; 100]).collect();
        let mut on_frame = |f: &Frame, u: &mut Units<'_>| {
            if let Some(data) = units.pop() {
                u.insert(InsertUnit::sync(f.pts, data)).unwrap();
            }
        };
        // Warm up: the first half fills every queue and buffer once.
        inj.push_with(&ts[..half], &mut out, &mut on_frame).unwrap();
        let n = allocations(|| {
            for chunk in ts[half..].chunks(1316) {
                inj.push_with(chunk, &mut out, &mut on_frame).unwrap();
            }
        });
        assert_eq!(n, 0, "the injector allocated {n} times in steady state");
        let stats = inj.finish(&mut out).unwrap();
        assert!(stats.units_inserted > 250, "{stats:?}");
    }
}
