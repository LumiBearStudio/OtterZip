//! PROBE: does a .deb truncated *inside a member's content* extract
//! silently and report full success?
//!
//! `DebBackend::open` pushes the member onto `members` BEFORE the
//! `if next > total_len { break }` EOF guard runs, so a member whose
//! declared size runs past EOF is kept with its full declared length.
//! `extract_entry` then does `take(content_len)` + `io::copy`, which
//! happily copies however many bytes actually exist and returns Ok.

use std::fs;
use std::io::{Cursor, Write};
use std::path::Path;

use otterzip_core::{Archive, ExtractOptions, OpenMode, ProgressSink};
use tempfile::tempdir;

struct NullSink;
impl ProgressSink for NullSink {
    fn update(&mut self, _: &otterzip_core::Progress) -> bool {
        true
    }
}

fn write_ar_member<W: Write>(w: &mut W, name: &str, content: &[u8]) {
    let mut name_field = [b' '; 16];
    let display = format!("{name}/");
    let bytes = display.as_bytes();
    let n = bytes.len().min(16);
    name_field[..n].copy_from_slice(&bytes[..n]);

    let mut header = Vec::with_capacity(60);
    header.extend_from_slice(&name_field);
    header.extend_from_slice(b"0           ");
    header.extend_from_slice(b"0     ");
    header.extend_from_slice(b"0     ");
    header.extend_from_slice(b"0       ");
    let mut size_field = [b' '; 10];
    let s = content.len().to_string();
    size_field[..s.len()].copy_from_slice(s.as_bytes());
    header.extend_from_slice(&size_field);
    header.extend_from_slice(b"`\n");
    assert_eq!(header.len(), 60);

    w.write_all(&header).unwrap();
    w.write_all(content).unwrap();
    if content.len() % 2 == 1 {
        w.write_all(b"\n").unwrap();
    }
}

fn make_tar_gz(members: &[(&str, &[u8])]) -> Vec<u8> {
    let mut buf = Cursor::new(Vec::new());
    {
        let genc = flate2::write::GzEncoder::new(&mut buf, flate2::Compression::default());
        let mut tarball = tar::Builder::new(genc);
        for (name, data) in members {
            let mut header = tar::Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            tarball.append_data(&mut header, name, *data).unwrap();
        }
        tarball.into_inner().unwrap().finish().unwrap();
    }
    buf.into_inner()
}

fn write_full_deb(path: &Path) -> (Vec<u8>, usize) {
    let control = make_tar_gz(&[("control", b"Package: otterzip-probe\n")]);
    // Incompressible payload (LCG) so the gzipped member stays large —
    // this is what a real half-downloaded .deb looks like.
    let mut seed: u32 = 0x1234_5678;
    let payload: Vec<u8> = (0..64_000)
        .map(|_| {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (seed >> 24) as u8
        })
        .collect();
    let data = make_tar_gz(&[("usr/bin/hello", payload.as_slice())]);

    let mut out: Vec<u8> = Vec::new();
    out.extend_from_slice(b"!<arch>\n");
    write_ar_member(&mut out, "debian-binary", b"2.0\n");
    write_ar_member(&mut out, "control.tar.gz", &control);
    write_ar_member(&mut out, "data.tar.gz", &data);
    fs::write(path, &out).unwrap();
    (out, data.len())
}

#[test]

#[ignore = "known-failure probe: reproduces an unfixed defect. Run with `cargo test -- --ignored`; delete this attribute when fixed."]
fn deb_truncated_mid_member_reports_success_with_short_output() {
    let td = tempdir().unwrap();
    let full = td.path().join("full.deb");
    let (bytes, data_len) = write_full_deb(&full);

    // Chop the last ~60 % of the data.tar.gz member off — the header
    // survives intact, the content does not.
    let cut = bytes.len() - (data_len * 6 / 10);
    let truncated = td.path().join("truncated.deb");
    fs::write(&truncated, &bytes[..cut]).unwrap();

    println!("full deb      : {} bytes", bytes.len());
    println!("truncated deb : {cut} bytes (lost {} bytes)", bytes.len() - cut);
    println!("declared data.tar.gz member size: {data_len}");

    let archive = Archive::open(&truncated, OpenMode::Read).expect("open truncated .deb");
    let entries: Vec<_> = archive.entries().unwrap().filter_map(|r| r.ok()).collect();
    println!("--- entries the backend reports ---");
    for e in &entries {
        println!("  {:?}  declared size = {}", e.path, e.uncompressed_size);
    }

    let dest = td.path().join("out");
    fs::create_dir_all(&dest).unwrap();
    let opts = ExtractOptions {
        destination: dest.clone(),
        ..ExtractOptions::default()
    };
    let report = archive.extract_all::<NullSink>(&opts, None);
    println!("--- extract_all result ---");
    match &report {
        Ok(r) => println!(
            "  Ok: entries_extracted={} entries_skipped={} bytes_written={} warnings={:?}",
            r.entries_extracted, r.entries_skipped, r.bytes_written, r.warnings
        ),
        Err(e) => println!("  Err: {e:?}"),
    }

    println!("--- what actually landed on disk ---");
    let mut on_disk_data_len = 0u64;
    for de in fs::read_dir(&dest).unwrap() {
        let p = de.unwrap().path();
        let len = fs::metadata(&p).unwrap().len();
        println!("  {:?} = {len} bytes", p.file_name().unwrap());
        if p.file_name().unwrap() == "data.tar.gz" {
            on_disk_data_len = len;
        }
    }

    // REGRESSION ASSERT (desired behaviour): a member whose declared size
    // runs past EOF is truncation. `DebBackend::open` should reject it (as
    // it already does for a truncated *header*), or extract_all must at
    // minimum not report unqualified success. Silently writing a short
    // file and calling it done is the defect.
    match report {
        Err(e) => {
            println!("\nGOOD: surfaced an error -> {e:?}");
        }
        Ok(r) => {
            println!(
                "\nDECLARED data.tar.gz = {data_len}, WROTE = {on_disk_data_len}, \
                 report = {} extracted / {} skipped / {} warnings",
                r.entries_extracted,
                r.entries_skipped,
                r.warnings.len()
            );
            assert!(
                on_disk_data_len >= data_len as u64 || !r.warnings.is_empty(),
                "extract_all reported success ({} entries, {} skipped, no warnings) \
                 but only wrote {on_disk_data_len} of the declared {data_len} bytes \
                 for data.tar.gz — {}% of the member is missing and nothing said so",
                r.entries_extracted,
                r.entries_skipped,
                100 - (on_disk_data_len * 100 / data_len as u64)
            );
        }
    }
}
