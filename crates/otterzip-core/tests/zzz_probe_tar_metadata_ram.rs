//! PROBE — claim 4: `extract_all` on a tar materialises the WHOLE entry list
//! in RAM before a single byte is written.
//!
//! `TarBackend::extract_all_inner` opens with
//! `let entries_meta = self.read_metadata_uncached()?;` — a `Vec<Entry>` with
//! one heap-allocated `Entry` (owned `String` path + `Option<String>` comment)
//! per member. It is built before the first progress tick and stays alive for
//! the whole extraction.
//!
//! Two things make this more than a memory-footprint nit:
//!   1. the pre-pass runs BEFORE any bomb gate, so `max_total_output_bytes` /
//!      `max_compression_ratio` cannot bound it — the allocation happens while
//!      `bytes_written` is still 0;
//!   2. tar headers are 512 bytes of mostly-zeros and compress ~1000:1, so a
//!      small `.tar.gz` maps to an unbounded `Vec<Entry>`.
//!
//! The probe installs a counting global allocator, cancels extraction at the
//! FIRST progress callback, and prints the peak heap at that instant.

use std::alloc::{GlobalAlloc, Layout, System};
use std::fs;
use std::io::Write;
use std::sync::atomic::{AtomicUsize, Ordering};

use otterzip_core::{Archive, ExtractOptions, OpenMode, OverwritePolicy, ProgressSink};
use tempfile::tempdir;

static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

struct Counting;

// SAFETY: every method forwards verbatim to `System`; the atomics only
// observe sizes and never affect the returned pointers.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        let p = unsafe { System.alloc(l) };
        if !p.is_null() {
            let now = LIVE.fetch_add(l.size(), Ordering::Relaxed) + l.size();
            PEAK.fetch_max(now, Ordering::Relaxed);
        }
        p
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        LIVE.fetch_sub(l.size(), Ordering::Relaxed);
        unsafe { System.dealloc(p, l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, new: usize) -> *mut u8 {
        let np = unsafe { System.realloc(p, l, new) };
        if !np.is_null() {
            LIVE.fetch_sub(l.size(), Ordering::Relaxed);
            let now = LIVE.fetch_add(new, Ordering::Relaxed) + new;
            PEAK.fetch_max(now, Ordering::Relaxed);
        }
        np
    }
}

#[global_allocator]
static ALLOC: Counting = Counting;

/// Cancels at the first progress tick — i.e. the instant the metadata
/// pre-pass has finished and before any entry has been written.
struct CancelAtFirstTick {
    ticks: u32,
    peak_at_first_tick: usize,
    live_at_first_tick: usize,
}
impl ProgressSink for CancelAtFirstTick {
    fn update(&mut self, p: &otterzip_core::Progress) -> bool {
        if self.ticks == 0 {
            self.peak_at_first_tick = PEAK.load(Ordering::Relaxed);
            self.live_at_first_tick = LIVE.load(Ordering::Relaxed);
            println!(
                "  first progress tick: entries_total={} bytes_total={} bytes_processed={}",
                p.entries_total, p.bytes_total, p.bytes_processed
            );
        }
        self.ticks += 1;
        false // cancel
    }
}

fn ustar_header(name: &[u8], size: u64, typeflag: u8) -> [u8; 512] {
    let mut h = [0u8; 512];
    h[..name.len()].copy_from_slice(name);
    h[100..107].copy_from_slice(b"0000644");
    h[108..115].copy_from_slice(b"0000000");
    h[116..123].copy_from_slice(b"0000000");
    h[124..135].copy_from_slice(format!("{size:011o}").as_bytes());
    h[136..147].copy_from_slice(b"00000000000");
    h[156] = typeflag;
    h[257..263].copy_from_slice(b"ustar\0");
    h[263..265].copy_from_slice(b"00");
    for b in h.iter_mut().skip(148).take(8) {
        *b = b' ';
    }
    let sum: u32 = h.iter().map(|&b| u32::from(b)).sum();
    h[148..155].copy_from_slice(format!("{sum:06o}\0").as_bytes());
    h[155] = b' ';
    h
}

