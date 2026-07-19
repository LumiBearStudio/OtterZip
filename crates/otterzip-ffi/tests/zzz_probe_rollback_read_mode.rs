//! PROBE 2b — reachability through the C ABI: `otterzip_archive_rollback`
//! accepts a handle produced by `otterzip_archive_open` (READ mode) and
//! deletes the user's source archive, returning OTTERZIP_OK.

use std::fs;
use std::io::Write;
use std::ptr;

use otterzip_ffi::{
    otterzip_archive_open, otterzip_archive_rollback, OtterzipArchive,
};

const OTTERZIP_OK: i32 = 0;

fn build_fixture_zip(out: &std::path::Path) {
    let file = fs::File::create(out).unwrap();
    let mut w = zip::ZipWriter::new(file);
    let opts = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    w.start_file("readme.txt", opts).unwrap();
    w.write_all(b"irreplaceable\n").unwrap();
    w.finish().unwrap();
}

#[test]

#[ignore = "known-failure probe: reproduces an unfixed defect. Run with `cargo test -- --ignored`; delete this attribute when fixed."]
fn ffi_rollback_on_a_read_handle_deletes_the_source() {
    let td = tempfile::tempdir().unwrap();
    let zip_path = td.path().join("precious.zip");
    build_fixture_zip(&zip_path);
    println!("before: exists={}", zip_path.exists());

    let s = zip_path.to_str().unwrap();
    let mut handle: *mut OtterzipArchive = ptr::null_mut();
    let rc = otterzip_archive_open(
        s.as_ptr().cast(),
        s.len(),
        0, // OpenMode::Read
        ptr::null(),
        0,
        &mut handle,
    );
    assert_eq!(rc, OTTERZIP_OK, "open failed");
    assert!(!handle.is_null());

    // A C consumer holding an opaque OtterzipArchive* — same type as the
    // create-mode handle — calls rollback on it.
    let rc = otterzip_archive_rollback(handle);
    println!("otterzip_archive_rollback(read handle) -> rc={rc}");
    println!("after:  exists={}", zip_path.exists());

    assert!(
        zip_path.exists(),
        "FFI rollback deleted a read-mode source archive (rc={rc})"
    );
}
