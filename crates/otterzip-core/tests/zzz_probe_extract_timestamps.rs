//! PROBE 3 — claim: `ExtractOptions::preserve_timestamps` is never read;
//! every extracted file gets wall-clock "now" as its mtime.

use std::fs;
use std::io::Write;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use otterzip_core::options::{ExtractOptions, OverwritePolicy};
use otterzip_core::{Archive, OpenMode};
use tempfile::tempdir;

/// 1990-06-15 12:00:00 — comfortably outside any plausible "now".
const OLD_UNIX_SECS: u64 = 645_451_200;

fn build_zip_with_old_mtime(out: &Path, name: &str, body: &[u8]) {
    let f = fs::File::create(out).unwrap();
    let mut w = zip::ZipWriter::new(f);
    let stamp = zip::DateTime::from_date_and_time(1990, 6, 15, 12, 0, 0).unwrap();
    let opts = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .last_modified_time(stamp);
    w.start_file(name, opts).unwrap();
    w.write_all(body).unwrap();
    w.finish().unwrap();
}

fn secs(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH).unwrap().as_secs()
}

#[test]

#[ignore = "known-failure probe: reproduces an unfixed defect. Run with `cargo test -- --ignored`; delete this attribute when fixed."]
fn extract_preserves_the_archived_mtime() {
    let td = tempdir().unwrap();
    let zip_path = td.path().join("old.zip");
    build_zip_with_old_mtime(&zip_path, "old.txt", b"vintage\n");

    // Confirm the fixture really carries the old stamp (reader side works).
    {
        let a = Archive::open(&zip_path, OpenMode::Read).unwrap();
        let e = a.entries().unwrap().next().unwrap().unwrap();
        println!(
            "entry {:?} modified-in-archive = {:?} ({} unix secs)",
            e.path,
            e.modified,
            e.modified.map(secs).unwrap_or(0)
        );
        assert_eq!(
            e.modified.map(secs),
            Some(OLD_UNIX_SECS),
            "fixture sanity: the archive must carry the 1990 stamp"
        );
    }

    let dest = td.path().join("out");
    fs::create_dir_all(&dest).unwrap();
    let opts = ExtractOptions {
        destination: dest.clone(),
        overwrite: OverwritePolicy::Always,
        preserve_timestamps: true,
        ..Default::default()
    };
    let a = Archive::open(&zip_path, OpenMode::Read).unwrap();
    a.extract_all::<fn(&otterzip_core::Progress) -> bool>(&opts, None)
        .unwrap();

    let out_file = dest.join("old.txt");
    let got = fs::metadata(&out_file).unwrap().modified().unwrap();
    let now = SystemTime::now();
    println!(
        "extracted mtime = {} unix secs; now = {} unix secs; delta = {} s",
        secs(got),
        secs(now),
        secs(now).saturating_sub(secs(got))
    );

    let drift = now
        .duration_since(got)
        .unwrap_or(Duration::ZERO);
    println!("mtime is {} seconds old", drift.as_secs());

    assert_eq!(
        secs(got),
        OLD_UNIX_SECS,
        "preserve_timestamps=true, but the extracted file was stamped with \
         wall-clock now instead of the archived 1990 mtime"
    );
}

#[test]

#[ignore = "known-failure probe: reproduces an unfixed defect. Run with `cargo test -- --ignored`; delete this attribute when fixed."]
fn extract_preserves_mtime_on_the_streaming_tar_path_too() {
    // tar.* uses a different extractor (TarBackend::extract_all_inner,
    // hand-rolled File::create + copy rather than tar::Entry::unpack), so
    // cover it separately.
    let td = tempdir().unwrap();
    let tgz = td.path().join("old.tar.gz");
    {
        let f = fs::File::create(&tgz).unwrap();
        let enc = flate2::write::GzEncoder::new(f, flate2::Compression::default());
        let mut builder = tar::Builder::new(enc);
        let body = b"vintage\n";
        let mut header = tar::Header::new_gnu();
        header.set_size(body.len() as u64);
        header.set_mode(0o644);
        header.set_mtime(OLD_UNIX_SECS);
        header.set_cksum();
        builder
            .append_data(&mut header, "old.txt", &body[..])
            .unwrap();
        builder.into_inner().unwrap().finish().unwrap();
    }

    let dest = td.path().join("tar-out");
    fs::create_dir_all(&dest).unwrap();
    let opts = ExtractOptions {
        destination: dest.clone(),
        overwrite: OverwritePolicy::Always,
        preserve_timestamps: true,
        ..Default::default()
    };
    let a = Archive::open(&tgz, OpenMode::Read).unwrap();
    a.extract_all::<fn(&otterzip_core::Progress) -> bool>(&opts, None)
        .unwrap();

    let got = secs(
        fs::metadata(dest.join("old.txt"))
            .unwrap()
            .modified()
            .unwrap(),
    );
    println!("tar.gz extracted mtime = {got} (archived {OLD_UNIX_SECS})");
    assert_eq!(
        got, OLD_UNIX_SECS,
        "tar.gz streaming extract also stamps wall-clock now"
    );
}

#[test]

#[ignore = "known-failure probe: reproduces an unfixed defect. Run with `cargo test -- --ignored`; delete this attribute when fixed."]
fn preserve_timestamps_false_vs_true_should_differ() {
    // If the flag were wired at all, these two runs would produce different
    // mtimes. Identical "now" for both proves the field is inert.
    let td = tempdir().unwrap();
    let zip_path = td.path().join("old2.zip");
    build_zip_with_old_mtime(&zip_path, "old.txt", b"vintage\n");

    let mut stamps = Vec::new();
    for keep in [true, false] {
        let dest = td.path().join(format!("out-{keep}"));
        fs::create_dir_all(&dest).unwrap();
        let opts = ExtractOptions {
            destination: dest.clone(),
            overwrite: OverwritePolicy::Always,
            preserve_timestamps: keep,
            ..Default::default()
        };
        let a = Archive::open(&zip_path, OpenMode::Read).unwrap();
        a.extract_all::<fn(&otterzip_core::Progress) -> bool>(&opts, None)
            .unwrap();
        let m = secs(
            fs::metadata(dest.join("old.txt"))
                .unwrap()
                .modified()
                .unwrap(),
        );
        println!("preserve_timestamps={keep} -> extracted mtime {m}");
        stamps.push(m);
    }
    assert_ne!(
        stamps[0], stamps[1],
        "preserve_timestamps true/false produced the same mtime — flag is inert"
    );
}
