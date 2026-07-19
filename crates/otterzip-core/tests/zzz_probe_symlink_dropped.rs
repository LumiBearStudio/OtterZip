//! PROBE: are symlinks / NTFS junctions silently dropped when creating
//! an archive from a directory tree?
//!
//! Both walkers in `backends/writer.rs` (`collect_entries` for the
//! parallel bulk path, `pre_scan_visit`/the serial walker) do:
//!
//!     if meta.file_type().is_symlink() && !follow_symlinks { continue; }
//!
//! `Archive::add_dir_recursive` hard-codes `follow_symlinks = false`,
//! and its return type is `Result<()>` — there is no report channel, so
//! a dropped link cannot be surfaced even in principle.
//!
//! On Windows `FileType::is_symlink()` is true for BOTH real symlinks
//! and junctions (name-surrogate reparse tags), so `mklink /J` — which
//! needs no elevation — is enough to lose an entire subtree.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use otterzip_core::{Archive, ArchiveFormat, CreateOptions, OpenMode};
use tempfile::tempdir;

/// Create a junction with `mklink /J` (no elevation required).
#[cfg(windows)]
fn make_junction(link: &Path, target: &Path) -> bool {
    let status = std::process::Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(link)
        .arg(target)
        .output();
    match status {
        Ok(o) => {
            if !o.status.success() {
                println!(
                    "  mklink /J failed: {}",
                    String::from_utf8_lossy(&o.stderr).trim()
                );
            }
            o.status.success() && link.exists()
        }
        Err(e) => {
            println!("  mklink /J could not run: {e}");
            false
        }
    }
}

#[cfg(windows)]
fn try_symlink_file(link: &Path, target: &Path) -> bool {
    match std::os::windows::fs::symlink_file(target, link) {
        Ok(()) => true,
        Err(e) => {
            println!("  symlink_file unavailable ({e}) — needs Developer Mode / admin");
            false
        }
    }
}

fn archive_entries(path: &Path) -> BTreeSet<String> {
    let a = Archive::open(path, OpenMode::Read).expect("open created archive");
    a.entries()
        .expect("entries")
        .filter_map(|r| r.ok())
        .map(|e| e.path)
        .collect()
}

/// Build a tree, zip it via the public create API, return what made it in.
fn build_and_zip(td: &Path, filler: usize, label: &str) -> (BTreeSet<String>, bool, bool) {
    let src = td.join(format!("src_{label}"));
    let outside = td.join(format!("outside_{label}"));
    fs::create_dir_all(&src).unwrap();
    fs::create_dir_all(&outside).unwrap();

    fs::write(src.join("real.txt"), b"a normal file\n").unwrap();
    fs::create_dir_all(src.join("subdir")).unwrap();
    fs::write(src.join("subdir").join("inner.txt"), b"nested\n").unwrap();
    // Filler decides whether the writer takes the parallel bulk path
    // (>= 16 entries) or the serial walker.
    for i in 0..filler {
        fs::write(src.join(format!("filler{i:03}.txt")), b"x").unwrap();
    }

    // The link targets: a directory with real content the user expects
    // to be in the backup.
    fs::create_dir_all(outside.join("docs")).unwrap();
    fs::write(outside.join("docs").join("contract.pdf"), b"IMPORTANT\n").unwrap();

    println!("[{label}] building links:");
    let junction_ok = make_junction(&src.join("linked_docs"), &outside.join("docs"));
    println!("  junction created : {junction_ok}");
    let symlink_ok = try_symlink_file(&src.join("alias.txt"), &src.join("real.txt"));
    println!("  file symlink     : {symlink_ok}");

    // Sanity: the junction really does resolve to the content.
    if junction_ok {
        let through = src.join("linked_docs").join("contract.pdf");
        println!(
            "  content readable through junction: {}",
            fs::read(&through).map(|b| b.len()).unwrap_or(0)
        );
    }

    let zip_path = td.join(format!("out_{label}.zip"));
    let mut a = Archive::create(
        &zip_path,
        CreateOptions {
            format: ArchiveFormat::Zip,
            ..CreateOptions::default()
        },
    )
    .expect("create");
    let add_result = a.add_dir_recursive::<fn(&otterzip_core::Progress) -> bool>(&src, "", None);
    println!("[{label}] add_dir_recursive -> {add_result:?}   <-- the ONLY feedback channel");
    add_result.expect("add_dir_recursive");
    a.commit().expect("commit");

    (archive_entries(&zip_path), junction_ok, symlink_ok)
}

#[test]

#[ignore = "known-failure probe: reproduces an unfixed defect. Run with `cargo test -- --ignored`; delete this attribute when fixed."]
fn junctions_and_symlinks_vanish_from_created_archives_without_warning() {
    let td = tempdir().unwrap();

    // 2 real files + 2 dirs => serial walker.
    let (serial, j1, s1) = build_and_zip(td.path(), 0, "serial");
    // 20 filler files => parallel bulk walker.
    let (parallel, j2, s2) = build_and_zip(td.path(), 20, "parallel");

    println!("\n[serial]   archive contains {} entries:", serial.len());
    for e in &serial {
        println!("    {e}");
    }
    println!("\n[parallel] archive contains {} entries (filler elided)", parallel.len());
    for e in parallel.iter().filter(|e| !e.contains("filler")) {
        println!("    {e}");
    }

    println!("\n================== VERDICT ==================");
    let mut losses: Vec<(&str, bool, bool)> = Vec::new();
    for (label, set, j, s) in [
        ("serial", &serial, j1, s1),
        ("parallel", &parallel, j2, s2),
    ] {
        let has_junction = set.iter().any(|e| e.contains("linked_docs"));
        let has_contract = set.iter().any(|e| e.contains("contract.pdf"));
        let has_alias = set.iter().any(|e| e.contains("alias.txt"));
        println!(
            "{label:9} junction_made={j} -> in archive: linked_docs={has_junction} contract.pdf={has_contract}"
        );
        println!("{label:9} symlink_made={s}  -> in archive: alias.txt={has_alias}");
        losses.push((label, j && !has_junction, s && !has_alias));
    }
    println!("real.txt present (control): {}", serial.contains("real.txt"));
    println!("=============================================");

    assert!(
        j1 && j2,
        "junction creation failed — probe inconclusive on this machine"
    );
    assert!(
        serial.contains("real.txt"),
        "control: ordinary files must still be archived"
    );

    // REGRESSION ASSERT (desired behaviour): design-neutral — whether the
    // writer stores the link, follows it, or skips it is a policy call.
    // What it must NOT do is drop a filesystem object the user pointed at
    // and return `Ok(())` with no way to find out. `add_dir_recursive`
    // returns `Result<()>`, so there is no channel at all; contrast the
    // extract direction, which already has
    // `ExtractWarning::SymlinkSkipped { path, target }`.
    for (label, junction_lost, symlink_lost) in losses {
        assert!(
            !junction_lost,
            "{label}: the junction 'linked_docs' (and the contract.pdf beneath it) \
             is absent from the archive, yet add_dir_recursive returned Ok(()) — \
             there is no report channel to tell the user anything was left out"
        );
        assert!(
            !symlink_lost,
            "{label}: the symlink 'alias.txt' is absent from the archive, yet \
             add_dir_recursive returned Ok(()) with no warning"
        );
    }
}
