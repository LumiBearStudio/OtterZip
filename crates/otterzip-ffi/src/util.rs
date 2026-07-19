//! FFI helpers — panic guard, UTF-8 decode, error → code mapping.
//!
//! Every `extern "C"` function in this crate must funnel through
//! [`catch_unwind_to_error`] so a Rust panic never unwinds across the C ABI
//! boundary (UB per the Rustonomicon). See `ffi-api.md` §12.1.

use std::any::Any;
use std::os::raw::c_char;
use std::panic::{catch_unwind, AssertUnwindSafe};

use otterzip_core::OtterzipError;

use crate::error::{set_last_error, ErrorCode};

/// Run `f` inside `catch_unwind`, mapping panics and `OtterzipError` to the
/// appropriate [`ErrorCode`]. The closure returns its own success code on
/// the happy path so callers can distinguish e.g. iterator-end from OK.
pub(crate) fn catch_unwind_to_error<F>(f: F) -> i32
where
    F: FnOnce() -> Result<i32, OtterzipError>,
{
    // AssertUnwindSafe: closures touching FFI-owned pointers don't expose
    // observable broken invariants — on panic we abort the operation and
    // surface a generic error code. UnwindSafe would force the caller to
    // wrap every captured `&mut` for no real safety win at this boundary.
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(Ok(code)) => code,
        Ok(Err(err)) => {
            let code = error_code_for(&err) as i32;
            set_last_error(err.to_string());
            code
        }
        Err(payload) => {
            set_last_error(panic_message(&*payload));
            ErrorCode::Generic as i32
        }
    }
}

fn panic_message(payload: &dyn Any) -> String {
    if let Some(s) = payload.downcast_ref::<&'static str>() {
        format!("panic in FFI: {s}")
    } else if let Some(s) = payload.downcast_ref::<String>() {
        format!("panic in FFI: {s}")
    } else {
        "panic in FFI (non-string payload)".to_string()
    }
}

/// Map a `OtterzipError` to its FFI error code.
pub(crate) fn error_code_for(err: &OtterzipError) -> ErrorCode {
    match err {
        OtterzipError::Io(io) => match io.kind() {
            std::io::ErrorKind::NotFound => ErrorCode::FileNotFound,
            std::io::ErrorKind::PermissionDenied => ErrorCode::PermissionDenied,
            // Stable since 1.83; older toolchains hit the wildcard.
            _ => ErrorCode::Io,
        },
        OtterzipError::InvalidArgument(_) => ErrorCode::InvalidArgument,
        OtterzipError::UnsupportedFormat(_) => ErrorCode::UnsupportedFormat,
        OtterzipError::Corrupted { .. } => ErrorCode::CorruptedArchive,
        OtterzipError::WrongPassword => ErrorCode::WrongPassword,
        OtterzipError::MissingVolume { .. } => ErrorCode::MissingVolume,
        OtterzipError::Canceled => ErrorCode::OperationCanceled,
        OtterzipError::FeatureDisabled(_) => ErrorCode::FeatureDisabled,
        OtterzipError::EntryNotFound(_) => ErrorCode::EntryNotFound,
        OtterzipError::PathTraversalBlocked(_) => ErrorCode::PathTraversal,
        OtterzipError::ZipBombSuspected { .. } => ErrorCode::ZipBomb,
        OtterzipError::BackendError(_) => ErrorCode::BackendError,
    }
}

/// Read a `(ptr, len)` pair as UTF-8. `len == 0` is a valid empty string
/// only when `ptr` is non-null; a null pointer is always rejected.
///
/// # Safety
/// Caller must ensure `ptr` points to at least `len` bytes of readable
/// memory and that the bytes outlive the returned slice.
pub(crate) unsafe fn read_utf8<'a>(
    ptr: *const c_char,
    len: usize,
) -> Result<&'a str, OtterzipError> {
    if ptr.is_null() {
        return Err(OtterzipError::InvalidArgument("null string pointer"));
    }
    // SAFETY: caller contract — `ptr` references `len` valid bytes.
    let bytes = unsafe { std::slice::from_raw_parts(ptr.cast::<u8>(), len) };
    std::str::from_utf8(bytes)
        .map_err(|_| OtterzipError::InvalidArgument("string is not valid UTF-8"))
}

