//! PROBE: does `extract_entry` materialise a whole entry in RAM, sized by
//! an attacker-declared field, and do `Archive::test` / `read_entry` reach it?
//!
//! `backends/iso.rs::extract_entry` calls `iso9660::read_file_vec`, which is
//!   `let mut buffer = alloc::vec![0u8; file.size as usize];`
//! — the ISO **directory record's declared size**, committed before a single
//! byte is read. The `__copy_capped` ZIP-bomb gate on the extract path only
//! sees bytes *after* that allocation, and `Archive::test` has no gate at all.
//!
//! Second instance, same class: `backends/zip.rs::open_entry_stream` does
//! `Vec::with_capacity(zf.size())` where `zf.size()` is the central
//! directory's *declared* uncompressed size.
//!
//! Same shape as the RAR `bigdecl.rar` fixture (a header that lies about
//! 100 MiB over 11 real bytes), which the RAR backend already defends
//! against — see `rar.rs::extract_entry_never_stages_through_temp`.
//!
//! Note the other backends are fine: zip / tar / 7z / rar / cab / msi /
//! deb / single_stream all stream `extract_entry` through `io::copy`.

use std::alloc::{GlobalAlloc, Layout, System};
use std::fs;
use std::sync::atomic::{AtomicUsize, Ordering};

use otterzip_core::{Archive, ExtractOptions, OpenMode, OverwritePolicy};
use tempfile::tempdir;

// ---------------------------------------------------------------- allocator
static PEAK_SINGLE: AtomicUsize = AtomicUsize::new(0);

struct Watch;

unsafe impl GlobalAlloc for Watch {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        PEAK_SINGLE.fetch_max(l.size(), Ordering::Relaxed);
        System.alloc(l)
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        System.dealloc(p, l)
    }
    unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
        PEAK_SINGLE.fetch_max(l.size(), Ordering::Relaxed);
        System.alloc_zeroed(l)
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        PEAK_SINGLE.fetch_max(n, Ordering::Relaxed);
        System.realloc(p, l, n)
    }
}

#[global_allocator]
static A: Watch = Watch;

fn reset() {
    PEAK_SINGLE.store(0, Ordering::Relaxed);
}
fn peak_mib() -> f64 {
    PEAK_SINGLE.load(Ordering::Relaxed) as f64 / (1024.0 * 1024.0)
}

// ------------------------------------------------------------- ISO builder
const SECTOR: usize = 2048;

fn w_both_u32(dst: &mut [u8], v: u32) {
    dst[0..4].copy_from_slice(&v.to_le_bytes());
    dst[4..8].copy_from_slice(&v.to_be_bytes());
}
fn w_both_u16(dst: &mut [u8], v: u16) {
    dst[0..2].copy_from_slice(&v.to_le_bytes());
    dst[2..4].copy_from_slice(&v.to_be_bytes());
}
fn w_dir(data: &mut [u8], off: usize, lba: u32, size: u32, flags: u8, name: &[u8]) -> usize {
    let mut len = 33 + name.len();
    if len % 2 != 0 {
        len += 1;
    }
    data[off] = len as u8;
    w_both_u32(&mut data[off + 2..], lba);
    w_both_u32(&mut data[off + 10..], size);
    data[off + 25] = flags;
    data[off + 32] = name.len() as u8;
    data[off + 33..off + 33 + name.len()].copy_from_slice(name);
    len
}

/// A tiny ISO whose one file holds `real` bytes but whose directory record
/// *declares* `declared` bytes.
fn build_lying_iso(real: &[u8], declared: u32) -> Vec<u8> {
    let root_lba = 18u32;
    let file_lba = 19u32;
    let total = file_lba as usize + 2;
    let mut d = vec![0u8; total * SECTOR];

    let pvd = 16 * SECTOR;
    d[pvd] = 1;
    d[pvd + 1..pvd + 6].copy_from_slice(b"CD001");
    d[pvd + 6] = 1;
    for (i, b) in b"OTTERZIP-PROBE".iter().enumerate() {
        d[pvd + 40 + i] = *b;
    }
    for i in b"OTTERZIP-PROBE".len()..32 {
        d[pvd + 40 + i] = 0x20;
    }
    d[pvd + 156] = 34;
    w_both_u32(&mut d[pvd + 158..], root_lba);
    w_both_u32(&mut d[pvd + 166..], SECTOR as u32);
    d[pvd + 156 + 25] = 0x02;
    d[pvd + 156 + 32] = 1;
    d[pvd + 156 + 33] = 0;
    w_both_u32(&mut d[pvd + 80..], total as u32);
    w_both_u16(&mut d[pvd + 128..], SECTOR as u16);

    let term = 17 * SECTOR;
    d[term] = 255;
    d[term + 1..term + 6].copy_from_slice(b"CD001");
    d[term + 6] = 1;

    let root = root_lba as usize * SECTOR;
    let mut off = root;
    off += w_dir(&mut d, off, root_lba, SECTOR as u32, 0x02, b"\0");
    off += w_dir(&mut d, off, root_lba, SECTOR as u32, 0x02, b"\x01");
    // THE LIE: declared size, not real.len().
    w_dir(&mut d, off, file_lba, declared, 0x00, b"BIG.BIN;1");

    let fo = file_lba as usize * SECTOR;
    d[fo..fo + real.len()].copy_from_slice(real);
    d
}

#[test]

