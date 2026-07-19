//! PROBE — claim 5: `.tar.gz` / `.tar.bz2` / `.tar.xz` built from MULTIPLE
//! concatenated compressed streams are mishandled.
//!
//! Multi-member/multi-stream containers are legal and common:
//!   * gzip  — RFC 1952 §2.2 "a gzip file consists of a series of members".
//!             Produced by `cat a.gz b.gz`, `gzip -c x >> out.gz`, and bgzip
//!             (BGZF, the samtools/htslib container).
//!   * bzip2 — produced by **pbzip2 / lbzip2**, the standard parallel bzip2
//!             compressors: each worker emits its own stream and they are
//!             concatenated. `bzip2 -d` and `tar -xjf` read them fine.
//!   * xz    — the .xz spec defines a file as one-or-more streams; `xz -d`
//!             decodes all of them.
//!
//! The backend uses the SINGLE-stream decoders (`GzDecoder`, `BzDecoder`,
//! `XzDecoder::new`) rather than `MultiGzDecoder` / `MultiBzDecoder` /
//! `XzDecoder::new_multi_decoder`, so everything past the first stream is
//! invisible.

use std::fs;
use std::io::Write;

use otterzip_core::{Archive, ExtractOptions, OpenMode, OverwritePolicy, ProgressSink};
use tempfile::tempdir;

struct NullSink;
impl ProgressSink for NullSink {
    fn update(&mut self, _: &otterzip_core::Progress) -> bool {
        true
    }
}

/// One logical tar stream holding four members plus the end-of-archive
/// marker, returned as raw bytes so we can slice it at a block boundary.
fn tar_bytes() -> Vec<u8> {
    let mut b = tar::Builder::new(Vec::new());
    for (name, data) in [
        ("one.txt", "AAAA"),
        ("two.txt", "BBBB"),
        ("three.txt", "CCCC"),
        ("four.txt", "DDDD"),
    ] {
        let mut h = tar::Header::new_gnu();
        h.set_path(name).unwrap();
        h.set_size(data.len() as u64);
        h.set_mode(0o644);
        h.set_cksum();
        b.append(&h, data.as_bytes()).unwrap();
    }
    b.into_inner().unwrap()
}

fn gz(data: &[u8]) -> Vec<u8> {
    let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    e.write_all(data).unwrap();
    e.finish().unwrap()
}

fn bz2(data: &[u8]) -> Vec<u8> {
    let mut e = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::default());
    e.write_all(data).unwrap();
    e.finish().unwrap()
}

fn xz(data: &[u8]) -> Vec<u8> {
    let mut e = xz2::write::XzEncoder::new(Vec::new(), 6);
    e.write_all(data).unwrap();
    e.finish().unwrap()
}

/// Compress `tar_bytes()` as TWO concatenated streams split at a 512-byte
/// tar block boundary — byte-for-byte the shape pbzip2 / bgzip emit.
fn split_compressed(compress: fn(&[u8]) -> Vec<u8>) -> Vec<u8> {
    let raw = tar_bytes();
    let mid = (raw.len() / 2 / 512) * 512;
    assert!(mid > 0 && mid < raw.len(), "split point must be interior");
    let mut out = compress(&raw[..mid]);
    out.extend_from_slice(&compress(&raw[mid..]));
    out
}

fn run_case(label: &str, ext: &str, compress: fn(&[u8]) -> Vec<u8>) -> bool {
    let dir = tempdir().unwrap();
    let path = dir.path().join(format!("multi.{ext}"));
    let blob = split_compressed(compress);
    fs::write(&path, &blob).unwrap();
    println!("=== {label} ({} bytes, 2 concatenated streams) ===", blob.len());

    let archive = match Archive::open(&path, OpenMode::Read) {
        Ok(a) => a,
        Err(e) => {
            println!("  Archive::open FAILED: {e}");
            return false;
        }
    };

    match archive.entries() {
        Ok(it) => {
            let names: Vec<_> = it
                .map(|e| e.map(|e| e.path).unwrap_or_else(|e| format!("<err: {e}>")))
                .collect();
            println!("  entries() -> {:?}", names);
        }
        Err(e) => println!("  entries() FAILED: {e}"),
    }

    let dest = dir.path().join("out");
    fs::create_dir_all(&dest).unwrap();
    let opts = ExtractOptions {
        destination: dest.clone(),
        overwrite: OverwritePolicy::Always,
        ..Default::default()
    };
    let mut sink = NullSink;
    match archive.extract_all(&opts, Some(&mut sink)) {
        Ok(r) => println!(
            "  extract_all -> Ok(extracted={}, skipped={}, bytes={})",
            r.entries_extracted, r.entries_skipped, r.bytes_written
        ),
        Err(e) => println!("  extract_all -> Err({e})"),
    }

    let mut on_disk: Vec<String> = fs::read_dir(&dest)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    on_disk.sort();
    println!("  on disk -> {:?}", on_disk);
    on_disk.len() == 4
}

