//! PROBE (claim 4): does the per-entry zip-bomb gate false-positive on a
//! zero-filled / sparse file and abort the whole extract?

use std::fs;
use std::io::Write;

use otterzip_core::{Archive, ExtractOptions, OpenMode, OtterzipError, Progress};
use tempfile::tempdir;
use zip::write::SimpleFileOptions;

fn build_zip(path: &std::path::Path, entries: &[(&str, Vec<u8>)]) {
    let f = fs::File::create(path).unwrap();
    let mut w = zip::ZipWriter::new(f);
    let opts = SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .large_file(true);
    for (name, body) in entries {
        w.start_file(*name, opts).unwrap();
        w.write_all(body).unwrap();
    }
    w.finish().unwrap();
}

fn dump_ratios(path: &std::path::Path) -> Vec<(String, u64, u64, u64)> {
    let a = Archive::open(path, OpenMode::Read).unwrap();
    let mut out = Vec::new();
    for e in a.entries().unwrap() {
        let e = e.unwrap();
        let ratio = e.uncompressed_size / e.compressed_size.max(1);
        println!(
            "  entry {:<16} uncompressed={:>10} compressed={:>8} ratio={:>6}",
            e.path, e.uncompressed_size, e.compressed_size, ratio
        );
        out.push((e.path, e.uncompressed_size, e.compressed_size, ratio));
    }
    out
}

#[test]

