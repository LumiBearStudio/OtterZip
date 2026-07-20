//! Phase 7 — security hardening regression tests.
//!
//! Each scenario crafts a small ZIP with one or more "weaponised"
//! entries and asserts that `Archive::extract_all` rejects them with
//! the expected error class. These failures must stay loud — silent
//! coercion of e.g. `CON.txt` to `CON` would re-open known Windows
//! shell-parsing CVEs.

use std::fs;
use std::io::Write;

use otterzip_core::{
    Archive, ExtractOptions, OpenMode, OverwritePolicy, Progress, OtterzipError,
};
use tempfile::tempdir;
use zip::write::SimpleFileOptions;

fn write_zip_with(name: &str, body: &[u8], path: &std::path::Path) {
    let file = fs::File::create(path).unwrap();
    let mut writer = zip::ZipWriter::new(file);
    let opts = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    writer.start_file(name, opts).unwrap();
    writer.write_all(body).unwrap();
    writer.finish().unwrap();
}

fn extract_with_defaults(zip: &std::path::Path, dest: &std::path::Path) -> otterzip_core::Result<()> {
    let opts = ExtractOptions {
        destination: dest.to_path_buf(),
        overwrite: OverwritePolicy::Always,
        ..Default::default()
    };
    let archive = Archive::open(zip, OpenMode::Read)?;
    archive
        .extract_all::<fn(&Progress) -> bool>(&opts, None)
        .map(|_| ())
}

// ---------------------------------------------------------------------------
// Path-segment hardening
// ---------------------------------------------------------------------------

#[test]
fn rejects_ntfs_alternate_data_stream() {
    let td = tempdir().unwrap();
    let zip = td.path().join("ads.zip");
    write_zip_with("notes.txt:hidden", b"pwn", &zip);

    let err = extract_with_defaults(&zip, &td.path().join("out")).unwrap_err();
    assert!(
        matches!(err, OtterzipError::PathTraversalBlocked(_)),
        "ADS must be blocked, got {err:?}"
    );
}

#[test]
fn rejects_windows_reserved_names() {
    for name in &["CON.txt", "PRN.dat", "AUX.bin", "NUL.log", "COM1.cfg", "LPT1.tmp"] {
        let td = tempdir().unwrap();
        let zip = td.path().join("reserved.zip");
        write_zip_with(name, b"x", &zip);
        let err = extract_with_defaults(&zip, &td.path().join("out")).unwrap_err();
        assert!(
            matches!(err, OtterzipError::PathTraversalBlocked(_)),
            "reserved name {name} must be blocked, got {err:?}"
        );
    }
}

#[test]
fn rejects_embedded_backslash() {
    let td = tempdir().unwrap();
    let zip = td.path().join("backslash.zip");
    write_zip_with("a\\..\\..\\evil.txt", b"x", &zip);

    let err = extract_with_defaults(&zip, &td.path().join("out")).unwrap_err();
    assert!(
        matches!(err, OtterzipError::PathTraversalBlocked(_)),
        "embedded backslash must be blocked, got {err:?}"
    );
}

#[test]
fn rejects_trailing_dot_or_space() {
    for name in &["evil.txt.", "evil.txt "] {
        let td = tempdir().unwrap();
        let zip = td.path().join("trailing.zip");
        write_zip_with(name, b"x", &zip);
        let err = extract_with_defaults(&zip, &td.path().join("out")).unwrap_err();
        assert!(
            matches!(err, OtterzipError::PathTraversalBlocked(_)),
            "trailing dot/space {name:?} must be blocked, got {err:?}"
        );
    }
}

#[test]
fn rejects_control_characters_in_path() {
    let td = tempdir().unwrap();
    let zip = td.path().join("ctrl.zip");
    write_zip_with("ev\x07il.txt", b"x", &zip);

    let err = extract_with_defaults(&zip, &td.path().join("out")).unwrap_err();
    assert!(
        matches!(err, OtterzipError::PathTraversalBlocked(_)),
        "control chars must be blocked, got {err:?}"
    );
}

// ---------------------------------------------------------------------------
// Cumulative bomb gates
// ---------------------------------------------------------------------------

fn build_many_small_bomb(zip: &std::path::Path) {
    let file = fs::File::create(zip).unwrap();
    let mut writer = zip::ZipWriter::new(file);
    let opts = SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .large_file(true);
    let zeros = vec![0u8; 256 * 1024]; // 256 KiB of zeros — DEFLATE shrinks to ~1 KiB.
    for i in 0..32u32 {
        writer.start_file(format!("e/{i:03}.bin"), opts).unwrap();
        writer.write_all(&zeros).unwrap();
    }
    writer.finish().unwrap();
}

