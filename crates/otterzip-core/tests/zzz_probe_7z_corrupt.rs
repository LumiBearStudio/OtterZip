//! PROBE: does a corrupt *unencrypted* 7z surface as WrongPassword?
//!
//! `backends/sevenz.rs::map_sevenz_err` folds
//! `sevenz_rust2::Error::ChecksumVerificationFailed` into
//! `OtterzipError::WrongPassword`. That variant is raised by
//! `reader.rs::read_start_header` on a plain **start-header CRC mismatch**,
//! which has nothing to do with encryption.

use std::fs;

use otterzip_core::{
    Archive, ArchiveFormat, CreateOptions, ExtractOptions, OpenMode, OtterzipError,
    OverwritePolicy,
};
use otterzip_core::format::CompressionMethod;
use tempfile::tempdir;

fn make_7z(dir: &std::path::Path, name: &str, body: &[u8]) -> std::path::PathBuf {
    let src = dir.join(format!("{name}.src"));
    fs::write(&src, body).unwrap();

    let path = dir.join(format!("{name}.7z"));
    let opts = CreateOptions {
        format: ArchiveFormat::SevenZ,
        compression: CompressionMethod::Lzma2,
        compression_level: 3,
        ..Default::default()
    };
    let mut a = Archive::create(&path, opts).unwrap();
    a.add_file(&src, "payload.bin").unwrap();
    a.commit().unwrap();
    path
}

/// Incompressible payload → LZMA2 stores it in *uncompressed* chunks, so a
/// single flipped byte still decodes cleanly and only trips the CRC.
fn incompressible(n: usize) -> Vec<u8> {
    let mut s: u64 = 0x2545_F491_4F6C_DD1D;
    (0..n)
        .map(|_| {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            (s >> 24) as u8
        })
        .collect()
}

fn report(tag: &str, e: &OtterzipError) {
    println!(
        "  {tag:<12} -> {e:?}\n               WrongPassword? {}",
        matches!(e, OtterzipError::WrongPassword)
    );
}

#[test]

#[ignore = "known-failure probe: reproduces an unfixed defect. Run with `cargo test -- --ignored`; delete this attribute when fixed."]
fn corrupt_unencrypted_7z_reported_as_wrong_password() {
    let td = tempdir().unwrap();
    let good = make_7z(td.path(), "plain", &incompressible(64 * 1024));
    let bytes = fs::read(&good).unwrap();
    println!("archive size = {} bytes", bytes.len());

    // Pristine control.
    let a = Archive::open(&good, OpenMode::Read).unwrap();
    println!("pristine: is_encrypted = {:?}", a.is_encrypted().unwrap());

    let mut wrong_password_hits = Vec::new();

    // The 7z signature header is 32 bytes:
    //   [0..6) magic  [6..8) version  [8..12) StartHeaderCRC
    //   [12..32) StartHeader (NextHeaderOffset/Size/CRC)
    // Corrupting anything in [12..32) trips the start-header CRC.
    for (off, what) in [
        (12usize, "NextHeaderOffset"),
        (20, "NextHeaderSize"),
        (28, "NextHeaderCRC field"),
    ] {
        let mut bad = bytes.clone();
        bad[off] ^= 0x01;
        let p = td.path().join(format!("bad_{off}.7z"));
        fs::write(&p, &bad).unwrap();

        println!("\n-- 1 bit flipped at byte {off} ({what}) --");
        match Archive::open(&p, OpenMode::Read) {
            Err(e) => {
                report("open", &e);
                if matches!(e, OtterzipError::WrongPassword) {
                    wrong_password_hits.push(format!("open @{off} ({what})"));
                }
            }
            Ok(a) => {
                let opts = ExtractOptions {
                    destination: td.path().join(format!("out_{off}")),
                    overwrite: OverwritePolicy::Always,
                    ..Default::default()
                };
                match a.extract_all::<fn(&otterzip_core::Progress) -> bool>(&opts, None) {
                    Ok(r) => println!("  extract      -> Ok({} entries)", r.entries_extracted),
                    Err(e) => {
                        report("extract_all", &e);
                        if matches!(e, OtterzipError::WrongPassword) {
                            wrong_password_hits.push(format!("extract @{off} ({what})"));
                        }
                    }
                }
            }
        }
    }

    // Truncation — the single most common real-world damage (interrupted
    // download / copy).
    for keep in [16usize, 24, bytes.len() / 2] {
        let p = td.path().join(format!("trunc_{keep}.7z"));
        fs::write(&p, &bytes[..keep]).unwrap();
        println!("\n-- truncated to {keep} of {} bytes --", bytes.len());
        match Archive::open(&p, OpenMode::Read) {
            Err(e) => {
                report("open", &e);
                if matches!(e, OtterzipError::WrongPassword) {
                    wrong_password_hits.push(format!("open @truncate({keep})"));
                }
            }
            Ok(a) => {
                let opts = ExtractOptions {
                    destination: td.path().join(format!("tout_{keep}")),
                    overwrite: OverwritePolicy::Always,
                    ..Default::default()
                };
                match a.extract_all::<fn(&otterzip_core::Progress) -> bool>(&opts, None) {
                    Ok(r) => println!("  extract      -> Ok({} entries)", r.entries_extracted),
                    Err(e) => {
                        report("extract_all", &e);
                        if matches!(e, OtterzipError::WrongPassword) {
                            wrong_password_hits.push(format!("extract @truncate({keep})"));
                        }
                    }
                }
            }
        }
    }

    println!("\n=== corrupt-but-unencrypted cases reported as WrongPassword ===");
    for h in &wrong_password_hits {
        println!("  * {h}");
    }
    assert!(
        wrong_password_hits.is_empty(),
        "a corrupt UNENCRYPTED 7z must never surface as WrongPassword \
         (the UI then prompts for a password that cannot help): {wrong_password_hits:?}"
    );
}
