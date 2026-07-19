//! PROBE 4 — claim: `ExtractOptions::entry_filter` is parsed by the FFI and
//! then silently ignored by the core extract path.

use std::fs;
use std::io::Write;
use std::path::Path;

use otterzip_core::options::{ExtractOptions, OverwritePolicy};
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

fn listing(dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(cur) = stack.pop() {
        for e in fs::read_dir(&cur).unwrap() {
            let e = e.unwrap();
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else {
                out.push(
                    p.strip_prefix(dir)
                        .unwrap()
                        .to_string_lossy()
                        .replace('\\', "/"),
                );
            }
        }
    }
    out.sort();
    out
}

#[test]

#[ignore = "known-failure probe: reproduces an unfixed defect. Run with `cargo test -- --ignored`; delete this attribute when fixed."]
fn entry_filter_limits_extraction_to_the_listed_entries() {
    let td = tempdir().unwrap();
    let zip_path = td.path().join("many.zip");
    build_zip(
        &zip_path,
        &[
            ("wanted.txt", b"yes\n"),
            ("unwanted.txt", b"no\n"),
            ("also/unwanted.bin", &[0xAA, 0xBB]),
        ],
    );

    let dest = td.path().join("out");
    fs::create_dir_all(&dest).unwrap();
    let opts = ExtractOptions {
        destination: dest.clone(),
        overwrite: OverwritePolicy::Always,
        // Caller asks for exactly one entry.
        entry_filter: Some(vec!["wanted.txt".to_string()]),
        ..Default::default()
    };

    let a = Archive::open(&zip_path, OpenMode::Read).unwrap();
    let report = a
        .extract_all::<fn(&otterzip_core::Progress) -> bool>(&opts, None)
        .unwrap();

    let got = listing(&dest);
    println!("entry_filter = [\"wanted.txt\"]");
    println!(
        "report: entries_extracted={} entries_skipped={} bytes={}",
        report.entries_extracted, report.entries_skipped, report.bytes_written
    );
    println!("files on disk after extract: {got:?}");

    assert_eq!(
        got,
        vec!["wanted.txt".to_string()],
        "entry_filter was ignored — everything got extracted"
    );
}
