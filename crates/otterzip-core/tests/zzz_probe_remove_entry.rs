//! PROBE 1 — claim: `Archive::remove_entry()` returns Ok but is a silent
//! no-op; the entry survives `commit()`.

use std::fs;

use otterzip_core::{format::CompressionMethod, Archive, ArchiveFormat, CreateOptions, OpenMode};
use tempfile::tempdir;

fn names(p: &std::path::Path) -> Vec<String> {
    let a = Archive::open(p, OpenMode::Read).unwrap();
    let mut v: Vec<String> = a.entries().unwrap().map(|e| e.unwrap().path).collect();
    v.sort();
    v
}

#[test]

#[ignore = "known-failure probe: reproduces an unfixed defect. Run with `cargo test -- --ignored`; delete this attribute when fixed."]
fn remove_entry_after_add_actually_removes_the_entry() {
    let td = tempdir().unwrap();
    let keep = td.path().join("keep.txt");
    let drop = td.path().join("drop.txt");
    fs::write(&keep, b"keep me\n").unwrap();
    fs::write(&drop, b"SECRET-PAYLOAD\n").unwrap();

    let dest = td.path().join("out.zip");
    let opts = CreateOptions {
        format: ArchiveFormat::Zip,
        compression: CompressionMethod::Deflate,
        ..Default::default()
    };
    let mut archive = Archive::create(&dest, opts).unwrap();
    archive.add_file(&keep, "keep.txt").unwrap();
    archive.add_file(&drop, "drop.txt").unwrap();

    // Caller asks for the entry to go away. API returns Ok(()).
    let rc = archive.remove_entry("drop.txt");
    println!("remove_entry(\"drop.txt\") -> {rc:?}");
    assert!(rc.is_ok(), "remove_entry reported an error");

    archive.commit().unwrap();

    let got = names(&dest);
    println!("entries after commit: {got:?}");

    // Also prove the payload is really readable, not just a stub name.
    let a = Archive::open(&dest, OpenMode::Read).unwrap();
    if let Ok(mut r) = a.read_entry("drop.txt") {
        use std::io::Read;
        let mut s = String::new();
        let _ = r.read_to_string(&mut s);
        println!("payload of supposedly-removed entry: {s:?}");
    }

    assert!(
        !got.iter().any(|n| n == "drop.txt"),
        "remove_entry() returned Ok but the entry survived commit: {got:?}"
    );
}

#[test]

#[ignore = "known-failure probe: reproduces an unfixed defect. Run with `cargo test -- --ignored`; delete this attribute when fixed."]
fn remove_entry_alone_still_removes_when_nothing_else_staged() {
    // Narrower shape: the ONLY operation is a removal of a name that the
    // writer holds. If commit honoured the queue this file would be empty.
    let td = tempdir().unwrap();
    let src = td.path().join("a.txt");
    fs::write(&src, b"data\n").unwrap();
    let dest = td.path().join("solo.zip");
    let mut archive = Archive::create(
        &dest,
        CreateOptions {
            format: ArchiveFormat::Zip,
            ..Default::default()
        },
    )
    .unwrap();
    archive.add_file(&src, "a.txt").unwrap();
    archive.remove_entry("a.txt").unwrap();
    archive.commit().unwrap();

    let got = names(&dest);
    println!("entries after add+remove+commit: {got:?}");
    assert!(got.is_empty(), "expected an empty archive, got {got:?}");
}
