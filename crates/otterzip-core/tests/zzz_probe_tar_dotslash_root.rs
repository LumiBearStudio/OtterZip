//! PROBE (out-of-batch, found while probing claim 5) — `tar -cf x.tar -C dir .`
//! emits a leading `./` directory member. That member resolves to dest_root
//! itself, which the d25b3e4 hardening now treats as path traversal, so with
//! the DEFAULT options the whole extraction aborts and nothing lands.
//!
//! Ground truth (GNU tar 1.35 on this machine):
//! ```text
//! $ tar -cf big.tar -C src .
//! $ tar -tf big.tar | head -1
//! ./
//! ```
//! Every `tar -C <dir> .` tarball in existence starts this way.

use std::fs;

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
fn probe_tar_dot_slash_root_member() {
    let dir = tempdir().unwrap();
    let tar_path = dir.path().join("dotslash.tar");

    {
        let f = fs::File::create(&tar_path).unwrap();
        let mut b = tar::Builder::new(f);

        // "./" directory member, exactly as GNU tar writes it.
        let mut h = tar::Header::new_gnu();
        h.set_path("./").unwrap();
        h.set_size(0);
        h.set_mode(0o755);
        h.set_entry_type(tar::EntryType::Directory);
        h.set_cksum();
        b.append(&h, std::io::empty()).unwrap();

        for (name, data) in [("./a.txt", "AAAA"), ("./b.txt", "BBBB")] {
            let mut h = tar::Header::new_gnu();
            h.set_path(name).unwrap();
            h.set_size(data.len() as u64);
            h.set_mode(0o644);
            h.set_cksum();
            b.append(&h, data.as_bytes()).unwrap();
        }
        b.finish().unwrap();
    }

    let dest = dir.path().join("out");
    fs::create_dir_all(&dest).unwrap();

    let archive = Archive::open(&tar_path, OpenMode::Read).unwrap();
    let opts = ExtractOptions {
        destination: dest.clone(),
        overwrite: OverwritePolicy::Always,
        ..Default::default() // block_path_traversal: true is the default
    };
    let mut sink = NullSink;
    let result = archive.extract_all(&opts, Some(&mut sink));
    match &result {
        Ok(r) => println!(
            "extract_all -> Ok(extracted={}, skipped={})",
            r.entries_extracted, r.entries_skipped
        ),
        Err(e) => println!("extract_all -> Err({e})"),
    }

    let mut on_disk: Vec<String> = fs::read_dir(&dest)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    on_disk.sort();
    println!("on disk -> {:?}", on_disk);

    assert!(
        result.is_ok(),
        "REGRESSION: a normal `tar -C dir .` tarball is rejected outright — \
         the leading `./` member is read as a path-traversal attempt"
    );
    assert_eq!(on_disk, vec!["a.txt".to_string(), "b.txt".to_string()]);
}
