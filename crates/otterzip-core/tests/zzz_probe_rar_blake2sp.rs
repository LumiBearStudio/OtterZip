//! PROBE: RAR5 archives hashed with BLAKE2sp (WinRAR `-htb`).
//!
//! UnRAR's `HashValue` is a union — `{ uint CRC32; byte Digest[..] }` —
//! and `dll.cpp:273` copies it out unconditionally:
//!     `D->FileCRC = hd->FileHash.CRC32;`
//! With `FileHash.Type == HASH_BLAKE2` that field is simply the first four
//! bytes of the 32-byte BLAKE2sp digest. `backends/rar.rs:916` stores it as
//! `crc32: Some(h.file_crc)` with no hash-type guard, and `Archive::test`
//! then compares it against a real CRC32 of the extracted bytes.
//!
//! Both fixtures below hold the SAME payload as a stored (method 0) entry
//! and differ only in how the hash is recorded. Both are accepted by
//! third-party readers (verified with `bz t`: "All OK").

use std::fs;

use otterzip_core::{Archive, ExtractOptions, OpenMode, OverwritePolicy};
use tempfile::tempdir;

/// RAR5, entry `HELLO.TXT`, stored, hash in a FHEXTRA_HASH (BLAKE2sp) record.
const BLAKE_RAR: &str = concat!(
    "526172211a070100c51a333203010000e99e0d1238020323e40100e401200000",
    "0948454c4c4f2e545854220200ce6ec4cf5e636744537f979c7445c6c84f7691",
    "d313c51e2f13e395c2f24577cb4f747465725a6970205241523520424c414b45",
    "3273702070726f6265207061796c6f61642e204f747465725a69702052415235",
    "20424c414b453273702070726f6265207061796c6f61642e204f747465725a69",
    "70205241523520424c414b453273702070726f6265207061796c6f61642e204f",
    "747465725a6970205241523520424c414b453273702070726f6265207061796c",
    "6f61642e204f747465725a6970205241523520424c414b453273702070726f62",
    "65207061796c6f61642e204f747465725a6970205241523520424c414b453273",
    "702070726f6265207061796c6f61642e201d77565103050400",
);

/// Identical, except the hash is a classic CRC32 in the FHFL_CRC32 field.
const CRC_RAR: &str = concat!(
    "526172211a070100c51a333203010000081c662a180202e40104e40120b5fc0f",
    "b500000948454c4c4f2e5458544f747465725a6970205241523520424c414b45",
    "3273702070726f6265207061796c6f61642e204f747465725a69702052415235",
    "20424c414b453273702070726f6265207061796c6f61642e204f747465725a69",
    "70205241523520424c414b453273702070726f6265207061796c6f61642e204f",
    "747465725a6970205241523520424c414b453273702070726f6265207061796c",
    "6f61642e204f747465725a6970205241523520424c414b453273702070726f62",
    "65207061796c6f61642e204f747465725a6970205241523520424c414b453273",
    "702070726f6265207061796c6f61642e201d77565103050400",
);

/// crc32 of the payload; the value a correct RAR5 CRC32 archive records.
const REAL_CRC32: u32 = 0xb50f_fcb5;
/// First four bytes (LE) of the payload's BLAKE2sp digest
/// (ce6ec4cf5e63...) — what the union hands back instead.
const BLAKE_PREFIX_AS_U32: u32 = 0xcfc4_6ece;

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len() / 2)
        .map(|i| u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).unwrap())
        .collect()
}

fn inspect(tag: &str, path: &std::path::Path, dest: &std::path::Path) -> (Option<u32>, u32) {
    let a = Archive::open(path, OpenMode::Read).expect("open rar5 fixture");

    let e = a.entries().unwrap().next().unwrap().unwrap();
    println!(
        "[{tag}] entry {:?}  size={}  crc32 field = {:?}",
        e.path,
        e.uncompressed_size,
        e.crc32.map(|c| format!("0x{c:08x}"))
    );

    // Extraction proves unrar itself is happy with the archive's hash.
    let opts = ExtractOptions {
        destination: dest.to_path_buf(),
        overwrite: OverwritePolicy::Always,
        ..Default::default()
    };
    let rep = a.extract_all::<fn(&otterzip_core::Progress) -> bool>(&opts, None);
    match &rep {
        Ok(r) => println!("[{tag}] extract_all -> Ok({} entries)", r.entries_extracted),
        Err(err) => println!("[{tag}] extract_all -> Err({err:?})"),
    }
    assert!(
        rep.is_ok(),
        "[{tag}] unrar must accept the fixture, otherwise this probe proves nothing"
    );

    let a2 = Archive::open(path, OpenMode::Read).unwrap();
    let t = a2
        .test::<fn(&otterzip_core::Progress) -> bool>(None)
        .expect("test must not error");
    println!(
        "[{tag}] Archive::test -> tested={} corrupted={} {:?}",
        t.entries_tested, t.entries_corrupted, t.corrupted_entries
    );
    (e.crc32, t.entries_corrupted)
}

#[test]

#[ignore = "known-failure probe: reproduces an unfixed defect. Run with `cargo test -- --ignored`; delete this attribute when fixed."]
fn rar5_blake2sp_entry_passes_verification() {
    let td = tempdir().unwrap();

    let crc_path = td.path().join("crc.rar");
    fs::write(&crc_path, unhex(CRC_RAR)).unwrap();
    let (crc_field, crc_corrupt) = inspect("CRC32 ", &crc_path, &td.path().join("out_crc"));

    println!();

    let blake_path = td.path().join("blake.rar");
    fs::write(&blake_path, unhex(BLAKE_RAR)).unwrap();
    let (blake_field, blake_corrupt) =
        inspect("BLAKE2", &blake_path, &td.path().join("out_blake"));

    println!(
        "\nreal crc32 of payload      = 0x{REAL_CRC32:08x}\n\
         blake2sp digest[0..4] (LE) = 0x{BLAKE_PREFIX_AS_U32:08x}"
    );

    // Control: the CRC32 archive behaves.
    assert_eq!(crc_field, Some(REAL_CRC32), "control fixture crc mismatch");
    assert_eq!(crc_corrupt, 0, "control fixture must verify clean");

    // The BLAKE2sp archive is byte-identical in payload, and is accepted by
    // unrar and by Bandizip. It must not be reported as damaged.
    assert_ne!(
        blake_field,
        Some(BLAKE_PREFIX_AS_U32),
        "the BLAKE2sp digest prefix is being surfaced as if it were a CRC32"
    );
    assert_eq!(
        blake_corrupt, 0,
        "a valid -htb (BLAKE2sp) RAR5 must not report entries as corrupted; \
         every entry of every such archive fails verification"
    );
}
