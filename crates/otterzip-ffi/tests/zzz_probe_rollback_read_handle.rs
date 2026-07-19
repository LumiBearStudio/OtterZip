//! PROBE: does `otterzip_archive_rollback()` accept a handle that came
//! from `otterzip_archive_open(..., mode=Read, ...)`, delete the user's
//! archive, and return OK?
//!
//! At the C ABI both open and create hand back the same opaque
//! `*mut OtterzipArchive`, so a caller (or a C# `SafeHandle` wrapper
//! holding a read handle) has no type-level way to be stopped. The
//! core `Archive::rollback` has no `mode` guard, unlike `commit`.

use std::ffi::CString;
use std::fs;
use std::io::Write;
use std::ptr;

use otterzip_ffi::{
    otterzip_archive_commit, otterzip_archive_open, otterzip_archive_rollback, OtterzipArchive,
};

const MODE_READ: u32 = 0;

fn build_zip(out: &std::path::Path) {
    let f = fs::File::create(out).unwrap();
    let mut w = zip::ZipWriter::new(f);
    let opts = zip::write::SimpleFileOptions::default();
    w.start_file("taxes-2025.pdf", opts).unwrap();
    w.write_all(b"the only copy\n").unwrap();
    w.finish().unwrap();
}

fn open_read(path: &str) -> *mut OtterzipArchive {
    let c = CString::new(path).unwrap();
    let mut handle: *mut OtterzipArchive = ptr::null_mut();
    let rc = otterzip_archive_open(
        c.as_ptr(),
        path.len(),
        MODE_READ,
        ptr::null(),
        0,
        &raw mut handle,
    );
    assert_eq!(rc, 0, "otterzip_archive_open should succeed");
    assert!(!handle.is_null());
    handle
}

#[test]

#[ignore = "known-failure probe: reproduces an unfixed defect. Run with `cargo test -- --ignored`; delete this attribute when fixed."]
fn ffi_rollback_accepts_a_read_handle_and_destroys_the_archive() {
    let td = tempfile::tempdir().unwrap();
    let archive = td.path().join("taxes.zip");
    build_zip(&archive);
    let path = archive.to_str().unwrap().to_string();

    println!(
        "archive before : exists={} size={}",
        archive.exists(),
        fs::metadata(&archive).unwrap().len()
    );

    // Control: commit() on the very same kind of handle is refused.
    let h1 = open_read(&path);
    let commit_rc = otterzip_archive_commit(h1);
    println!("otterzip_archive_commit(read handle)   -> rc={commit_rc}");
    println!("  archive still exists: {}", archive.exists());

    // The call under test, on an identically-obtained read handle.
    let h2 = open_read(&path);
    let rollback_rc = otterzip_archive_rollback(h2);
    let exists_after = archive.exists();
    println!("otterzip_archive_rollback(read handle) -> rc={rollback_rc}");
    println!("  archive still exists: {exists_after}");

    println!("\n=====================================================");
    println!("commit   rc = {commit_rc:>3}  (non-zero = rejected, file kept)");
    println!("rollback rc = {rollback_rc:>3}  (0 = OK)   file exists = {exists_after}");
    println!("=====================================================");

    assert_ne!(commit_rc, 0, "commit is guarded at the FFI layer");

    // REGRESSION ASSERT (desired behaviour): rollback must refuse a
    // read-mode handle exactly as commit does, and must never unlink a
    // file it did not create.
    assert!(
        exists_after,
        "otterzip_archive_rollback DELETED a file opened read-only, and returned \
         rc={rollback_rc} (OK). commit() on the same handle returns {commit_rc}."
    );
    assert_ne!(
        rollback_rc, 0,
        "rollback should reject a read-mode handle like commit does"
    );
}
