//! PROBE — claim 3: the `pax_global_header` pseudo-entry that `git archive`
//! puts at the head of every tarball is extracted as if it were a real file.
//!
//! `git archive --format=tar HEAD` writes a typeflag 'g' member named
//! `pax_global_header` carrying `comment=<commit sha>`. GNU tar, bsdtar,
//! 7-Zip and Bandizip all consume-and-discard it. If OtterZip writes it to
//! disk, every extracted git tarball gains a junk file.

use std::fs;

use otterzip_core::{Archive, ExtractOptions, OpenMode, OverwritePolicy, ProgressSink, RootLayout};
use tempfile::tempdir;

struct NullSink;
impl ProgressSink for NullSink {
    fn update(&mut self, _: &otterzip_core::Progress) -> bool {
        true
    }
}

fn ustar_header(name: &[u8], size: u64, typeflag: u8) -> [u8; 512] {
    let mut h = [0u8; 512];
    h[..name.len()].copy_from_slice(name);
    h[100..107].copy_from_slice(b"0000666");
    h[108..115].copy_from_slice(b"0000000");
    h[116..123].copy_from_slice(b"0000000");
    h[124..135].copy_from_slice(format!("{:011o}", size).as_bytes());
    h[136..147].copy_from_slice(b"00000000000");
    h[156] = typeflag;
    h[257..263].copy_from_slice(b"ustar\0");
    h[263..265].copy_from_slice(b"00");
    for b in h.iter_mut().skip(148).take(8) {
        *b = b' ';
    }
    let sum: u32 = h.iter().map(|&b| b as u32).sum();
    h[148..155].copy_from_slice(format!("{:06o}\0", sum).as_bytes());
    h[155] = b' ';
    h
}

fn push_member(out: &mut Vec<u8>, name: &[u8], data: &[u8], typeflag: u8) {
    out.extend_from_slice(&ustar_header(name, data.len() as u64, typeflag));
    out.extend_from_slice(data);
    let pad = (512 - (data.len() % 512)) % 512;
    out.extend(std::iter::repeat(0u8).take(pad));
}

#[test]

#[ignore = "known-failure probe: reproduces an unfixed defect. Run with `cargo test -- --ignored`; delete this attribute when fixed."]
fn probe_tar_pax_global_header() {
    let dir = tempdir().unwrap();
    let tar_path = dir.path().join("git-archive.tar");

    // Byte-identical in shape to `git archive` output.
    let pax_record = b"52 comment=9f1c0e5b0f2d4a6c8e0b2d4f6a8c0e2b4d6f8a0c\n";

    let mut bytes = Vec::new();
    push_member(&mut bytes, b"pax_global_header", pax_record, b'g');
    push_member(&mut bytes, b"README.md", b"# project\n", b'0');
    bytes.extend(std::iter::repeat(0u8).take(1024));
    fs::write(&tar_path, &bytes).unwrap();

    let archive = Archive::open(&tar_path, OpenMode::Read).unwrap();

    println!("--- entries() ---");
    for e in archive.entries().unwrap() {
        let e = e.unwrap();
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

    let mut on_disk: Vec<String> = fs::read_dir(&dest)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    on_disk.sort();
    println!("--- filesystem ---");
    println!("  {:?}", on_disk);
    let pgh = dest.join("pax_global_header");
    if pgh.exists() {
        println!("  pax_global_header content = {:?}", fs::read(&pgh).unwrap());
    }

    assert!(
        !pgh.exists(),
        "REPRODUCED: pax_global_header materialised as a real file on disk"
    );
    assert_eq!(on_disk, vec!["README.md".to_string()]);
}

/// Amplification: `git archive --prefix=proj-1.0/` (exactly what GitHub's
/// "Source code (tar.gz)" release asset is) wraps everything in one folder,
/// so Smart Extract should report `SingleFolder`. The stray root-level
/// `pax_global_header` is the FIRST entry probed, has no `/`, and makes
/// `detect_root_layout` bail to `Flat` — so the UI wraps an already-wrapped
/// tarball into `proj-1.0/proj-1.0/`.
#[test]
#[ignore = "known-failure probe: reproduces an unfixed defect. Run with `cargo test -- --ignored`; delete this attribute when fixed."]
fn probe_pax_global_header_poisons_root_layout() {
    let dir = tempdir().unwrap();
    let tar_path = dir.path().join("prefixed.tar");

    let mut bytes = Vec::new();
    push_member(
        &mut bytes,
        b"pax_global_header",
        b"52 comment=9f1c0e5b0f2d4a6c8e0b2d4f6a8c0e2b4d6f8a0c\n",
        b'g',
    );
    push_member(&mut bytes, b"proj-1.0/README.md", b"# project\n", b'0');
    push_member(&mut bytes, b"proj-1.0/src/main.rs", b"fn main() {}\n", b'0');
    bytes.extend(std::iter::repeat(0u8).take(1024));
    fs::write(&tar_path, &bytes).unwrap();

    let archive = Archive::open(&tar_path, OpenMode::Read).unwrap();
    let layout = archive.detect_root_layout().unwrap();
    println!("--- detect_root_layout = {:?} ---", layout);

    assert!(
        matches!(&layout, RootLayout::SingleFolder(n) if n == "proj-1.0"),
        "REPRODUCED: pax_global_header forced Smart Extract to {:?}, \
         which double-wraps every GitHub source tarball",
        layout
    );
}