#[ignore = "known-failure probe: reproduces an unfixed defect. Run with `cargo test -- --ignored`; delete this attribute when fixed."]
fn zero_filled_file_trips_default_bomb_gate() {
    let td = tempdir().unwrap();

    // --- Case A: a plain 64 MiB zero-filled file (VM disk image, DB page
    // file, preallocated log, `fsutil file createnew` output, etc.)
    let path_a = td.path().join("zeros.zip");
    build_zip(&path_a, &[("disk.img", vec![0u8; 64 * 1024 * 1024])]);
    println!("--- CASE A: single 64 MiB all-zero entry ---");
    let ratios_a = dump_ratios(&path_a);

    let dest_a = td.path().join("out_a");
    let opts_a = ExtractOptions {
        destination: dest_a.clone(),
        ..Default::default() // max_compression_ratio = 1000 (the shipped default)
    };
    let archive_a = Archive::open(&path_a, OpenMode::Read).unwrap();
    let res_a = archive_a.extract_all::<fn(&Progress) -> bool>(&opts_a, None);
    match &res_a {
        Ok(r) => println!("CASE A extract OK: {} entries", r.entries_extracted),
        Err(e) => println!("CASE A extract FAILED: {e:?}"),
    }

    // --- Case B: a realistic mixed archive — one small real file plus one
    // zero-filled file. Demonstrates the abort takes the *whole* run down,
    // not just the offending entry.
    let path_b = td.path().join("mixed.zip");
    let real = b"a genuinely useful document the user wants back\n".repeat(64);
    build_zip(
        &path_b,
        &[
            ("zzz_padding.bin", vec![0u8; 64 * 1024 * 1024]),
            ("important.txt", real.to_vec()),
        ],
    );
    println!("--- CASE B: zero-filled entry FIRST, real file second ---");
    let _ = dump_ratios(&path_b);
    let dest_b = td.path().join("out_b");
    let opts_b = ExtractOptions {
        destination: dest_b.clone(),
        ..Default::default()
    };
    let archive_b = Archive::open(&path_b, OpenMode::Read).unwrap();
    let res_b = archive_b.extract_all::<fn(&Progress) -> bool>(&opts_b, None);
    match &res_b {
        Ok(r) => println!("CASE B extract OK: {} entries", r.entries_extracted),
        Err(e) => println!("CASE B extract FAILED: {e:?}"),
    }
    println!(
        "CASE B: important.txt present on disk? {}",
        dest_b.join("important.txt").exists()
    );

    // --- Case C: control — a normal compressible text file. Should NOT trip.
    let path_c = td.path().join("text.zip");
    let text = b"the quick brown otter jumps over the lazy zip archive tool\n".repeat(200_000);
    build_zip(&path_c, &[("log.txt", text.to_vec())]);
    println!("--- CASE C (control): highly-compressible repeated text ---");
    let _ = dump_ratios(&path_c);
    let dest_c = td.path().join("out_c");
    let opts_c = ExtractOptions {
        destination: dest_c.clone(),
        ..Default::default()
    };
    let archive_c = Archive::open(&path_c, OpenMode::Read).unwrap();
    let res_c = archive_c.extract_all::<fn(&Progress) -> bool>(&opts_c, None);
    match &res_c {
        Ok(r) => println!("CASE C extract OK: {} entries", r.entries_extracted),
        Err(e) => println!("CASE C extract FAILED: {e:?}"),
    }

    // --- Size sweep: at what size does an all-zero DEFLATE entry cross 1000?
    println!("--- SIZE SWEEP: all-zero entry, DEFLATE ---");
    for mb in [1usize, 2, 4, 8, 16, 32, 64] {
        let p = td.path().join(format!("sweep_{mb}.zip"));
        build_zip(&p, &[("z.bin", vec![0u8; mb * 1024 * 1024])]);
        let a = Archive::open(&p, OpenMode::Read).unwrap();
        let e = a.entries().unwrap().next().unwrap().unwrap();
        let ratio = e.uncompressed_size / e.compressed_size.max(1);
        println!(
            "  {:>3} MiB zeros -> compressed {:>7} bytes, ratio {:>5} {}",
            mb,
            e.compressed_size,
            ratio,
            if ratio > 1000 { "*** REFUSED ***" } else { "ok" }
        );
    }

    // --- 7z / LZMA reaches far higher ratios on the same benign input.
    {
        use otterzip_core::format::CompressionMethod as Cm;
        use otterzip_core::{ArchiveFormat, CreateOptions};
        let srcdir = td.path().join("srcz");
        fs::create_dir_all(&srcdir).unwrap();
        fs::write(srcdir.join("disk.img"), vec![0u8; 8 * 1024 * 1024]).unwrap();
        let sevenz = td.path().join("zeros.7z");
        let copts = CreateOptions {
            format: ArchiveFormat::SevenZ,
            compression: Cm::Lzma2,
            compression_level: 5,
            ..Default::default()
        };
        let mut w = Archive::create(&sevenz, copts).unwrap();
        w.add_file(srcdir.join("disk.img"), "disk.img").unwrap();
        w.commit().unwrap();
        println!("--- 7z / LZMA2, 8 MiB of zeros ---");
        let _ = dump_ratios(&sevenz);
        let dest_z = td.path().join("out_z");
        let zopts = ExtractOptions {
            destination: dest_z,
            ..Default::default()
        };
        let az = Archive::open(&sevenz, OpenMode::Read).unwrap();
        match az.extract_all::<fn(&Progress) -> bool>(&zopts, None) {
            Ok(r) => println!("7z extract OK: {} entries", r.entries_extracted),
            Err(e) => println!("7z extract FAILED: {e:?}"),
        }
    }

    println!("=== SUMMARY ===");
    println!("A ratio = {:?}", ratios_a.first().map(|r| r.3));
    println!(
        "A = {}, B = {}, C = {}",
        if res_a.is_ok() { "extracted" } else { "REFUSED" },
        if res_b.is_ok() { "extracted" } else { "REFUSED" },
        if res_c.is_ok() { "extracted" } else { "REFUSED" },
    );

    // The assertion that makes this a regression test: a plain zero-filled
    // file is NOT a zip bomb and must extract under the shipped defaults.
    if let Err(OtterzipError::ZipBombSuspected { entry, ratio, limit }) = &res_a {
        panic!(
            "REPRODUCED: zero-filled file refused as a zip bomb \
             (entry={entry}, ratio={ratio}, limit={limit})"
        );
    }
    assert!(res_a.is_ok(), "case A failed for another reason: {res_a:?}");
}
