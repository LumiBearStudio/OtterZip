//! PROBE — claim 1: tar hard-link entries silently extract as 0-byte files
//! and are counted as successes.
//!
//! A GNU/BSD tar records the *second and subsequent* occurrences of a file
//! that shares an inode as `EntryType::Link` ('1') with size 0 and a
//! `linkname` pointing at the first occurrence. Every Linux rootfs tarball,
//! every `tar` of a tree with hardlinks (node_modules, .git alternates,
//! busybox applets) carries these.

use std::fs;
use std::io;

use otterzip_core::{Archive, ExtractOptions, OpenMode, OverwritePolicy, ProgressSink};
use tempfile::tempdir;

struct NullSink;
impl ProgressSink for NullSink {
    fn update(&mut self, _: &otterzip_core::Progress) -> bool {
        true
    }
}

#[test]

#[ignore = "known-failure probe: reproduces an unfixed defect. Run with `cargo test -- --ignored`; delete this attribute when fixed."]
fn probe_tar_hardlink_entry() {
    let dir = tempdir().unwrap();
    let tar_path = dir.path().join("hardlink.tar");

    // Build: real.txt (12 bytes) + link.txt (hard link -> real.txt).
    {
        let file = fs::File::create(&tar_path).unwrap();
        let mut b = tar::Builder::new(file);

        let payload = b"REAL CONTENT";
        let mut h = tar::Header::new_gnu();
        h.set_path("real.txt").unwrap();
        h.set_size(payload.len() as u64);
        h.set_mode(0o644);
        h.set_entry_type(tar::EntryType::Regular);
        h.set_cksum();
        b.append(&h, &payload[..]).unwrap();

        let mut h2 = tar::Header::new_gnu();
        h2.set_path("link.txt").unwrap();
        h2.set_link_name("real.txt").unwrap();
        h2.set_size(0);
        h2.set_mode(0o644);
        h2.set_entry_type(tar::EntryType::Link);
        h2.set_cksum();
        b.append(&h2, io::empty()).unwrap();

        b.finish().unwrap();
    }

    let dest = dir.path().join("out");
    fs::create_dir_all(&dest).unwrap();

    let archive = Archive::open(&tar_path, OpenMode::Read).unwrap();

    println!("--- entries() ---");
    for e in archive.entries().unwrap() {
        let e = e.unwrap();
        println!(
            "  path={:?} size={} dir={} symlink={}",
            e.path, e.uncompressed_size, e.is_directory, e.is_symlink
        );
    }

    let opts = ExtractOptions {
        destination: dest.clone(),
        overwrite: OverwritePolicy::Always,
        ..Default::default()
    };
    let mut sink = NullSink;
    let report = archive.extract_all(&opts, Some(&mut sink)).unwrap();

    println!("--- report ---");
    println!("  entries_extracted = {}", report.entries_extracted);
    println!("  entries_skipped   = {}", report.entries_skipped);
    println!("  bytes_written     = {}", report.bytes_written);
    println!("  warnings          = {:?}", report.warnings);

    let real = dest.join("real.txt");
    let link = dest.join("link.txt");
    println!("--- filesystem ---");
    println!(
        "  real.txt exists={} len={:?}",
        real.exists(),
        fs::metadata(&real).map(|m| m.len()).ok()
    );
    println!(
        "  link.txt exists={} len={:?} content={:?}",
        link.exists(),
        fs::metadata(&link).map(|m| m.len()).ok(),
        fs::read(&link).ok()
    );

    // What a user expects: link.txt has the same content as real.txt.
    assert!(link.exists(), "link.txt was not created at all");
    assert_eq!(
        fs::read(&link).unwrap(),
        b"REAL CONTENT",
        "REPRODUCED: hard-link entry extracted with the wrong content \
         (should mirror its link target)"
    );
}
