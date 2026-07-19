//! PROBE 2 — claim: `rollback()` on a READ-mode `Archive` deletes the
//! source archive off disk (no mode guard, unlike `commit()`).

use std::fs;
use std::io::Write;
use std::path::Path;

use otterzip_core::{Archive, OpenMode};
use tempfile::tempdir;

fn build_zip(out: &Path, entries: &[(&str, &[u8])]) {
    let f = fs::File::create(out).unwrap();
    let mut w = zip::ZipWriter::new(f);
    let opts =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    for (name, body) in entries {
        w.start_file(*name, opts).unwrap();
        w.write_all(body).unwrap();
    }
    w.finish().unwrap();
}

#[test]

#[ignore = "known-failure probe: reproduces an unfixed defect. Run with `cargo test -- --ignored`; delete this attribute when fixed."]
fn rollback_on_read_mode_archive_must_not_delete_the_source() {
    let td = tempdir().unwrap();
    let src = td.path().join("precious.zip");
    build_zip(&src, &[("a.txt", b"irreplaceable\n")]);
    let size_before = fs::metadata(&src).unwrap().len();
    println!("before: exists={} size={}", src.exists(), size_before);

    let archive = Archive::open(&src, OpenMode::Read).unwrap();

    // For contrast: commit() on a read-mode handle is explicitly rejected.
    // rollback() is the asymmetric sibling.
    let rc = archive.rollback();
    println!("rollback() on OpenMode::Read handle -> {rc:?}");
    println!("after:  exists={}", src.exists());

    assert!(
        src.exists(),
        "rollback() on a read-mode Archive DELETED the user's source archive"
    );
}

#[test]

#[ignore = "known-failure probe: reproduces an unfixed defect. Run with `cargo test -- --ignored`; delete this attribute when fixed."]
fn commit_on_read_mode_archive_is_rejected_for_contrast() {
    let td = tempdir().unwrap();
    let src = td.path().join("precious2.zip");
    build_zip(&src, &[("a.txt", b"x\n")]);
    let archive = Archive::open(&src, OpenMode::Read).unwrap();
    let rc = archive.commit();
    println!("commit() on OpenMode::Read handle -> {rc:?}");
    assert!(rc.is_err(), "commit is guarded");
    assert!(src.exists(), "commit did not touch the file");
}