/// Variant that accepts NULL → `None` (for optional strings like password).
///
/// # Safety
/// Same contract as [`read_utf8`] when `ptr` is non-null.
pub(crate) unsafe fn read_optional_utf8<'a>(
    ptr: *const c_char,
    len: usize,
) -> Result<Option<&'a str>, OtterzipError> {
    if ptr.is_null() {
        return Ok(None);
    }
    // SAFETY: forwarded.
    unsafe { read_utf8(ptr, len).map(Some) }
}

// =====================================================================
// PROBE (zzz, temporary): is ErrorCode::DiskFull (-13) reachable?
// =====================================================================
// `error_code_for` is pub(crate), so an integration test cannot reach
// it; this has to live next to the function.
#[cfg(test)]
mod zzz_probe_disk_full {
    use super::error_code_for;
    use crate::error::ErrorCode;
    use otterzip_core::OtterzipError;

    /// Win32 ERROR_DISK_FULL / ERROR_HANDLE_DISK_FULL. These are the two
    /// codes NTFS returns when a write cannot be satisfied for space.
    const ERROR_DISK_FULL: i32 = 112;
    const ERROR_HANDLE_DISK_FULL: i32 = 39;

    #[test]

    #[ignore = "known-failure probe: reproduces an unfixed defect. Run with `cargo test -- --ignored`; delete this attribute when fixed."]
    fn disk_full_io_errors_do_not_map_to_disk_full_code() {
        for (name, raw) in [
            ("ERROR_DISK_FULL", ERROR_DISK_FULL),
            ("ERROR_HANDLE_DISK_FULL", ERROR_HANDLE_DISK_FULL),
        ] {
            let io = std::io::Error::from_raw_os_error(raw);
            let kind = io.kind();
            let code = error_code_for(&OtterzipError::Io(io)) as i32;
            println!(
                "{name} (os {raw}) -> ErrorKind::{kind:?} -> FFI code {code} \
                 (DiskFull would be {})",
                ErrorCode::DiskFull as i32
            );
            // REGRESSION ASSERT (desired behaviour): ErrorCode::DiskFull
            // exists, the UI ships an `Error_DiskFull` string in ten
            // locales, and nothing can ever produce the code. Storage
            // exhaustion must be distinguishable from generic IO.
            assert_eq!(
                code,
                ErrorCode::DiskFull as i32,
                "{name} ({kind:?}) mapped to {code} (generic Io) instead of \
                 DiskFull ({}) — the C# layer turns every unmapped code into \
                 \"Can't process this archive (corrupted or unsupported)\"",
                ErrorCode::DiskFull as i32
            );
        }

        // Contrast: the two kinds that ARE handled map correctly, so the
        // gap is specific to storage exhaustion, not a broken function.
        let nf = error_code_for(&OtterzipError::Io(std::io::Error::from_raw_os_error(2))) as i32;
        let pd = error_code_for(&OtterzipError::Io(std::io::Error::from_raw_os_error(5))) as i32;
        println!("ERROR_FILE_NOT_FOUND (os 2) -> FFI code {nf} (FileNotFound = {})",
            ErrorCode::FileNotFound as i32);
        println!("ERROR_ACCESS_DENIED  (os 5) -> FFI code {pd} (PermissionDenied = {})",
            ErrorCode::PermissionDenied as i32);
        assert_eq!(nf, ErrorCode::FileNotFound as i32);
        assert_eq!(pd, ErrorCode::PermissionDenied as i32);
    }
}