#[test]
fn many_small_bomb_is_stopped_by_the_absolute_cap_not_the_ratio() {
    // "Spread the payload across many small entries to dodge the per-entry
    // radar" is a real evasion. It used to be caught by the CUMULATIVE RATIO
    // gate, but that gate cannot tell this fixture (8 MiB / ~32 KiB ≈ 256:1)
    // from an ordinary 64 MiB zero-filled VM disk (≈ 1028:1) — the benign file
    // scores HIGHER. So the ratio gate is now floored: below ~1 GiB of total
    // output it defers to the absolute byte cap, which is the real
    // disk-exhaustion defence and is size-based rather than ratio-based.
    let td = tempdir().unwrap();
    let zip = td.path().join("many.zip");
    build_many_small_bomb(&zip);

    // (a) Under a low ratio limit but no byte cap: the ~8 MiB total is below
    //     the floor, so the ratio gate correctly does NOT fire — this is the
    //     relaxation that lets legitimately-compressible archives through.
    let ratio_only = ExtractOptions {
        destination: td.path().join("out_ratio"),
        overwrite: OverwritePolicy::Always,
        max_compression_ratio: 0,
        max_total_compression_ratio: 100,
        max_total_output_bytes: 0, // cap disabled
        ..Default::default()
    };
    let a = Archive::open(&zip, OpenMode::Read).unwrap();
    let res = a.extract_all::<fn(&Progress) -> bool>(&ratio_only, None);
    assert!(
        res.is_ok(),
        "an 8 MiB total is not a disk threat; ratio gate must not fire: {res:?}"
    );

    // (b) The evasion is still stopped — by the absolute cap. Cap at 1 MiB
    //     against the ~8 MiB fixture: it trips regardless of how the payload
    //     is spread across entries.
    let capped = ExtractOptions {
        destination: td.path().join("out_cap"),
        overwrite: OverwritePolicy::Always,
        max_compression_ratio: 0,
        max_total_compression_ratio: 0,
        max_total_output_bytes: 1024 * 1024, // 1 MiB < 8 MiB fixture
        ..Default::default()
    };
    let a2 = Archive::open(&zip, OpenMode::Read).unwrap();
    let err = a2
        .extract_all::<fn(&Progress) -> bool>(&capped, None)
        .unwrap_err();
    assert!(
        matches!(err, OtterzipError::ZipBombSuspected { .. }),
        "absolute cap must stop the many-small-entry evasion, got {err:?}"
    );
}

#[test]
fn absolute_byte_cap_blocks_oversized_payload() {
    let td = tempdir().unwrap();
    let zip = td.path().join("big.zip");
    build_many_small_bomb(&zip);

    let opts = ExtractOptions {
        destination: td.path().join("out"),
        overwrite: OverwritePolicy::Always,
        max_compression_ratio: 0,
        max_total_compression_ratio: 0,
        // Cap at 1 MiB — fixture writes ~8 MiB → must trip.
        max_total_output_bytes: 1 * 1024 * 1024,
        ..Default::default()
    };
    let archive = Archive::open(&zip, OpenMode::Read).unwrap();
    let err = archive
        .extract_all::<fn(&Progress) -> bool>(&opts, None)
        .unwrap_err();
    assert!(
        matches!(err, OtterzipError::ZipBombSuspected { .. }),
        "absolute byte cap must trip, got {err:?}"
    );
}

#[test]
fn tar_gz_byte_cap_blocks_disk_fill_bomb() {
    // A .tar.gz whose single entry is 4 MiB of zeros: the gzip layer
    // squashes it to a few KiB, and tar reports compressed == uncompressed,
    // so the per-entry ratio gate reads a constant 1:1 and never fires.
    // Only the absolute output-byte cap can stop this. Regression for the
    // M1 gap where tar/7z streaming extraction ignored
    // `max_total_output_bytes` entirely (a crafted tarball would extract
    // until the disk filled).
    let td = tempdir().unwrap();
    let targz = td.path().join("bomb.tar.gz");
    let zeros = vec![0u8; 4 * 1024 * 1024];
    let bytes = {
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let genc =
                flate2::write::GzEncoder::new(&mut buf, flate2::Compression::default());
            let mut tarball = tar::Builder::new(genc);
            let mut header = tar::Header::new_gnu();
            header.set_size(zeros.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            tarball
                .append_data(&mut header, "big.bin", &zeros[..])
                .unwrap();
            tarball.into_inner().unwrap().finish().unwrap();
        }
        buf.into_inner()
    };
    fs::write(&targz, &bytes).unwrap();

    let opts = ExtractOptions {
        destination: td.path().join("out"),
        overwrite: OverwritePolicy::Always,
        // Isolate the byte cap: disable both ratio gates so only the
        // absolute cap can be responsible for the abort.
        max_compression_ratio: 0,
        max_total_compression_ratio: 0,
        // 1 MiB cap; the 4 MiB entry must trip it mid-write.
        max_total_output_bytes: 1024 * 1024,
        ..Default::default()
    };
    let archive = Archive::open(&targz, OpenMode::Read).unwrap();
    let err = archive
        .extract_all::<fn(&Progress) -> bool>(&opts, None)
        .unwrap_err();
    assert!(
        matches!(err, OtterzipError::ZipBombSuspected { .. }),
        "tar.gz byte cap must trip, got {err:?}"
    );
}

