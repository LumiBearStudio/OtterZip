//! PROBE: does `Archive::open_multi_auto` build a correct volume list
//! for a real split ZIP set (`name.z01..zNN` + `name.zip`)?
//!
//! Correct disk order per APPNOTE.TXT §8 is z01, z02, ..., .zip (the
//! `.zip` is the LAST disk — it carries the central directory + EOCD).

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use otterzip_core::{Archive, OpenMode};
use tempfile::tempdir;

fn build_zip(out: &Path, entries: &[(&str, &[u8])]) {
    let f = fs::File::create(out).unwrap();
    let mut w = zip::ZipWriter::new(f);
    let opts = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    for (name, body) in entries {
        w.start_file(*name, opts).unwrap();
        w.write_all(body).unwrap();
    }
    w.finish().unwrap();
}

fn pseudo(n: usize, seed: u64) -> Vec<u8> {
    let mut s = seed;
    let mut v = Vec::with_capacity(n);
    for _ in 0..n {
        s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        v.push((s >> 33) as u8);
    }
    v
}

/// Build `stem.z01..z04` + `stem.zip` as a raw byte-split of one real ZIP.
/// Returns (dir, correct_order_paths).
fn make_split_set(dir: &Path, stem: &str) -> Vec<PathBuf> {
    let bodies = vec![
        ("alpha.bin".to_string(), pseudo(8 * 1024, 1)),
        ("beta.bin".to_string(), pseudo(8 * 1024, 2)),
        ("gamma/nested.dat".to_string(), pseudo(8 * 1024, 3)),
        ("delta.bin".to_string(), pseudo(8 * 1024, 4)),
    ];
    let single = dir.join("__source_tmp.zip");
    let refs: Vec<(&str, &[u8])> =
        bodies.iter().map(|(n, b)| (n.as_str(), b.as_slice())).collect();
    build_zip(&single, &refs);
    let blob = fs::read(&single).unwrap();
    fs::remove_file(&single).unwrap();

    let chunk = blob.len() / 5;
    let mut correct = Vec::new();
    for i in 0..4 {
        let p = dir.join(format!("{stem}.z{:02}", i + 1));
        fs::write(&p, &blob[i * chunk..(i + 1) * chunk]).unwrap();
        correct.push(p);
    }
    let last = dir.join(format!("{stem}.zip"));
    fs::write(&last, &blob[4 * chunk..]).unwrap();
    correct.push(last);
    correct
}

fn names(a: &Archive) -> Vec<String> {
    a.volumes()
        .iter()
        .map(|v| v.path.file_name().unwrap().to_string_lossy().into_owned())
        .collect()
}

/// Same function, sibling branch: `discover_split_volumes` derives the
/// 7z candidate name as `format!("{stem}.7z.{idx:03}")` where `stem` is
/// `first.file_stem()`. Show what that actually produces.
#[test]
#[ignore = "known-failure probe: reproduces an unfixed defect. Run with `cargo test -- --ignored`; delete this attribute when fixed."]
fn discover_split_volumes_7z_stem_is_computed_correctly() {
    let first = Path::new("C:/tmp/name.7z.001");
    let stem = first.file_stem().and_then(|s| s.to_str()).unwrap();
    let candidate = format!("{stem}.7z.{:03}", 2);
    println!("first                   : {}", first.display());
    println!("file_stem()             : {stem:?}");
    println!("candidate for volume 2  : {candidate:?}");
    assert_eq!(
        candidate, "name.7z.002",
        "7z sibling discovery must look for name.7z.002"
    );
}

#[test]

#[ignore = "known-failure probe: reproduces an unfixed defect. Run with `cargo test -- --ignored`; delete this attribute when fixed."]
fn open_multi_auto_orders_split_zip_volumes_correctly() {
    let td = tempdir().unwrap();
    let correct = make_split_set(td.path(), "source");
    let expected: Vec<String> = correct
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    println!("CORRECT disk order      : {expected:?}");

    // Control: the explicit-list API is known-good.
    let ctl = Archive::open_multi(&correct, OpenMode::Read);
    println!("open_multi(explicit)    : {:?}", ctl.as_ref().map(names).map_err(|e| e.to_string()));
    assert!(ctl.is_ok(), "control: explicit ordered list must open");

    // Subject: auto-discovery, entered from the last volume (`.zip`) —
    // this is what a user double-clicking / dropping the archive hits.
    let last = td.path().join("source.zip");
    let got = Archive::open_multi_auto(&last, OpenMode::Read);
    match &got {
        Ok(a) => println!("open_multi_auto(.zip)   : Ok {:?}", names(a)),
        Err(e) => println!("open_multi_auto(.zip)   : Err {e}"),
    }

    // Subject 2: entered from the FIRST volume (`.z01`).
    let first = td.path().join("source.z01");
    let got2 = Archive::open_multi_auto(&first, OpenMode::Read);
    match &got2 {
        Ok(a) => println!("open_multi_auto(.z01)   : Ok {:?}", names(a)),
        Err(e) => println!("open_multi_auto(.z01)   : Err {e}"),
    }

    // Causation: `discover_split_volumes` unconditionally puts the
    // caller-supplied path at index 0 and then appends `stem.z01..zNN`.
    // Reproduce both list shapes explicitly and show identical errors.
    let dir = td.path();
    let as_if_entered_from_zip: Vec<PathBuf> = vec![
        dir.join("source.zip"),
        dir.join("source.z01"),
        dir.join("source.z02"),
        dir.join("source.z03"),
        dir.join("source.z04"),
    ];
    println!(
        "explicit [zip,z01..z04] : {:?}",
        Archive::open_multi(&as_if_entered_from_zip, OpenMode::Read)
            .map(|a| names(&a))
            .map_err(|e| e.to_string())
    );
    // Entering from .z01 yields a DUPLICATE first volume and drops the
    // .zip (which holds the EOCD) entirely.
    let as_if_entered_from_z01: Vec<PathBuf> = vec![
        dir.join("source.z01"),
        dir.join("source.z01"),
        dir.join("source.z02"),
        dir.join("source.z03"),
        dir.join("source.z04"),
    ];
    println!(
        "explicit [z01,z01..z04] : {:?}",
        Archive::open_multi(&as_if_entered_from_z01, OpenMode::Read)
            .map(|a| names(&a))
            .map_err(|e| e.to_string())
    );

    let archive = got.expect("open_multi_auto on the .zip volume must open the split set");
    assert_eq!(
        names(&archive),
        expected,
        "auto-discovered volume order must match true disk order"
    );
}
