//! Sprint 3 integration tests: tar.gz extraction, 7z extraction,
//! password-protected ZIP, and ZIP-bomb detection.
//!
//! These exercise the multi-backend dispatch + streaming `extract_all`
//! path added in Sprint 3.

use std::fs;
use std::io::Write;

use otterzip_core::{
    Archive, ArchiveFormat, ExtractOptions, OpenMode, OverwritePolicy, OtterzipError,
};
use tempfile::tempdir;

// ---------------------------------------------------------------------------
// tar.gz
// ---------------------------------------------------------------------------

fn build_targz_fixture(out: &std::path::Path) {
    let file = fs::File::create(out).expect("create tar.gz fixture");
    let gz = flate2::write::GzEncoder::new(file, flate2::Compression::default());
    let mut tar = tar::Builder::new(gz);

    let mut header = tar::Header::new_gnu();
    let body_a: &[u8] = b"alpha\n";
    header.set_path("alpha.txt").unwrap();
    header.set_size(body_a.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    tar.append(&header, body_a).unwrap();

    let mut header = tar::Header::new_gnu();
    let body_b: &[u8] = b"beta nested entry contents\n";
    header.set_path("nested/beta.txt").unwrap();
    header.set_size(body_b.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    tar.append(&header, body_b).unwrap();

    let gz = tar.into_inner().unwrap();
    gz.finish().unwrap();
}

#[test]
fn targz_format_detected() {
    let td = tempdir().unwrap();
    let path = td.path().join("fixture.tar.gz");
    build_targz_fixture(&path);
    let fmt = otterzip_core::detect(&path).unwrap();
    assert_eq!(fmt, ArchiveFormat::TarGz);
}

#[test]
fn targz_extract_roundtrip() {
    let td = tempdir().unwrap();
    let archive_path = td.path().join("fixture.tar.gz");
    build_targz_fixture(&archive_path);
    let out = td.path().join("out");

    let opts = ExtractOptions {
        destination: out.clone(),
        overwrite: OverwritePolicy::Always,
        ..Default::default()
    };
    let archive = Archive::open(&archive_path, OpenMode::Read).unwrap();
    let report = archive
        .extract_all::<fn(&otterzip_core::Progress) -> bool>(&opts, None)
        .unwrap();
    assert_eq!(report.entries_extracted, 2);
    assert_eq!(fs::read(out.join("alpha.txt")).unwrap(), b"alpha\n");
    assert_eq!(
        fs::read(out.join("nested").join("beta.txt")).unwrap(),
        b"beta nested entry contents\n"
    );
}

#[test]
fn targz_path_traversal_blocked() {
    let td = tempdir().unwrap();
    let path = td.path().join("evil.tar.gz");

    {
        // tar's `Header::set_path` validates and rejects `..`. Bypass it by
        // writing into the legacy header byte field directly — the on-disk
        // archive then contains the malicious path verbatim, which is what
        // a real attacker's tar would do.
        let file = fs::File::create(&path).unwrap();
        let gz = flate2::write::GzEncoder::new(file, flate2::Compression::default());
        let mut tar = tar::Builder::new(gz);
        let mut header = tar::Header::new_old();
        let raw_name = b"../escape.txt";
        let name_field = &mut header.as_old_mut().name;
        name_field[..raw_name.len()].copy_from_slice(raw_name);
        let body: &[u8] = b"pwn";
        header.set_size(body.len() as u64);
        header.set_mode(0o644);
        header.set_entry_type(tar::EntryType::Regular);
        header.set_cksum();
        tar.append(&header, body).unwrap();
        tar.into_inner().unwrap().finish().unwrap();
    }

    let out = td.path().join("out");
    let opts = ExtractOptions {
        destination: out.clone(),
        overwrite: OverwritePolicy::Always,
        block_path_traversal: true,
        ..Default::default()
    };
    let archive = Archive::open(&path, OpenMode::Read).unwrap();
    let err = archive
        .extract_all::<fn(&otterzip_core::Progress) -> bool>(&opts, None)
        .unwrap_err();
    assert!(matches!(err, OtterzipError::PathTraversalBlocked(_)));
    assert!(!td.path().join("escape.txt").exists());
}

// ---------------------------------------------------------------------------
// 7z
// ---------------------------------------------------------------------------

fn build_7z_fixture(out: &std::path::Path) {
    // 2026-05-15: migrated to `sevenz-rust2` (active fork; the old
    // `sevenz-rust = 0.6` is unmaintained). 0.16 renamed
    // `SevenZWriter` → `ArchiveWriter` and `SevenZArchiveEntry` →
    // `ArchiveEntry`. We alias them locally so the rest of the fixture
    // body stays unchanged.
    use sevenz_rust2::{ArchiveEntry as SevenZArchiveEntry, ArchiveWriter as SevenZWriter};
    let mut writer = SevenZWriter::create(out).expect("create 7z fixture");

    let entry_a = SevenZArchiveEntry::from_path(
        std::path::Path::new("entry_a.txt"),
        "entry_a.txt".to_owned(),
    );
    writer
        .push_archive_entry::<&[u8]>(entry_a, Some(b"hello 7z\n"))
        .unwrap();

    let entry_b = SevenZArchiveEntry::from_path(
        std::path::Path::new("dir/entry_b.bin"),
        "dir/entry_b.bin".to_owned(),
    );
    writer
        .push_archive_entry::<&[u8]>(entry_b, Some(&[0u8, 1, 2, 3, 4, 5, 6, 7]))
        .unwrap();

    writer.finish().unwrap();
}

#[test]
fn sevenz_format_detected() {
    let td = tempdir().unwrap();
    let path = td.path().join("fixture.7z");
    build_7z_fixture(&path);
    let fmt = otterzip_core::detect(&path).unwrap();
    assert_eq!(fmt, ArchiveFormat::SevenZ);
}

#[test]
fn sevenz_extract_roundtrip() {
    let td = tempdir().unwrap();
    let archive_path = td.path().join("fixture.7z");
    build_7z_fixture(&archive_path);
    let out = td.path().join("out");

    let opts = ExtractOptions {
        destination: out.clone(),
        overwrite: OverwritePolicy::Always,
        ..Default::default()
    };
    let archive = Archive::open(&archive_path, OpenMode::Read).unwrap();
    let report = archive
        .extract_all::<fn(&otterzip_core::Progress) -> bool>(&opts, None)
        .unwrap();
    assert!(report.entries_extracted >= 2);
    assert_eq!(fs::read(out.join("entry_a.txt")).unwrap(), b"hello 7z\n");
    assert_eq!(
        fs::read(out.join("dir").join("entry_b.bin")).unwrap(),
        [0u8, 1, 2, 3, 4, 5, 6, 7]
    );
}

// ---------------------------------------------------------------------------
// Password-protected ZIP
// ---------------------------------------------------------------------------

fn build_encrypted_zip(out: &std::path::Path, password: &str) {
    use zip::write::SimpleFileOptions;
    let file = fs::File::create(out).expect("create encrypted zip");
    let mut writer = zip::ZipWriter::new(file);
    let opts = SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .with_aes_encryption(zip::AesMode::Aes256, password);
    writer.start_file("secret.txt", opts).unwrap();
    writer.write_all(b"top secret content\n").unwrap();
    writer.finish().unwrap();
}

#[test]
fn encrypted_zip_requires_password() {
    let td = tempdir().unwrap();
    let path = td.path().join("locked.zip");
    build_encrypted_zip(&path, "hunter2");

    let archive = Archive::open(&path, OpenMode::Read).unwrap();
    let opts = ExtractOptions {
        destination: td.path().join("out_no_pw"),
        overwrite: OverwritePolicy::Always,
        ..Default::default()
    };
    let err = archive
        .extract_all::<fn(&otterzip_core::Progress) -> bool>(&opts, None)
        .unwrap_err();
    assert!(matches!(err, OtterzipError::WrongPassword), "got {err:?}");
}

#[test]
fn encrypted_zip_unlocks_with_correct_password() {
    let td = tempdir().unwrap();
    let path = td.path().join("locked.zip");
    build_encrypted_zip(&path, "hunter2");

    let archive = Archive::open_with_password(&path, OpenMode::Read, "hunter2".to_owned())
        .expect("open_with_password");
    let out = td.path().join("out");
    let opts = ExtractOptions {
        destination: out.clone(),
        overwrite: OverwritePolicy::Always,
        ..Default::default()
    };
    let report = archive
        .extract_all::<fn(&otterzip_core::Progress) -> bool>(&opts, None)
        .unwrap();
    assert_eq!(report.entries_extracted, 1);
    assert_eq!(
        fs::read(out.join("secret.txt")).unwrap(),
        b"top secret content\n"
    );
}

// ---------------------------------------------------------------------------
// ZIP-bomb detection
// ---------------------------------------------------------------------------

fn build_bomb_zip(out: &std::path::Path) {
    use zip::write::SimpleFileOptions;
    let file = fs::File::create(out).expect("create bomb zip");
    let mut writer = zip::ZipWriter::new(file);
    let opts = SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .large_file(true);
    writer.start_file("bomb.bin", opts).unwrap();
    // ~10 MB of zeros — DEFLATE compresses to ~10 KB so ratio ~1000:1.
    let zeros = vec![0u8; 10 * 1024 * 1024];
    writer.write_all(&zeros).unwrap();
    writer.finish().unwrap();
}

#[test]
fn oversized_high_ratio_payload_is_blocked_by_the_absolute_cap() {
    // `build_bomb_zip` is 10 MB of zeros — high ratio (~1000:1) but only 10 MB
    // of output, i.e. NOT a disk threat. It is indistinguishable by ratio from
    // an ordinary zero-filled VM disk, so the ratio gate no longer aborts on it
    // (that was the false positive that refused benign files, see
    // tests/zip_bomb_false_positive.rs). The size-based absolute cap is what
    // stops a payload from actually exhausting the disk — prove it fires here.
    let td = tempdir().unwrap();
    let path = td.path().join("bomb.zip");
    build_bomb_zip(&path);
    let archive = Archive::open(&path, OpenMode::Read).unwrap();

    // Ratio-only, no byte cap: the 10 MB total is below the floor, so it
    // extracts — the relaxation.
    let ratio_only = ExtractOptions {
        destination: td.path().join("out_ratio"),
        overwrite: OverwritePolicy::Always,
        max_compression_ratio: 100,
        max_total_output_bytes: 0,
        ..Default::default()
    };
    assert!(
        archive
            .extract_all::<fn(&otterzip_core::Progress) -> bool>(&ratio_only, None)
            .is_ok(),
        "10 MB of zeros is not a disk threat and must extract"
    );

    // Absolute cap below the payload: refused, regardless of ratio.
    let archive2 = Archive::open(&path, OpenMode::Read).unwrap();
    let capped = ExtractOptions {
        destination: td.path().join("out_cap"),
        overwrite: OverwritePolicy::Always,
        max_compression_ratio: 0,
        max_total_compression_ratio: 0,
        max_total_output_bytes: 1024 * 1024, // 1 MiB < 10 MB payload
        ..Default::default()
    };
    let err = archive2
        .extract_all::<fn(&otterzip_core::Progress) -> bool>(&capped, None)
        .unwrap_err();
    assert!(
        matches!(err, OtterzipError::ZipBombSuspected { .. }),
        "the absolute cap must stop an over-cap payload, got {err:?}"
    );
}

#[test]
fn zip_bomb_check_disabled_when_threshold_zero() {
    let td = tempdir().unwrap();
    let path = td.path().join("bomb.zip");
    build_bomb_zip(&path);

    let archive = Archive::open(&path, OpenMode::Read).unwrap();
    let opts = ExtractOptions {
        destination: td.path().join("out"),
        overwrite: OverwritePolicy::Always,
        max_compression_ratio: 0,
        // Phase 7: cumulative gate is also defaulted ON. Disable both
        // layers + lift the absolute byte cap to truly opt the test out
        // of bomb defence.
        max_total_compression_ratio: 0,
        max_total_output_bytes: 0,
        ..Default::default()
    };
    let report = archive
        .extract_all::<fn(&otterzip_core::Progress) -> bool>(&opts, None)
        .unwrap();
    assert_eq!(report.entries_extracted, 1);
}