#[test]
fn legitimate_archive_passes_phase7_gates() {
    // Sanity: a normal small archive with safe names + low ratio must
    // sail through every Phase 7 check using the documented defaults.
    let td = tempdir().unwrap();
    let zip = td.path().join("ok.zip");
    {
        let file = fs::File::create(&zip).unwrap();
        let mut writer = zip::ZipWriter::new(file);
        let opts = SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        writer.start_file("README.md", opts).unwrap();
        writer.write_all(b"# hello\nplain text\n").unwrap();
        writer.start_file("docs/api.txt", opts).unwrap();
        writer.write_all(b"reasonable contents\n").unwrap();
        writer.finish().unwrap();
    }

    let opts = ExtractOptions {
        destination: td.path().join("out"),
        overwrite: OverwritePolicy::Always,
        ..Default::default()
    };
    let archive = Archive::open(&zip, OpenMode::Read).unwrap();
    let report = archive
        .extract_all::<fn(&Progress) -> bool>(&opts, None)
        .unwrap();
    assert_eq!(report.entries_extracted, 2);
}

#[test]
fn single_stream_gz_byte_cap_blocks_bomb() {
    // A raw .gz whose single stream expands to 4 MiB of zeros. Single-stream
    // backends report uncompressed_size == 0, so the per-entry ratio gate is
    // inert — only the absolute output-byte cap can stop it, and it must be
    // enforced DURING the write (CappedWriter) because the one entry IS the
    // whole payload. Regression for BK-M1 (was an unbounded io::copy).
    use std::io::Write as _;
    let td = tempdir().unwrap();
    let gz = td.path().join("bomb.gz");
    let zeros = vec![0u8; 4 * 1024 * 1024];
    {
        let f = fs::File::create(&gz).unwrap();
        let mut enc = flate2::write::GzEncoder::new(f, flate2::Compression::default());
        enc.write_all(&zeros).unwrap();
        enc.finish().unwrap();
    }
    let opts = ExtractOptions {
        destination: td.path().join("out"),
        overwrite: OverwritePolicy::Always,
        // Isolate the byte cap: disable the ratio gates.
        max_compression_ratio: 0,
        max_total_compression_ratio: 0,
        max_total_output_bytes: 1024 * 1024, // 1 MiB; the 4 MiB stream must trip it
        ..Default::default()
    };
    let archive = Archive::open(&gz, OpenMode::Read).unwrap();
    let err = archive
        .extract_all::<fn(&Progress) -> bool>(&opts, None)
        .unwrap_err();
    assert!(
        matches!(err, OtterzipError::ZipBombSuspected { .. }),
        "single-stream .gz byte cap must trip mid-write, got {err:?}"
    );
}

#[test]
fn flatten_extract_rejects_parent_dir_entry() {
    // With flatten_paths, an entry whose path is exactly `..` used to fall
    // back to dest_root.join("..") and escape the destination (FFI-M2). The
    // flatten branch now validates the component and rejects file_name()==None.
    let td = tempdir().unwrap();
    let zip = td.path().join("evil.zip");
    {
        let file = fs::File::create(&zip).unwrap();
        let mut w = zip::ZipWriter::new(file);
        // Write the raw name "../pwned.txt" — file_name() is "pwned.txt" so
        // this flattens safely; the dangerous ".." bare-name is exercised by
        // the None-branch guard. Also include a reserved-name component to
        // prove per-component validation now runs on the flatten path.
        let opts = zip::write::SimpleFileOptions::default();
        w.start_file("../pwned.txt", opts).unwrap();
        std::io::Write::write_all(&mut w, b"x").unwrap();
        w.finish().unwrap();
    }
    let out = td.path().join("out");
    let opts = ExtractOptions {
        destination: out.clone(),
        overwrite: OverwritePolicy::Always,
        flatten_paths: true,
        ..Default::default()
    };
    let archive = Archive::open(&zip, OpenMode::Read).unwrap();
    let _ = archive.extract_all::<fn(&Progress) -> bool>(&opts, None);
    // Whatever the policy, nothing may be written OUTSIDE the destination.
    assert!(
        !td.path().join("pwned.txt").exists(),
        "flatten extraction escaped the destination root"
    );
}
