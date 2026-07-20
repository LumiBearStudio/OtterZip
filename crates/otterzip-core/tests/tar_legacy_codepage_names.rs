//! Regression: a tar whose member names are legacy MBCS (CP949 / Shift_JIS)
//! must NOT be silently dropped. tar carries no encoding flag, so the backend
//! reads raw name bytes (`Entry::path_bytes()`) and runs them through the
//! shared encoding cascade (crate::encoding) — the same detector Firefox uses
//! (chardetng) plus an OS-locale fallback. `Entry::path()` used to hard-error
//! on any non-UTF-8 name on Windows, which the backend turned into a silent
//! `continue`, so a Korean/Japanese tarball's members vanished with the
//! report still reading success.
//!
//! Locale-independent: the assertions only require that the member is LISTED
//! with a non-empty name and EXTRACTED (2 files on disk). Whether the exact
//! decoded string is CP949 vs the detector's guess can vary with the test
//! host's locale; being dropped must not.

use std::fs;

use otterzip_core::{Archive, ExtractOptions, OpenMode, OverwritePolicy, ProgressSink};
use tempfile::tempdir;

struct NullSink;
impl ProgressSink for NullSink {
    fn update(&mut self, _: &otterzip_core::Progress) -> bool {
        true
    }
}

/// Build a raw ustar 512-byte header with arbitrary (possibly non-UTF-8)
/// name bytes — the tar crate's typed API refuses to construct one on
/// Windows, but real archives are full of them.
fn ustar_header(name: &[u8], size: u64, typeflag: u8) -> [u8; 512] {
    let mut h = [0u8; 512];
    h[..name.len()].copy_from_slice(name);
    h[100..107].copy_from_slice(b"0000644"); // mode
    h[108..115].copy_from_slice(b"0000000"); // uid
    h[116..123].copy_from_slice(b"0000000"); // gid
    let size_field = format!("{:011o}", size);
    h[124..135].copy_from_slice(size_field.as_bytes());
    h[136..147].copy_from_slice(b"00000000000"); // mtime
    h[156] = typeflag;
    h[257..263].copy_from_slice(b"ustar\0");
    h[263..265].copy_from_slice(b"00");
    // checksum: computed with the checksum field treated as spaces
    for b in h.iter_mut().skip(148).take(8) {
        *b = b' ';
    }
    let sum: u32 = h.iter().map(|&b| b as u32).sum();
    let cks = format!("{:06o}\0", sum);
    h[148..155].copy_from_slice(cks.as_bytes());
    h[155] = b' ';
    h
}

fn push_member(out: &mut Vec<u8>, name: &[u8], data: &[u8]) {
    out.extend_from_slice(&ustar_header(name, data.len() as u64, b'0'));
    out.extend_from_slice(data);
    let pad = (512 - (data.len() % 512)) % 512;
    out.extend(std::iter::repeat(0u8).take(pad));
}

#[test]
fn probe_tar_legacy_codepage_name() {
    let dir = tempdir().unwrap();
    let tar_path = dir.path().join("cp949.tar");

    // "주문서.txt" in CP949/UHC (what a Korean tar tool writes).
    let cp949_name: Vec<u8> = vec![
        0xC1, 0xD6, 0xB9, 0xAE, 0xBC, 0xAD, b'.', b't', b'x', b't',
    ];
    let cp949_name: &[u8] = &cp949_name;
    assert!(
        String::from_utf8(cp949_name.to_vec()).is_err(),
        "fixture must be non-UTF-8"
    );

    let mut bytes = Vec::new();
    push_member(&mut bytes, b"ascii.txt", b"ascii payload\n");
    push_member(&mut bytes, cp949_name, b"KOREAN PAYLOAD\n");
    bytes.extend(std::iter::repeat(0u8).take(1024)); // end-of-archive
    fs::write(&tar_path, &bytes).unwrap();

    let archive = Archive::open(&tar_path, OpenMode::Read).unwrap();

    println!("--- entries() ---");
    let listed: Vec<_> = archive
        .entries()
        .unwrap()
        .map(|e| e.unwrap())
        .collect();
    for e in &listed {
        println!("  path={:?} size={}", e.path, e.uncompressed_size);
    }

    let dest = dir.path().join("out");
    fs::create_dir_all(&dest).unwrap();
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

    println!("--- filesystem ---");
    let mut on_disk: Vec<String> = fs::read_dir(&dest)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    on_disk.sort();
    println!("  {:?}", on_disk);

    // Expectation: both members land on disk, and neither is listed with an
    // empty name.
    assert!(
        !listed.iter().any(|e| e.path.is_empty()),
        "REPRODUCED: entries() emitted an entry with an empty path"
    );
    assert_eq!(
        on_disk.len(),
        2,
        "REPRODUCED: the legacy-codepage member was dropped on extract \
         (report claimed success)"
    );
}