/// A `.tar.gz` of `n` empty members — 512 bytes each on the wire, all
/// near-identical, so gzip squashes them to almost nothing.
fn build_many_entry_targz(path: &std::path::Path, n: usize) -> u64 {
    let f = fs::File::create(path).unwrap();
    let mut enc = flate2::write::GzEncoder::new(f, flate2::Compression::default());
    for i in 0..n {
        let name = format!("dir/file{i:09}.bin");
        enc.write_all(&ustar_header(name.as_bytes(), 0, b'0')).unwrap();
    }
    enc.write_all(&[0u8; 1024]).unwrap();
    enc.finish().unwrap().sync_all().unwrap();
    fs::metadata(path).unwrap().len()
}

fn measure(n: usize) -> (u64, usize, usize) {
    let dir = tempdir().unwrap();
    let path = dir.path().join("many.tar.gz");
    let on_wire = build_many_entry_targz(&path, n);

    let dest = dir.path().join("out");
    fs::create_dir_all(&dest).unwrap();

    let archive = Archive::open(&path, OpenMode::Read).unwrap();
    let opts = ExtractOptions {
        destination: dest.clone(),
        overwrite: OverwritePolicy::Always,
        ..Default::default()
    };

    // Baseline the counters immediately before extract_all.
    PEAK.store(LIVE.load(Ordering::Relaxed), Ordering::Relaxed);
    let mut sink = CancelAtFirstTick {
        ticks: 0,
        peak_at_first_tick: 0,
        live_at_first_tick: 0,
    };
    let base = LIVE.load(Ordering::Relaxed);
    let res = archive.extract_all(&opts, Some(&mut sink));
    println!("  extract_all -> {:?}", res.as_ref().err().map(|e| e.to_string()));

    let files_written = fs::read_dir(&dest).map(|d| d.count()).unwrap_or(0);
    println!("  files written before cancel: {files_written}");

    let held = sink.live_at_first_tick.saturating_sub(base);
    (on_wire, held, sink.peak_at_first_tick.saturating_sub(base))
}

#[test]

#[ignore = "known-failure probe: reproduces an unfixed defect. Run with `cargo test -- --ignored`; delete this attribute when fixed."]
fn probe_tar_metadata_prepass_ram() {
    for n in [10_000usize, 40_000, 80_000] {
        println!("=== {n} entries ===");
        let (on_wire, held, peak) = measure(n);
        println!(
            "  archive on disk      : {on_wire} bytes ({:.1} KiB)",
            on_wire as f64 / 1024.0
        );
        println!(
            "  heap HELD at 1st tick: {held} bytes ({:.1} MiB)  = {:.0} B/entry",
            held as f64 / 1048576.0,
            held as f64 / n as f64
        );
        println!(
            "  heap PEAK at 1st tick: {peak} bytes ({:.1} MiB)",
            peak as f64 / 1048576.0
        );
        println!(
            "  amplification        : {:.0}x  (heap / archive bytes)",
            held as f64 / on_wire as f64
        );
    }

    // Sanity floor: if the pre-pass were streamed, heap at the first tick
    // would be O(1), not O(entries).
    let (_, held_small, _) = measure(10_000);
    let (_, held_big, _) = measure(80_000);
    println!("--- held(10k)={held_small}  held(80k)={held_big} ---");
    assert!(
        held_big < held_small * 3,
        "REPRODUCED: heap held at the first progress tick scales linearly \
         with entry count ({held_small} B at 10k entries -> {held_big} B at \
         80k entries, ~190 B/entry), i.e. the whole entry list is materialised \
         before any byte is written and before any bomb gate can bound it. \
         A ~20 MB .tar.xz of headers (1.26 B/entry on the wire) maps to \
         ~3 GB of Vec<Entry>."
    );
}
