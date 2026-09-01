//! Regression (issue #1): a ZIP whose entry names are legacy CP949 raw bytes
//! with GP bit 11 (UTF-8) OFF and no Info-ZIP `0x7075` extra field must list
//! and extract with names decoded through the shared encoding cascade
//! (`crate::encoding`) — the same path tar already uses — instead of the `zip`
//! crate's CP437 fallback, which mojibakes Korean filenames
//! ("한글파일명.txt" → "╟╤▒█╞─└╧╕φ.txt").
//!
//! The fixture `fixtures/legacy_cp949.zip` is the exact repro attached to the
//! bug report. Its two entries are:
//!   * 한글파일명.txt   (CP949 C7 D1 B1 DB C6 C4 C0 CF B8 ED, bit 11 = 0)
//!   * 테스트_문서v1.txt (CP949, bit 11 = 0, no extra field at all)
//! Windows Explorer and Bandizip read both correctly via the system ANSI
//! codepage; OtterZip <=1.2.3 showed CP437 mojibake because the ZIP reader
//! never fed the raw bytes to the cascade.
//!
//! Locale-independent by design: like `tar_legacy_codepage_names`, we do NOT
//! assert the exact Korean string, because the codepage the cascade lands on
//! depends on the test host's locale + chardetng's guess. We DO assert the
//! entries are listed, are extracted (2 files on disk), and are NOT the CP437
//! box-drawing mojibake the old path produced.

use std::fs;

use otterzip_core::{Archive, ExtractOptions, OpenMode, OverwritePolicy, ProgressSink};
use tempfile::tempdir;

const FIXTURE: &[u8] = include_bytes!("fixtures/legacy_cp949.zip");

struct NullSink;
impl ProgressSink for NullSink {
    fn update(&mut self, _: &otterzip_core::Progress) -> bool {
        true
    }
}

/// True if `s` contains any code point in the CP437 box-drawing / block-element
/// range (U+2500..=U+259F). Korean Hangul (U+AC00+) and the Unicode replacement
/// character (U+FFFD, the non-CJK-locale fallback) both fall outside it, so this
/// is a robust "the name was mis-decoded as CP437" detector.
fn looks_like_cp437_mojibake(s: &str) -> bool {
    s.chars().any(|c| ('\u{2500}'..='\u{259F}').contains(&c))
}

#[test]
fn zip_cp949_names_do_not_mojibake() {
    let dir = tempdir().unwrap();
    let zip_path = dir.path().join("legacy_cp949.zip");
    fs::write(&zip_path, FIXTURE).unwrap();

    let archive = Archive::open(&zip_path, OpenMode::Read).unwrap();

    let listed: Vec<_> = archive
        .entries()
        .unwrap()
        .map(|e| e.unwrap())
        .collect();

    println!("--- entries() ---");
    for e in &listed {
        println!("  path={:?} size={}", e.path, e.uncompressed_size);
    }

    assert_eq!(listed.len(), 2, "fixture has exactly two entries");
    assert!(
        !listed.iter().any(|e| e.path.is_empty()),
        "an entry was listed with an empty name"
    );
    for e in &listed {
        assert!(
            !looks_like_cp437_mojibake(&e.path),
            "REPRODUCED issue #1: name decoded as CP437 mojibake: {:?}",
            e.path
        );
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
    println!(
        "--- report --- extracted={} skipped={} bytes={}",
        report.entries_extracted, report.entries_skipped, report.bytes_written
    );

    let mut on_disk: Vec<String> = fs::read_dir(&dest)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    on_disk.sort();
    println!("--- filesystem --- {:?}", on_disk);

    assert_eq!(
        on_disk.len(),
        2,
        "both legacy-codepage entries must extract to disk"
    );
    for name in &on_disk {
        assert!(
            !looks_like_cp437_mojibake(name),
            "extracted file name is CP437 mojibake: {name:?}"
        );
    }
}
