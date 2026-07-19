//! PROBE (claim 3): with two central-directory records sharing one name,
//! does the lenient backend hand back the FIRST record's payload where the
//! strict backend hands back the LAST?

use std::fs;
use std::io::{BufReader, Read as _, Write};

use otterzip_core::{
    __probe_lenient_entries, __probe_lenient_extract, Archive, ExtractOptions, OpenMode,
    OverwritePolicy, Progress,
};
use tempfile::tempdir;
use zip::write::SimpleFileOptions;

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

fn locate_eocd(bytes: &[u8]) -> usize {
    let needle = [0x50u8, 0x4b, 0x05, 0x06];
    let len = bytes.len();
    (len.saturating_sub(65_557)..=len - 4)
        .rev()
        .find(|&i| bytes[i..i + 4] == needle)
        .expect("EOCD missing")
}

fn force_lenient(path: &std::path::Path) {
    let mut bytes = fs::read(path).unwrap();
    let eocd_at = locate_eocd(&bytes);
    let off = eocd_at + 16;
    let bogus = (bytes.len() as u32).saturating_add(256);
    bytes[off..off + 4].copy_from_slice(&bogus.to_le_bytes());
    fs::write(path, &bytes).unwrap();
}

const FIRST: &[u8] = b"AAAA-FIRST-RECORD-benign-readme-shown-by-preview-tools\n";
const LAST: &[u8] = b"BBBB-LAST-RECORD-this-is-what-7zip-and-explorer-extract\n";

/// Replace every occurrence of `from` with `to`. Both must be the same
/// length so no ZIP offset or length field shifts.
fn patch_all(bytes: &mut [u8], from: &[u8], to: &[u8]) -> usize {
    assert_eq!(from.len(), to.len());
    let mut hits = 0;
    let mut i = 0;
    while i + from.len() <= bytes.len() {
        if &bytes[i..i + from.len()] == from {
            bytes[i..i + from.len()].copy_from_slice(to);
            hits += 1;
            i += from.len();
        } else {
            i += 1;
        }
    }
    hits
}

/// Two records named `dup1.txt` (benign first, hostile last) plus `bulk`
/// fat entries so the archive clears the parallel-extract thresholds.
///
/// The `zip` crate refuses to emit a duplicate name, so we write
/// `dup1.txt` / `dup2.txt` (equal length) and rename `dup2` -> `dup1`
/// afterwards. Every length field stays valid; only the two name bytes
/// in each LFH + CDFH change. This is exactly the shape a hostile
/// archive builder emits by hand.
fn build_dup(path: &std::path::Path, bulk: usize) {
    {
        let f = fs::File::create(path).unwrap();
        let mut w = zip::ZipWriter::new(f);
        let opts = SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored)
            .large_file(false);
        w.start_file("dup1.txt", opts).unwrap();
        w.write_all(FIRST).unwrap();
        for i in 0..bulk {
            w.start_file(format!("bulk/pad_{i:02}.bin"), opts).unwrap();
            w.write_all(&noise(512 * 1024, i as u64 + 3)).unwrap();
        }
        w.start_file("dup2.txt", opts).unwrap();
        w.write_all(LAST).unwrap();
        w.finish().unwrap();
    }
    let mut bytes = fs::read(path).unwrap();
    let hits = patch_all(&mut bytes, b"dup2.txt", b"dup1.txt");
    assert_eq!(hits, 2, "expected exactly one LFH + one CDFH name to patch");
    fs::write(path, &bytes).unwrap();
}

fn label(bytes: &[u8]) -> &'static str {
    if bytes == FIRST {
        "FIRST"
    } else if bytes == LAST {
        "LAST"
    } else {
        "??? (neither)"
    }
}

#[test]

