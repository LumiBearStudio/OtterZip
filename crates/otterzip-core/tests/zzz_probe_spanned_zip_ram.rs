//! PROBE: does `SpannedZipReader::open` buffer the whole archive in RAM
//! when the EOCD declares a tiny central directory far from the EOCD?
//!
//! The suspect is the "filler" copy in spanned_zip.rs:
//!     let between_size = eocd_pos - (cd_start_virtual + cd_size);
//!     let mut filler = vec![0u8; between_size as usize];   // copy #1
//!     patched_tail.extend_from_slice(&filler);             // copy #2
//! `between_size` is never bounded, so a malformed EOCD that puts the
//! CD at offset 0 with cd_size 0 makes it equal the whole archive.

use std::alloc::{GlobalAlloc, Layout, System};
use std::fs;
use std::io::Write;
use std::path::Path;
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

// ---------------------------------------------------------------------------

const PAYLOAD: usize = 32 * 1024 * 1024;

fn build_stored_zip(out: &Path) {
    let f = fs::File::create(out).unwrap();
    let mut w = zip::ZipWriter::new(f);
    let opts = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Stored);
    w.start_file("big.bin", opts).unwrap();
    // Stored, so the on-disk archive is ~PAYLOAD bytes.
    let block = vec![0xABu8; 64 * 1024];
    let mut written = 0;
    while written < PAYLOAD {
        w.write_all(&block).unwrap();
        written += block.len();
    }
    w.finish().unwrap();
}

#[test]

#[ignore = "known-failure probe: reproduces an unfixed defect. Run with `cargo test -- --ignored`; delete this attribute when fixed."]
fn spanned_zip_open_does_not_buffer_the_whole_archive() {
    let td = tempdir().unwrap();
    let src = td.path().join("big.zip");
    build_stored_zip(&src);

    let mut blob = fs::read(&src).unwrap();
    let file_len = blob.len();
    let eocd_pos = file_len - 22;
    assert_eq!(&blob[eocd_pos..eocd_pos + 4], &[0x50, 0x4b, 0x05, 0x06]);

    // Malformed-but-accepted EOCD:
    //   disk_number      = 1  -> is_real_spanned == true (takes patch path)
    //   disk_with_cd     = 0  -> disk_origin(0) == 0
    //   cd_size          = 0
    //   cd_offset_in_disk= 0  -> cd_start_virtual == 0
    // => between_size == eocd_pos == whole archive.
    blob[eocd_pos + 4..eocd_pos + 6].copy_from_slice(&1u16.to_le_bytes());
    blob[eocd_pos + 6..eocd_pos + 8].copy_from_slice(&0u16.to_le_bytes());
    blob[eocd_pos + 12..eocd_pos + 16].copy_from_slice(&0u32.to_le_bytes());
    blob[eocd_pos + 16..eocd_pos + 20].copy_from_slice(&0u32.to_le_bytes());

    let vol = td.path().join("evil.zip");
    fs::write(&vol, &blob).unwrap();
    drop(blob);

    println!("archive on disk         : {:.1} MiB", mib(file_len));

    // Measure only the open() call: peak live heap above the baseline
    // that was already live when we entered.
    let baseline = LIVE.load(Ordering::Relaxed);
    PEAK.store(baseline, Ordering::Relaxed);
    let result = Archive::open_multi(&[vol.clone()], OpenMode::Read);
    let peak = PEAK.load(Ordering::Relaxed).saturating_sub(baseline);

    println!("peak heap during open   : {:.1} MiB", mib(peak));
    println!("ratio (peak / filesize) : {:.2}x", peak as f64 / file_len as f64);
    println!(
        "open result             : {:?}",
        result.as_ref().map(|_| "Ok").map_err(|e| e.to_string())
    );

    // A streaming reader should need a bounded working set (CD + EOCD),
    // not a multiple of the archive size.
    assert!(
        peak < file_len / 4,
        "SpannedZipReader::open buffered {:.1} MiB of heap for a {:.1} MiB archive ({:.2}x)",
        mib(peak),
        mib(file_len),
        peak as f64 / file_len as f64
    );
}
