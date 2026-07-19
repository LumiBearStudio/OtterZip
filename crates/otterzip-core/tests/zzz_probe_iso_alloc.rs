//! PROBE: does the ISO backend allocate the directory record's declared
//! `data_length` before reading any bytes?
//!
//! `IsoBackend::extract_entry` calls `iso9660::read_file_vec`, which does
//! `alloc::vec![0u8; file.size as usize]` up front. `file.size` is the
//! attacker-controlled u32 from the directory record. If that is true a
//! tiny .iso can force a multi-GiB commit.
//!
//! Observation method: a counting global allocator records the largest
//! single allocation request. We craft a ~40 KB ISO whose one file
//! declares 1 GiB and watch what `extract_all` asks the allocator for.

use std::alloc::{GlobalAlloc, Layout, System};
use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};

use otterzip_core::{Archive, ExtractOptions, OpenMode, ProgressSink};
use tempfile::tempdir;

// ---------------------------------------------------------------------
// counting allocator
// ---------------------------------------------------------------------
static MAX_ALLOC: AtomicUsize = AtomicUsize::new(0);

struct Watch;

fn note(size: usize) {
    MAX_ALLOC.fetch_max(size, Ordering::Relaxed);
}

// SAFETY: pure delegation to System, plus an atomic counter bump.
unsafe impl GlobalAlloc for Watch {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        note(l.size());
        unsafe { System.alloc(l) }
    }
    unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
        note(l.size());
        unsafe { System.alloc_zeroed(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        unsafe { System.dealloc(p, l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, new: usize) -> *mut u8 {
        note(new);
        unsafe { System.realloc(p, l, new) }
    }
}

#[global_allocator]
static A: Watch = Watch;

struct NullSink;
impl ProgressSink for NullSink {
    fn update(&mut self, _: &otterzip_core::Progress) -> bool {
        true
    }
}

// ---------------------------------------------------------------------
// ISO builder that can LIE about a file's data_length
// ---------------------------------------------------------------------
fn write_both_endian_u32(dst: &mut [u8], value: u32) {
    dst[0..4].copy_from_slice(&value.to_le_bytes());
    dst[4..8].copy_from_slice(&value.to_be_bytes());
}
fn write_both_endian_u16(dst: &mut [u8], value: u16) {
    dst[0..2].copy_from_slice(&value.to_le_bytes());
    dst[2..4].copy_from_slice(&value.to_be_bytes());
}

fn write_dir_entry(
    data: &mut [u8],
    offset: &mut usize,
    lba: u32,
    declared_size: u32,
    flags: u8,
    name: &str,
) {
    let name_bytes = name.as_bytes();
    let name_len = name_bytes.len();
    let mut entry_len = 33 + name_len;
    if entry_len % 2 != 0 {
        entry_len += 1;
    }
    let start = *offset;
    data[start] = entry_len as u8;
    data[start + 1] = 0;
    write_both_endian_u32(&mut data[start + 2..], lba);
    write_both_endian_u32(&mut data[start + 10..], declared_size);
    data[start + 25] = flags;
    data[start + 28] = 0;
    data[start + 32] = name_len as u8;
    data[start + 33..start + 33 + name_len].copy_from_slice(name_bytes);
    *offset += entry_len;
}

/// Build a minimal ISO with one file whose *real* payload is
/// `content` but whose directory record declares `declared_size`.
fn build_lying_iso(name: &str, content: &[u8], declared_size: u32) -> Vec<u8> {
    let root_lba: u32 = 18;
    let file_lba: u32 = 19;
    let max_lba = file_lba + 1;
    let mut data = vec![0u8; (max_lba as usize + 1) * 2048];

    let pvd = 16 * 2048;
    data[pvd] = 1;
    data[pvd + 1..pvd + 6].copy_from_slice(b"CD001");
    data[pvd + 6] = 1;
    let label = b"PROBE-ALLOC";
    for (i, b) in label.iter().enumerate() {
        data[pvd + 40 + i] = *b;
    }
    for i in label.len()..32 {
        data[pvd + 40 + i] = 0x20;
    }
    data[pvd + 156] = 34;
    write_both_endian_u32(&mut data[pvd + 158..], root_lba);
    write_both_endian_u32(&mut data[pvd + 166..], 2048);
    data[pvd + 156 + 25] = 0x02;
    data[pvd + 156 + 32] = 1;
    data[pvd + 156 + 33] = 0;
    write_both_endian_u32(&mut data[pvd + 80..], max_lba);
    write_both_endian_u16(&mut data[pvd + 128..], 2048);

    let term = 17 * 2048;
    data[term] = 255;
    data[term + 1..term + 6].copy_from_slice(b"CD001");
    data[term + 6] = 1;

    let mut off = root_lba as usize * 2048;
    write_dir_entry(&mut data, &mut off, root_lba, 2048, 0x02, "\0");
    write_dir_entry(&mut data, &mut off, root_lba, 2048, 0x02, "\x01");
    // THE LIE: declared_size, not content.len()
    write_dir_entry(&mut data, &mut off, file_lba, declared_size, 0x00, name);

    let fo = file_lba as usize * 2048;
    data[fo..fo + content.len()].copy_from_slice(content);
    data
}

fn iso_on_disk(path: &Path, declared: u32) -> u64 {
    let bytes = build_lying_iso("BIG.BIN;1", b"only 8 b", declared);
    fs::write(path, &bytes).unwrap();
    bytes.len() as u64
}

#[test]

#[ignore = "known-failure probe: reproduces an unfixed defect. Run with `cargo test -- --ignored`; delete this attribute when fixed."]
fn iso_declared_data_length_is_allocated_before_any_read() {
    // 1 GiB is big enough to be unmistakable in the allocator trace and
    // small enough to allocate safely on a dev box. The field is a u32,
    // so the real ceiling an attacker can name is 4294967295.
    const LIE: u32 = 1024 * 1024 * 1024;

    let td = tempdir().unwrap();
    let src = td.path().join("lying.iso");
    let on_disk = iso_on_disk(&src, LIE);

    let archive = Archive::open(&src, OpenMode::Read).expect("open .iso");
    let entries: Vec<_> = archive
        .entries()
        .unwrap()
        .filter_map(|r| r.ok())
        .collect();
    for e in &entries {
        println!(
            "entry path={:?} declared uncompressed_size={} compressed_size={}",
            e.path, e.uncompressed_size, e.compressed_size
        );
    }

    let dest = td.path().join("out");
    fs::create_dir_all(&dest).unwrap();
    let opts = ExtractOptions {
        destination: dest.clone(),
        ..ExtractOptions::default()
    };

    // Reset right before the call under test so the number we print is
    // attributable to extract_all and nothing else.
    MAX_ALLOC.store(0, Ordering::Relaxed);
    let t0 = std::time::Instant::now();
    let result = archive.extract_all::<NullSink>(&opts, None);
    let peak = MAX_ALLOC.load(Ordering::Relaxed);
    let elapsed = t0.elapsed();

    println!("--------------------------------------------------------");
    println!("iso file on disk         : {on_disk} bytes");
    println!("declared data_length     : {LIE} bytes");
    println!("largest single allocation during extract_all: {peak} bytes");
    println!("amplification            : {}x", peak as u64 / on_disk);
    println!("extract_all result       : {result:?}");
    println!("elapsed                  : {elapsed:?}");
    println!("--------------------------------------------------------");

    // REGRESSION ASSERT (desired behaviour): the backend must not size an
    // allocation off an unvalidated directory-record field. A read that
    // cannot possibly be satisfied — the extent runs past EOF — should be
    // rejected before any buffer is reserved, or the buffer should grow
    // incrementally as bytes actually arrive. 64 MiB is generous headroom
    // for any legitimate chunking strategy.
    const SANE_CEILING: usize = 64 * 1024 * 1024;
    assert!(
        peak <= SANE_CEILING,
        "extract_all reserved {peak} bytes for a {on_disk}-byte ISO because the \
         directory record claimed {LIE}; nothing validated the claim against the \
         actual file length"
    );
}
