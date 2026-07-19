//! PROBE: `backends/sevenz.rs::meta_to_entry` hard-codes
//! `encryption: EncryptionMethod::None`, so no 7z entry ever reports
//! encryption and `Archive::is_encrypted()` is false for every 7z.
//!
//! The fixture below is the shape 7-Zip / most GUIs produce for a plain
//! `-p` password: **content encrypted with AES-256, header left readable**
//! ("Encrypt file names" unticked). It opens with no password at all — so
//! the WrongPassword-at-open shortcut that saves the header-encrypted case
//! never fires, and `is_encrypted()` is the only signal left.

use std::fs;

use otterzip_core::format::{CompressionMethod, EncryptionMethod};
use otterzip_core::{Archive, ArchiveFormat, CreateOptions, OpenMode};
use tempfile::tempdir;
use zeroize::Zeroizing;

/// AES-256 content, plaintext header. Entry `secret.txt`, password `hunter2`.
/// Generated with sevenz-rust2's `ArchiveWriter` + `AesEncoderOptions` and
/// *without* `set_encrypt_header(true)`.
const ENC_PLAINTEXT_HEADER_7Z: &str = concat!(
    "377abcaf271c00047c02c04534000000000000006b00000000000000281b1e45",
    "01002f2f6a6ed529dc6f23466877d809f50ac7e06e080bb81f764673a8511da5",
    "dd42e37bd5c22434b38c4822aac3714db2efca00010406000109340a016e0a8a",
    "3100070b010002212101102406f1070122c8fff2ed0041155da325715c15d66b",
    "d8ba9bb692e794ed0d59c2b61f1e7767c0453401000c302d00080a015393e496",
    "000005011117007300650063007200650074002e0074007800740000000000",
);

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len() / 2)
        .map(|i| u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).unwrap())
        .collect()
}

#[test]

#[ignore = "known-failure probe: reproduces an unfixed defect. Run with `cargo test -- --ignored`; delete this attribute when fixed."]
fn sevenz_encrypted_entry_reports_encryption() {
    let td = tempdir().unwrap();

    // ---- case A: real-world shape — AES content, readable header --------
    let a_path = td.path().join("enc_plainhdr.7z");
    fs::write(&a_path, unhex(ENC_PLAINTEXT_HEADER_7Z)).unwrap();

    let a = Archive::open(&a_path, OpenMode::Read)
        .expect("plaintext-header 7z must open with NO password");
    println!("[A] opened with no password (header is readable)");
    for e in a.entries().unwrap() {
        let e = e.unwrap();
        println!(
            "[A]   entry {:?}: encryption={:?} size={}",
            e.path, e.encryption, e.uncompressed_size
        );
    }
    let a_flag = a.is_encrypted().unwrap();
    println!("[A] Archive::is_encrypted() = {a_flag}");

    // Prove the content really is encrypted: reading it without the
    // password must fail.
    let mut sink = Vec::new();
    let read_no_pw = a
        .entries()
        .unwrap()
        .next()
        .map(|e| e.unwrap().path)
        .map(|p| {
            let mut r = a.read_entry(&p);
            match &mut r {
                Ok(rd) => std::io::copy(rd, &mut sink).map(|_| ()).map_err(|e| e.to_string()),
                Err(e) => Err(format!("{e:?}")),
            }
        });
    println!("[A] read_entry without password = {read_no_pw:?}");

    // ---- case B: our own writer, encrypted, opened WITH the password ----
    let src = td.path().join("b.txt");
    fs::write(&src, b"beta payload\n").unwrap();
    let b_path = td.path().join("ours_enc.7z");
    let mut w = Archive::create(
        &b_path,
        CreateOptions {
            format: ArchiveFormat::SevenZ,
            compression: CompressionMethod::Lzma2,
            compression_level: 1,
            encryption: EncryptionMethod::Aes256,
            password: Some(Zeroizing::new("hunter2".to_string())),
            ..Default::default()
        },
    )
    .unwrap();
    w.add_file(&src, "b.txt").unwrap();
    w.commit().unwrap();

    let b = Archive::open_with_password(
        &b_path,
        OpenMode::Read,
        Zeroizing::new("hunter2".to_string()),
    )
    .unwrap();
    for e in b.entries().unwrap() {
        let e = e.unwrap();
        println!("[B]   entry {:?}: encryption={:?}", e.path, e.encryption);
    }
    let b_flag = b.is_encrypted().unwrap();
    println!("[B] Archive::is_encrypted() = {b_flag}  (archive was built WITH AES-256)");

    assert!(
        a_flag,
        "an AES-256 7z with a readable header must report is_encrypted() == true; \
         got false, so the host never shows the password prompt for it"
    );
    assert!(b_flag, "our own AES-256 7z must report is_encrypted() == true");
}