#[ignore = "known-failure probe: reproduces an unfixed defect. Run with `cargo test -- --ignored`; delete this attribute when fixed."]
fn lenient_duplicate_name_resolves_to_first_record() {
    let td = tempdir().unwrap();

    // ---- Reference: what does a strict, spec-conformant reader do? ----
    let strict_path = td.path().join("dup_strict.zip");
    build_dup(&strict_path, 8);
    let strict_bytes = {
        let f = fs::File::open(&strict_path).unwrap();
        let mut a = zip::ZipArchive::new(BufReader::new(f)).unwrap();
        println!("strict reader sees {} records", a.len());
        for i in 0..a.len() {
            let zf = a.by_index_raw(i).unwrap();
            if zf.name() == "dup1.txt" {
                println!("  record #{i} name=dup1.txt size={}", zf.size());
            }
        }
        let mut zf = a.by_name("dup1.txt").unwrap();
        let mut b = Vec::new();
        zf.read_to_end(&mut b).unwrap();
        b
    };
    println!("STRICT zip crate by_name(\"dup1.txt\") -> {}", label(&strict_bytes));

    // What OtterZip's own strict backend writes to disk for the same file.
    let strict_dest = td.path().join("out_strict");
    let sopts = ExtractOptions {
        destination: strict_dest.clone(),
        overwrite: OverwritePolicy::Always,
        ..Default::default()
    };
    let sa = Archive::open(&strict_path, OpenMode::Read).unwrap();
    let sr = sa.extract_all::<fn(&Progress) -> bool>(&sopts, None);
    println!("otterzip STRICT backend extract_all -> {sr:?}");
    let strict_on_disk = fs::read(strict_dest.join("dup1.txt")).unwrap_or_default();
    println!(
        "otterzip STRICT backend dup1.txt on disk -> {}",
        label(&strict_on_disk)
    );

    // ---- Subject: the lenient backend on the same bytes ----
    let lenient_path = td.path().join("dup_lenient.zip");
    build_dup(&lenient_path, 8);
    force_lenient(&lenient_path);
    {
        let f = fs::File::open(&lenient_path).unwrap();
        assert!(
            zip::ZipArchive::new(BufReader::new(f)).is_err(),
            "strict must reject so the lenient backend is what runs"
        );
    }
    let listed = __probe_lenient_entries(&lenient_path).unwrap();
    let dups: Vec<_> = listed.iter().filter(|(n, _)| n == "dup1.txt").collect();
    println!(
        "lenient CD walk recovered {} records, {} of them named dup1.txt",
        listed.len(),
        dups.len()
    );

    let lenient_direct = __probe_lenient_extract(&lenient_path, "dup1.txt").unwrap();
    println!(
        "LENIENT extract_entry(\"dup1.txt\") -> {}",
        label(&lenient_direct)
    );

    let lenient_dest = td.path().join("out_lenient");
    let lopts = ExtractOptions {
        destination: lenient_dest.clone(),
        overwrite: OverwritePolicy::Always,
        ..Default::default()
    };
    let la = Archive::open(&lenient_path, OpenMode::Read).unwrap();
    let lr = la.extract_all::<fn(&Progress) -> bool>(&lopts, None);
    println!("LENIENT extract_all -> {lr:?}");
    let lenient_on_disk = fs::read(lenient_dest.join("dup1.txt")).unwrap_or_default();
    println!(
        "LENIENT backend dup1.txt on disk -> {}",
        label(&lenient_on_disk)
    );

    // ---- Same bug on the SERIAL lenient path (5 entries < PARALLEL_MIN_ENTRIES)
    let small_path = td.path().join("dup_small.zip");
    build_dup(&small_path, 3);
    force_lenient(&small_path);
    let small_dest = td.path().join("out_small");
    let small_opts = ExtractOptions {
        destination: small_dest.clone(),
        overwrite: OverwritePolicy::Always,
        ..Default::default()
    };
    let smalla = Archive::open(&small_path, OpenMode::Read).unwrap();
    let smallr = smalla.extract_all::<fn(&Progress) -> bool>(&small_opts, None);
    println!("LENIENT serial-path extract_all -> {smallr:?}");
    let small_on_disk = fs::read(small_dest.join("dup1.txt")).unwrap_or_default();
    println!(
        "LENIENT serial-path dup1.txt on disk -> {}",
        label(&small_on_disk)
    );

    // ---- Shipped DEFAULT overwrite policy (Never) on a duplicate-name archive
    let never_dest = td.path().join("out_never");
    let never_opts = ExtractOptions {
        destination: never_dest,
        ..Default::default() // OverwritePolicy::Never is the default
    };
    let nevera = Archive::open(&lenient_path, OpenMode::Read).unwrap();
    let neverr = nevera.extract_all::<fn(&Progress) -> bool>(&never_opts, None);
    println!("LENIENT, default overwrite=Never -> {neverr:?}");

    println!("=== SUMMARY ===");
    println!("strict zip crate by_name  : {}", label(&strict_bytes));
    println!("otterzip strict on disk   : {}", label(&strict_on_disk));
    println!("otterzip lenient on disk  : {}", label(&lenient_on_disk));
    println!("otterzip lenient (serial) : {}", label(&small_on_disk));

    assert_eq!(dups.len(), 2, "fixture must carry two dup1.txt records");
    assert_eq!(
        label(&lenient_on_disk),
        label(&strict_on_disk),
        "REPRODUCED: lenient backend extracted the {} record where the strict \
         backend extracted the {} record — same archive, two different files",
        label(&lenient_on_disk),
        label(&strict_on_disk),
    );
}
