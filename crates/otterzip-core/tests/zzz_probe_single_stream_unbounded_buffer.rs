//! PROBE: does `SingleStreamBackend::open_entry_stream` buffer the whole
//! decompressed payload into an uncapped `Vec`?
//!
//! single_stream.rs:226 —
//!     let mut buf = Vec::new();
//!     self.extract_entry(entry_path, &mut buf)?;   // decompress ALL of it
//!     Ok(Box::new(io::Cursor::new(buf)))
//! No size ceiling, no compression-ratio check. A small `.gz` whose
//! payload is enormous (classic gzip bomb) therefore turns into an
//! allocation the size of the payload, reachable from the public
//! `Archive::read_entry` API.

use std::alloc::{GlobalAlloc, Layout, System};
use std::fs;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicUsize, Ordering};

use otterzip_core::{Archive, OpenMode};
use tempfile::tempdir;

// ---------------------------------------------------------------------------
// Allocation accounting
// ---------------------------------------------------------------------------

static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

struct Counting;

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        let p = System.alloc(l);
        if !p.is_null() {
            let now = LIVE
                .fetch_add(l.size(), Ordering::Relaxed)
                .wrapping_add(l.size());
            PEAK.fetch_max(now, Ordering::Relaxed);
        }
        p
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        LIVE.fetch_sub(l.size(), Ordering::Relaxed);
        System.dealloc(p, l);
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, new: usize) -> *mut u8 {
        let q = System.realloc(p, l, new);
        if !q.is_null() {
            LIVE.fetch_sub(l.size(), Ordering::Relaxed);
            let now = LIVE.fetch_add(new, Ordering::Relaxed).wrapping_add(new);
            PEAK.fetch_max(now, Ordering::Relaxed);
        }
        q
    }
}

#[global_allocator]
static A: Counting = Counting;

fn mib(bytes: usize) -> f64 {
    bytes as f64 / (1024.0 * 1024.0)
}

/// Decompressed payload size of the bomb.
const PAYLOAD: usize = 192 * 1024 * 1024;

#[test]

#[ignore = "known-failure probe: reproduces an unfixed defect. Run with `cargo test -- --ignored`; delete this attribute when fixed."]
fn read_entry_on_a_gz_bomb_stays_within_a_bounded_working_set() {
    let td = tempdir().unwrap();
    let gz = td.path().join("payload.bin.gz");

    // A tiny .gz whose decompressed payload is PAYLOAD bytes of zeros.
    {
        let f = fs::File::create(&gz).unwrap();
        let mut enc = flate2::write::GzEncoder::new(f, flate2::Compression::fast());
        let block = vec![0u8; 1024 * 1024];
        let mut done = 0;
        while done < PAYLOAD {
            enc.write_all(&block).unwrap();
            done += block.len();
        }
        enc.finish().unwrap();
    }
    let on_disk = fs::metadata(&gz).unwrap().len() as usize;
    println!("gz on disk              : {} bytes", on_disk);
    println!("declared payload        : {:.0} MiB", mib(PAYLOAD));
    println!("compression ratio       : {:.0}:1", PAYLOAD as f64 / on_disk as f64);

    let archive = Archive::open(&gz, OpenMode::Read).unwrap();
    let name = archive
        .entries()
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path;
    println!("synthetic entry name    : {name:?}");

    // --- Control: the streaming path (extract_entry via io::copy). ---
    let base = LIVE.load(Ordering::Relaxed);
    PEAK.store(base, Ordering::Relaxed);
    let report = archive
        .test::<fn(&otterzip_core::Progress) -> bool>(None)
        .unwrap();
    let stream_peak = PEAK.load(Ordering::Relaxed).saturating_sub(base);
    println!(
        "streaming path (test())  : peak {:.1} MiB, entries_tested={}",
        mib(stream_peak),
        report.entries_tested
    );

    // --- Subject: read_entry -> open_entry_stream. ---
    let base = LIVE.load(Ordering::Relaxed);
    PEAK.store(base, Ordering::Relaxed);
    let mut r = archive.read_entry(&name).unwrap();
    let read_entry_peak = PEAK.load(Ordering::Relaxed).saturating_sub(base);
    let mut first = [0u8; 16];
    let n = r.read(&mut first).unwrap();
    println!(
        "read_entry path          : peak {:.1} MiB just to read the first {} bytes",
        mib(read_entry_peak),
        n
    );
    println!(
        "blowup vs streaming      : {:.0}x",
        read_entry_peak as f64 / stream_peak.max(1) as f64
    );

    assert!(
        read_entry_peak < PAYLOAD / 4,
        "read_entry buffered {:.1} MiB for a {} byte .gz file (payload {:.0} MiB) — \
         streaming path needed only {:.1} MiB",
        mib(read_entry_peak),
        on_disk,
        mib(PAYLOAD),
        mib(stream_peak)
    );
}
