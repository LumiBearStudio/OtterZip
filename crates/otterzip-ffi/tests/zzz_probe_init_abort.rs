//! PROBE: can `otterzip_init()` panic across the `extern "C"` boundary?
//!
//! Every other exported symbol funnels through `catch_unwind_to_error`
//! (util.rs documents that as a hard rule). `otterzip_init` does not.
//! Inside it, `tracing_appender::rolling::never()` is
//! `RollingFileAppender::new(...)` which ends in
//! `.expect("initializing rolling file appender failed")` — it panics
//! rather than returning Err when the log file cannot be opened.
//!
//! `install_subscriber` only propagates the `create_dir_all` error;
//! `create_dir_all` succeeds on an *existing* directory without ever
//! checking that the directory is writable, so "dir exists, file open
//! fails" is a live gap.
//!
//! Since rustc 1.71 an unwind out of `extern "C"` is an immediate
//! process abort, so the observation has to happen in a child process.

use std::process::Command;

const CHILD_ENV: &str = "OZ_INIT_PROBE_CHILD";

fn run_child(label: &str, temp: &std::path::Path) -> std::process::Output {
    let exe = std::env::current_exe().expect("current_exe");
    let out = Command::new(exe)
        .args([
            "--exact",
            "init_panics_across_the_c_abi_when_log_file_is_unopenable",
            "--nocapture",
        ])
        .env(CHILD_ENV, "1")
        .env("TEMP", temp)
        .env("TMP", temp)
        .output()
        .expect("spawn child");
    println!("--- child [{label}] ---");
    println!("  status : {:?}  (success={})", out.status, out.status.success());
    println!("  stdout : {}", String::from_utf8_lossy(&out.stdout).trim());
    println!("  stderr : {}", String::from_utf8_lossy(&out.stderr).trim());
    out
}

#[test]

#[ignore = "known-failure probe: reproduces an unfixed defect. Run with `cargo test -- --ignored`; delete this attribute when fixed."]
fn init_panics_across_the_c_abi_when_log_file_is_unopenable() {
    // ---- child role: just call the function under test -------------
    if std::env::var(CHILD_ENV).is_ok() {
        let rc = otterzip_ffi::otterzip_init();
        println!("CHILD-REACHED-END: otterzip_init returned {rc}");
        return;
    }

    // ---- parent role ------------------------------------------------
    // Control: a clean, writable TEMP. Proves the child harness itself
    // works and that init normally returns 0.
    let clean = tempfile::tempdir().unwrap();
    let ok = run_child("writable TEMP (control)", clean.path());

    // Hostile case: %TEMP%\otterzip\otterzip.log already exists as a
    // DIRECTORY. `create_dir_all` is happy; opening the log file is not.
    let hostile = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(hostile.path().join("otterzip").join("otterzip.log")).unwrap();
    let bad = run_child("otterzip.log is a directory", hostile.path());

    // The realistic trigger: the log file already exists and carries the
    // READONLY attribute (backup tool, ACL, sync client, leftover from a
    // copy). No exotic setup — just a file the process cannot append to.
    let ro = tempfile::tempdir().unwrap();
    let ro_log = ro.path().join("otterzip").join("otterzip.log");
    std::fs::create_dir_all(ro_log.parent().unwrap()).unwrap();
    std::fs::write(&ro_log, b"old log\n").unwrap();
    let mut perms = std::fs::metadata(&ro_log).unwrap().permissions();
    #[allow(clippy::permissions_set_readonly_false)]
    perms.set_readonly(true);
    std::fs::set_permissions(&ro_log, perms).unwrap();
    let ro_out = run_child("otterzip.log is read-only", ro.path());

    println!("\n================ SUMMARY ================");
    println!("control exit code       : {:?}", ok.status.code());
    println!("log-is-a-dir exit code  : {:?}", bad.status.code());
    println!("log-is-readonly exit    : {:?}", ro_out.status.code());
    println!("=========================================");

    // REGRESSION ASSERT (desired behaviour): logging is a side effect.
    // Failing to open the log file must degrade to "no log", never kill
    // the host process. `otterzip_init` is the one exported symbol that
    // does not funnel through `catch_unwind_to_error`, which util.rs
    // documents as mandatory for every `extern "C"` fn in this crate.
    assert!(
        ro_out.status.success(),
        "a read-only %TEMP%\\otterzip\\otterzip.log made otterzip_init abort the \
         process (exit {:?}) instead of returning — logging failure must not be fatal",
        ro_out.status.code()
    );

    assert!(
        ok.status.success(),
        "control child should exit cleanly — harness sanity check"
    );
    assert!(
        String::from_utf8_lossy(&ok.stdout).contains("CHILD-REACHED-END"),
        "control child should have returned from otterzip_init"
    );
    assert!(
        bad.status.success(),
        "an unopenable %TEMP%\\otterzip\\otterzip.log made otterzip_init abort the \
         process (exit {:?}) instead of returning",
        bad.status.code()
    );
}