/// Control case, NOT a bug: two COMPLETE tarballs concatenated
/// (`cat a.tar.gz b.tar.gz`). Stream 1 ends with a valid end-of-archive
/// marker, and tar stops there by definition. Verified against GNU tar
/// 1.35 on the same shape:
///
/// ```text
/// $ tar -tzf cat.tar.gz
/// ./
/// ./first.txt
/// ./second.txt        <- members of b.tar.gz not listed; exit 0
/// ```
///
/// So seeing only stream 1's members here is correct — this test pins that
/// down so a future multi-stream fix is not "corrected" past tar semantics.
#[test]
#[ignore = "known-failure probe: reproduces an unfixed defect. Run with `cargo test -- --ignored`; delete this attribute when fixed."]
fn concatenated_tarballs_stop_at_first_eof_marker_like_gnu_tar() {
    fn one_tar(names: &[&str]) -> Vec<u8> {
        let mut b = tar::Builder::new(Vec::new());
        for n in names {
            let mut h = tar::Header::new_gnu();
            h.set_path(n).unwrap();
            h.set_size(4);
            h.set_mode(0o644);
            h.set_cksum();
            b.append(&h, &b"DATA"[..]).unwrap();
        }
        b.into_inner().unwrap()
    }

    let dir = tempdir().unwrap();
    let path = dir.path().join("cat.tar.gz");
    let mut blob = gz(&one_tar(&["first.txt", "second.txt"]));
    blob.extend_from_slice(&gz(&one_tar(&["third.txt", "fourth.txt"])));
    fs::write(&path, &blob).unwrap();

    let archive = Archive::open(&path, OpenMode::Read).unwrap();
    let names: Vec<_> = archive
        .entries()
        .unwrap()
        .map(|e| e.unwrap().path)
        .collect();
    println!("=== cat a.tar.gz b.tar.gz ===");
    println!("  entries() -> {:?}", names);

    let dest = dir.path().join("out");
    fs::create_dir_all(&dest).unwrap();
    let opts = ExtractOptions {
        destination: dest.clone(),
        overwrite: OverwritePolicy::Always,
        ..Default::default()
    };
    let mut sink = NullSink;
    let report = archive.extract_all(&opts, Some(&mut sink)).unwrap();
    println!(
        "  extract_all -> Ok(extracted={}, skipped={}, warnings={:?})",
        report.entries_extracted, report.entries_skipped, report.warnings
    );
    let mut on_disk: Vec<String> = fs::read_dir(&dest)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    on_disk.sort();
    println!("  on disk -> {:?}", on_disk);

    assert_eq!(on_disk.len(), 2, "matches GNU tar: stop at the EOF marker");
}

/// The nastier real-world variant: when the gzip member boundary happens to
/// land on a tar HEADER boundary, there is no I/O error at all — the members
/// past stream 1 simply do not exist and `extract_all` reports success.
///
/// Observed on a fixture built with REAL GNU tar 1.35 + REAL gzip
/// (40 files, split at a 512-byte block boundary, `gzip -t` VALID):
/// ```text
/// GNU tar -tzf listed : 41
/// OtterZip entries()  : 24   (error: None)
/// ```
#[test]
#[ignore = "known-failure probe: reproduces an unfixed defect. Run with `cargo test -- --ignored`; delete this attribute when fixed."]
fn probe_multistream_silent_member_loss() {
    // 8 members, each exactly 1024 bytes on the wire (512 header + 512 data),
    // then the end-of-archive marker. Split after member 3.
    let mut b = tar::Builder::new(Vec::new());
    for i in 0..8 {
        let mut h = tar::Header::new_gnu();
        h.set_path(format!("m{i}.txt")).unwrap();
        h.set_size(4);
        h.set_mode(0o644);
        h.set_cksum();
        b.append(&h, &b"DATA"[..]).unwrap();
    }
    b.finish().unwrap();
    let raw = b.into_inner().unwrap();

    let split = 3 * 1024;
    let mut blob = gz(&raw[..split]);
    blob.extend_from_slice(&gz(&raw[split..]));

    let dir = tempdir().unwrap();
    let path = dir.path().join("boundary.tar.gz");
    fs::write(&path, &blob).unwrap();

    let archive = Archive::open(&path, OpenMode::Read).unwrap();
    let names: Vec<_> = archive
        .entries()
        .unwrap()
        .map(|e| e.map(|e| e.path).unwrap_or_else(|e| format!("<err: {e}>")))
        .collect();
    println!("=== split on a tar header boundary ===");
    println!("  entries() -> {:?}  (expected 8 members)", names);

    let dest = dir.path().join("out");
    fs::create_dir_all(&dest).unwrap();
    let opts = ExtractOptions {
        destination: dest.clone(),
        overwrite: OverwritePolicy::Always,
        ..Default::default()
    };
    let mut sink = NullSink;
    let report = archive.extract_all(&opts, Some(&mut sink)).unwrap();
    println!(
        "  extract_all -> Ok(extracted={}, skipped={}, warnings={:?})",
        report.entries_extracted, report.entries_skipped, report.warnings
    );

    assert_eq!(
        report.entries_extracted, 8,
        "REPRODUCED: members past the first gzip stream vanished with NO \
         error and NO warning — extract_all reported success"
    );
}

#[test]

#[ignore = "known-failure probe: reproduces an unfixed defect. Run with `cargo test -- --ignored`; delete this attribute when fixed."]
fn probe_multistream_tar_containers() {
    let gz_ok = run_case("tar.gz  (bgzip / cat a.gz b.gz)", "tar.gz", gz);
    let bz_ok = run_case("tar.bz2 (pbzip2 / lbzip2)", "tar.bz2", bz2);
    let xz_ok = run_case("tar.xz  (cat a.xz b.xz)", "tar.xz", xz);

    println!("--- summary: gz_ok={gz_ok} bz2_ok={bz_ok} xz_ok={xz_ok} ---");
    assert!(
        gz_ok && bz_ok && xz_ok,
        "REPRODUCED: multi-stream tar containers lose every member past the \
         first compressed stream (gz_ok={gz_ok} bz2_ok={bz_ok} xz_ok={xz_ok})"
    );
}