#[ignore = "known-failure probe: reproduces an unfixed defect. Run with `cargo test -- --ignored`; delete this attribute when fixed."]
fn iso_declared_size_does_not_drive_a_ram_allocation() {
    // 384 MiB declared. Big enough to be unmistakable, small enough not to
    // wedge a CI box. Real payload: 24 bytes.
    const DECLARED: u32 = 384 * 1024 * 1024;
    let real = b"only twenty-four bytes!!";

    let td = tempdir().unwrap();
    let iso = td.path().join("lying.iso");
    let bytes = build_lying_iso(real, DECLARED);
    fs::write(&iso, &bytes).unwrap();
    println!(
        "ISO on disk = {} bytes; directory record declares {} bytes ({:.0} MiB) for BIG.BIN;1",
        bytes.len(),
        DECLARED,
        DECLARED as f64 / 1048576.0
    );

    // Baseline: opening + listing must not allocate anything like that.
    reset();
    let a = Archive::open(&iso, OpenMode::Read).expect("open lying iso");
    let names: Vec<_> = a
        .entries()
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| (e.path, e.uncompressed_size))
        .collect();
    println!("entries = {names:?}");
    println!("peak single allocation, open+list  = {:.1} MiB", peak_mib());

    // --- reachable via Archive::test (no bomb gate on that path at all) ---
    reset();
    let t = a.test::<fn(&otterzip_core::Progress) -> bool>(None);
    let via_test = peak_mib();
    println!("Archive::test -> {t:?}");
    println!("peak single allocation, test()     = {via_test:.1} MiB");

    // --- reachable via read_entry ----------------------------------------
    reset();
    // Backend strips the ISO `;1` version suffix — use the enumerated name.
    let entry_name = names[0].0.clone();
    let r = a.read_entry(&entry_name).map(|_| ());
    let via_read = peak_mib();
    println!("read_entry    -> {r:?}");
    println!("peak single allocation, read_entry = {via_read:.1} MiB");

    // --- and via the ordinary extract path, whose byte cap is downstream --
    reset();
    let opts = ExtractOptions {
        destination: td.path().join("out"),
        overwrite: OverwritePolicy::Always,
        ..Default::default()
    };
    let e = a
        .extract_all::<fn(&otterzip_core::Progress) -> bool>(&opts, None)
        .map(|r| r.entries_extracted);
    let via_extract = peak_mib();
    println!("extract_all   -> {e:?}");
    println!("peak single allocation, extract    = {via_extract:.1} MiB");

    let budget = 16.0; // MiB — generous; the real payload is 24 bytes.
    let worst = via_test.max(via_read).max(via_extract);
    assert!(
        worst < budget,
        "a {} byte ISO made us commit {worst:.1} MiB in one allocation \
         (declared size drives the buffer, so the archive header alone \
         chooses our memory footprint)",
        bytes.len()
    );
}

/// Second instance of the same class: `zip.rs::open_entry_stream` does
/// `Vec::with_capacity(zf.size())`, and `zf.size()` is the *declared*
/// uncompressed size out of the central directory.
#[test]
#[ignore = "known-failure probe: reproduces an unfixed defect. Run with `cargo test -- --ignored`; delete this attribute when fixed."]
fn zip_declared_size_does_not_drive_a_ram_allocation() {
    use std::io::Write as _;

    const DECLARED: u32 = 384 * 1024 * 1024;
    let td = tempdir().unwrap();
    let zp = td.path().join("lying.zip");

    {
        let f = fs::File::create(&zp).unwrap();
        let mut w = zip::ZipWriter::new(f);
        w.start_file(
            "small.bin",
            zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored),
        )
        .unwrap();
        w.write_all(b"twenty-four bytes here!!").unwrap();
        w.finish().unwrap();
    }

    // Patch the declared uncompressed size in both the local file header
    // (PK\x03\x04, field at +22) and the central directory (PK\x01\x02, +24).
    let mut b = fs::read(&zp).unwrap();
    let le = DECLARED.to_le_bytes();
    let mut patched = 0;
    for i in 0..b.len().saturating_sub(30) {
        if &b[i..i + 4] == b"PK\x03\x04" {
            b[i + 22..i + 26].copy_from_slice(&le);
            patched += 1;
        } else if &b[i..i + 4] == b"PK\x01\x02" {
            b[i + 24..i + 28].copy_from_slice(&le);
            patched += 1;
        }
    }
    assert_eq!(patched, 2, "expected one LFH + one CDFH to patch");
    fs::write(&zp, &b).unwrap();
    println!(
        "\nZIP on disk = {} bytes; central directory declares {} bytes ({:.0} MiB)",
        b.len(),
        DECLARED,
        DECLARED as f64 / 1048576.0
    );

    let a = match Archive::open(&zp, OpenMode::Read) {
        Ok(a) => a,
        Err(e) => {
            println!("open rejected the patched zip: {e:?} — path not reachable");
            return;
        }
    };
    let listed: Vec<_> = a
        .entries()
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| (e.path, e.uncompressed_size))
        .collect();
    println!("entries = {listed:?}");

    reset();
    let r = a.read_entry("small.bin").map(|_| ());
    let via_read = peak_mib();
    println!("read_entry -> {r:?}");
    println!("peak single allocation, read_entry = {via_read:.1} MiB");

    assert!(
        via_read < 16.0,
        "a {} byte ZIP made read_entry commit {via_read:.1} MiB in one allocation",
        b.len()
    );
}
