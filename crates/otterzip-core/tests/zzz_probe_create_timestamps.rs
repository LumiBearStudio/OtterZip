//! PROBE 5 — claim: `CreateOptions::preserve_timestamps` is inert; every ZIP
//! entry is stamped with wall-clock now instead of the source file's mtime.

use std::fs;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use otterzip_core::{format::CompressionMethod, Archive, ArchiveFormat, CreateOptions, OpenMode};
use tempfile::tempdir;

/// 1990-06-15 12:00:00 UTC.
const OLD_UNIX_SECS: u64 = 645_451_200;

fn secs(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH).unwrap().as_secs()
}

fn write_old_file(p: &Path, body: &[u8]) {
    fs::write(p, body).unwrap();
    let old = UNIX_EPOCH + Duration::from_secs(OLD_UNIX_SECS);
    let f = fs::File::options().write(true).open(p).unwrap();
    let times = fs::FileTimes::new().set_accessed(old).set_modified(old);
    f.set_times(times).unwrap();
    drop(f);
    let back = secs(fs::metadata(p).unwrap().modified().unwrap());
    println!("source file on disk has mtime {back} unix secs (wanted {OLD_UNIX_SECS})");
    assert_eq!(back, OLD_UNIX_SECS, "fixture sanity: set_times must stick");
}

fn archived_mtime(zip_path: &Path, name: &str) -> Option<u64> {
    let a = Archive::open(zip_path, OpenMode::Read).unwrap();
    let found = a
        .entries()
        .unwrap()
        .filter_map(std::result::Result::ok)
        .find(|e| e.path == name)
        .and_then(|e| e.modified)
        .map(secs);
    found
}

#[test]

#[ignore = "known-failure probe: reproduces an unfixed defect. Run with `cargo test -- --ignored`; delete this attribute when fixed."]
fn create_stamps_entries_with_the_source_file_mtime() {
    let td = tempdir().unwrap();
    let src = td.path().join("vintage.txt");
    write_old_file(&src, b"written in 1990\n");

    let dest = td.path().join("out.zip");
    let mut archive = Archive::create(
        &dest,
        CreateOptions {
            format: ArchiveFormat::Zip,
            compression: CompressionMethod::Deflate,
            preserve_timestamps: true,
            ..Default::default()
        },
    )
    .unwrap();
    archive.add_file(&src, "vintage.txt").unwrap();
    archive.commit().unwrap();

    let got = archived_mtime(&dest, "vintage.txt").unwrap();
    let now = secs(SystemTime::now());
    println!(
        "preserve_timestamps=true -> entry mtime in ZIP = {got} unix secs \
         (source {OLD_UNIX_SECS}, now {now}, off-by {} s)",
        now.saturating_sub(got)
    );

    assert_eq!(
        got, OLD_UNIX_SECS,
        "preserve_timestamps=true but the ZIP entry was stamped with wall-clock now"
    );
}

#[test]

#[ignore = "known-failure probe: reproduces an unfixed defect. Run with `cargo test -- --ignored`; delete this attribute when fixed."]
fn create_timestamp_flag_changes_behaviour_at_all() {
    let td = tempdir().unwrap();
    let src = td.path().join("vintage.txt");
    write_old_file(&src, b"written in 1990\n");

    let mut stamps = Vec::new();
    for keep in [true, false] {
        let dest = td.path().join(format!("out-{keep}.zip"));
        let mut archive = Archive::create(
            &dest,
            CreateOptions {
                format: ArchiveFormat::Zip,
                preserve_timestamps: keep,
                ..Default::default()
            },
        )
        .unwrap();
        archive.add_file(&src, "vintage.txt").unwrap();
        archive.commit().unwrap();
        let m = archived_mtime(&dest, "vintage.txt").unwrap();
        println!("preserve_timestamps={keep} -> entry mtime {m}");
        stamps.push(m);
    }
    assert_ne!(
        stamps[0], stamps[1],
        "preserve_timestamps true/false produced the same entry stamp — flag is inert"
    );
}

#[test]

#[ignore = "known-failure probe: reproduces an unfixed defect. Run with `cargo test -- --ignored`; delete this attribute when fixed."]
fn create_via_add_dir_recursive_also_preserves() {
    // The GUI's normal path is add_dir_recursive (parallel bulk pipeline),
    // which is a different code path from add_file. Check it too.
    let td = tempdir().unwrap();
    let srcdir = td.path().join("tree");
    fs::create_dir_all(&srcdir).unwrap();
    let f = srcdir.join("vintage.txt");
    write_old_file(&f, b"written in 1990\n");

    let dest = td.path().join("tree.zip");
    let mut archive = Archive::create(
        &dest,
        CreateOptions {
            format: ArchiveFormat::Zip,
            preserve_timestamps: true,
            ..Default::default()
        },
    )
    .unwrap();
    archive
        .add_dir_recursive::<fn(&otterzip_core::Progress) -> bool>(&srcdir, "", None)
        .unwrap();
    archive.commit().unwrap();

    let got = archived_mtime(&dest, "vintage.txt").unwrap();
    println!("add_dir_recursive -> entry mtime {got} (source {OLD_UNIX_SECS})");
    assert_eq!(
        got, OLD_UNIX_SECS,
        "add_dir_recursive also drops the source mtime"
    );
}
