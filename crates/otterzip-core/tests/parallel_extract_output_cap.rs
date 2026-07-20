//! Regression: the parallel ZIP extract path must honour the absolute
//! output-byte cap DURING the write, like the serial path. It used to run an
//! unbounded `io::copy` per entry and only check the total AFTER the whole
//! entry was on disk, so a single 8 MiB entry blew ~8x through a 1 MiB cap
//! while the serial path (via CappedWriter) stopped mid-entry. Fixed with a
//! shared-atomic __AtomicCappedWriter so both paths agree.

use std::fs;
use std::io::Write;

use otterzip_core::{Archive, ExtractOptions, OpenMode, OverwritePolicy, Progress};
use tempfile::tempdir;
use zip::write::SimpleFileOptions;

/// Incompressible pseudo-random bytes — keeps the per-entry ratio at ~1 so
/// the zip-bomb ratio gate never fires and the ONLY thing that can stop the
/// write is the absolute output-byte cap.
fn noise(len: usize, seed0: u64) -> Vec<u8> {
    let mut v = Vec::with_capacity(len);
    let mut seed = seed0;
    for _ in 0..len {
        seed = seed
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        v.push((seed >> 33) as u8);
    }
    v
}

/// `entry_count` entries: one 8 MiB "big.bin" plus filler. 8 entries clears
/// `PARALLEL_MIN_ENTRIES`; 7 stays below it and forces the serial loop.
fn build(path: &std::path::Path, entry_count: usize) {
    let f = fs::File::create(path).unwrap();
    let mut w = zip::ZipWriter::new(f);
    let opts = SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Stored)
        .large_file(true);
    w.start_file("big.bin", opts).unwrap();
    w.write_all(&noise(8 * 1024 * 1024, 0xBEEF)).unwrap();
    for i in 1..entry_count {
        w.start_file(format!("filler_{i:02}.bin"), opts).unwrap();
        w.write_all(&noise(4096, i as u64)).unwrap();
    }
    w.finish().unwrap();
}

fn dir_bytes(root: &std::path::Path) -> u64 {
    let mut total = 0;
    if let Ok(rd) = fs::read_dir(root) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                total += dir_bytes(&p);
            } else if let Ok(m) = fs::metadata(&p) {
                total += m.len();
            }
        }
    }
    total
}

const CAP: u64 = 1024 * 1024; // 1 MiB absolute output cap

fn run(label: &str, entry_count: usize, td: &std::path::Path) -> (u64, u64) {
    let zip_path = td.join(format!("{label}.zip"));
    build(&zip_path, entry_count);
    let dest = td.join(format!("out_{label}"));
    let opts = ExtractOptions {
        destination: dest.clone(),
        overwrite: OverwritePolicy::Always,
        max_total_output_bytes: CAP,
        ..Default::default()
    };
    let archive = Archive::open(&zip_path, OpenMode::Read).unwrap();
    let res = archive.extract_all::<fn(&Progress) -> bool>(&opts, None);
    let big = fs::metadata(dest.join("big.bin")).map(|m| m.len()).unwrap_or(0);
    let total = dir_bytes(&dest);
    println!(
        "[{label}] entries={entry_count} result={:?}",
        res.as_ref().map(|r| r.entries_extracted).map_err(|e| format!("{e:?}"))
    );
    println!("[{label}]   cap            = {CAP} bytes");
    println!("[{label}]   big.bin ondisk = {big} bytes");
    println!("[{label}]   dest total     = {total} bytes");
    (big, total)
}

#[test]
fn parallel_path_ignores_output_cap_until_after_the_write() {
    let td = tempdir().unwrap();

    // 7 entries -> below PARALLEL_MIN_ENTRIES -> serial loop -> CappedWriter.
    let (serial_big, serial_total) = run("serial", 7, td.path());
    // 8 entries -> parallel rayon path -> unbounded io::copy per entry.
    let (par_big, par_total) = run("parallel", 8, td.path());

    println!("=== SUMMARY ===");
    println!("serial   big.bin={serial_big:>9}  total={serial_total:>9}  (cap {CAP})");
    println!("parallel big.bin={par_big:>9}  total={par_total:>9}  (cap {CAP})");
    println!(
        "parallel overshoot = {}x the cap",
        par_total as f64 / CAP as f64
    );

    assert!(
        serial_big <= CAP,
        "serial path should honour the cap in-flight, wrote {serial_big}"
    );
    assert!(
        par_total <= CAP,
        "REPRODUCED: parallel path wrote {par_total} bytes under a {CAP}-byte cap \
         (big.bin alone = {par_big}); the serial path stopped at {serial_total}"
    );
}
