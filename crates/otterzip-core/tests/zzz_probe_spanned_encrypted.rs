//! PROBE: can a password-protected split / spanned ZIP be opened at all?
//!
//! `Archive::open_multi` takes only `(volumes, mode)` — no password
//! parameter — and hardcodes `ZipBackend::open_multi(volumes, None)`
//! plus `password: None` on the returned handle, even though
//! `ZipBackend::open_multi` accepts `Option<&Zeroizing<String>>`.

use std::fs;
use std::path::{Path, PathBuf};

use otterzip_core::format::{CompressionMethod, EncryptionMethod};
use otterzip_core::options::ExtractOptions;
use otterzip_core::{Archive, ArchiveFormat, CreateOptions, OpenMode};
use tempfile::tempdir;
use zeroize::Zeroizing;

const PW: &str = "s3cret";

fn make_encrypted_zip(out: &Path, src_dir: &Path) {
    fs::create_dir_all(src_dir).unwrap();
    for (name, body) in [("a.txt", "alpha\n"), ("b.txt", "bravo\n")] {
        fs::write(src_dir.join(name), body).unwrap();
    }
    let opts = CreateOptions {
        format: ArchiveFormat::Zip,
        compression: CompressionMethod::Deflate,
        compression_level: 5,
        encryption: EncryptionMethod::Aes256,
        password: Some(Zeroizing::new(PW.to_string())),
        ..Default::default()
    };
    let mut w = Archive::create(out, opts).unwrap();
    w.add_file(src_dir.join("a.txt"), "a.txt").unwrap();
    w.add_file(src_dir.join("b.txt"), "b.txt").unwrap();
    w.commit().unwrap();
}

/// Raw byte-split into `stem.z01` + `stem.zip` (the shape 7-Zip / WinRAR
/// emit and the one `open_multi`'s doc comment says is supported).
fn split_in_two(blob: &[u8], dir: &Path, stem: &str) -> Vec<PathBuf> {
    let cut = blob.len() / 2;
    let a = dir.join(format!("{stem}.z01"));
    let b = dir.join(format!("{stem}.zip"));
    fs::write(&a, &blob[..cut]).unwrap();
    fs::write(&b, &blob[cut..]).unwrap();
    vec![a, b]
}

fn try_extract(archive: &Archive, dest: PathBuf) -> Result<(), String> {
    let opts = ExtractOptions {
        destination: dest,
        ..Default::default()
    };
    archive
        .extract_all::<fn(&otterzip_core::Progress) -> bool>(&opts, None)
        .map(|_| ())
        .map_err(|e| e.to_string())
}

#[test]

#[ignore = "known-failure probe: reproduces an unfixed defect. Run with `cargo test -- --ignored`; delete this attribute when fixed."]
fn encrypted_split_zip_is_openable_with_a_password() {
    let td = tempdir().unwrap();
    let single = td.path().join("enc.zip");
    make_encrypted_zip(&single, &td.path().join("src"));
    let blob = fs::read(&single).unwrap();
    println!("encrypted archive bytes : {}", blob.len());

    // --- Control A: single-volume WITH password works. ---
    let ok = Archive::open_with_password(&single, OpenMode::Read, Zeroizing::new(PW.to_string()))
        .unwrap();
    let r = try_extract(&ok, td.path().join("out_single_pw"));
    println!("single + password       : {r:?}");
    assert!(r.is_ok(), "control A must succeed");
    assert_eq!(
        fs::read_to_string(td.path().join("out_single_pw").join("a.txt")).unwrap(),
        "alpha\n"
    );

    // --- Control B: single-volume WITHOUT password fails (expected). ---
    let no_pw = Archive::open(&single, OpenMode::Read).unwrap();
    let r = try_extract(&no_pw, td.path().join("out_single_nopw"));
    println!("single, no password     : {r:?}");
    assert!(r.is_err(), "control B must fail");

    // --- Subject: the same archive, byte-split across two volumes. ---
    let vols = split_in_two(&blob, td.path(), "enc_split");
    let multi = Archive::open_multi(&vols, OpenMode::Read).expect("split set must open");
    let names: Vec<String> = multi
        .entries()
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path)
        .collect();
    println!("split: entries listed   : {names:?}");

    let r = try_extract(&multi, td.path().join("out_multi"));
    println!("split, open_multi       : {r:?}");

    // There is no `open_multi_with_password` / password argument anywhere
    // on the multi-volume path, so this is the ONLY way to ask for it.
    assert!(
        r.is_ok(),
        "an encrypted split ZIP must be extractable — no password can be supplied on the \
         open_multi path, so extraction fails with: {}",
        r.unwrap_err()
    );
}
