//! Lenient ZIP backend — custom EOCD / Central Directory parser that
//! salvages malformed archives the strict `zip` crate refuses.
//!
//! Replaces the libarchive (compress-tools) fallback retired at the
//! v1.0 sprint. No C dependency, no vcpkg, all pure Rust over
//! `flate2` (zlib-ng) — the read path lifts the actual byte
//! decompression onto a decoder that's been fuzz-hardened by its
//! upstream for 20+ years, so we never own deflate correctness
//! ourselves. Parser bugs surface as `Err` (and Sentry catches them);
//! decoder bugs would corrupt user data silently, which is exactly
//! the failure mode we won't take.
//!
//! ## What this module owns
//!
//!   * EOCD location, ZIP64 escalation via the locator → record chain.
//!   * Central directory walk: one [`Entry`] per CDFH, lenient resync
//!     to the next `PK\001\002` when an individual record's
//!     filename / extra length is bogus.
//!   * Per-entry LFH parse + decompression dispatch.
//!
//! Supported methods on the read path:
//!
//! | Method | Decoder | Notes |
//! |---|---|---|
//! | 0 (Stored) | direct copy via `std::io::copy` | `compressed_size == uncompressed_size`. |
//! | 8 (Deflate) | `flate2::read::DeflateDecoder` | zlib-ng backed; streaming, no size hint needed. |
//!
//! Other CDFH methods (BZip2 / LZMA / Zstd / Deflate64 / PPMd / AES)
//! surface [`OtterzipError::FeatureDisabled`] — those land in a v1.1
//! sprint. The strict `ZipBackend` already covers the healthy
//! archive case for the codec-side of those methods today.
//!
//! ## Lenient policy
//!
//! Mirrors what Bandizip / 7-Zip do on the same fixtures:
//!
//!   * Clamp CD bounds to the file size when the EOCD overshoots.
//!   * Scan forward from offset 0 for `PK\001\002` when `cd_offset`
//!     itself is past EOF or doesn't point at a CDFH.
//!   * Resync to the next `PK\001\002` signature inside the CD when
//!     a CDFH's filename / extra length doesn't sum to a sensible
//!     next-record offset.
//!   * Mark entries whose `local_header_offset` lands past EOF as
//!     unreadable (`u64::MAX` sentinel) and surface them as per-entry
//!     `Corrupted` at extract time — the rest of the archive still
//!     comes through.

use std::cell::RefCell;
use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rayon::prelude::*;
use zeroize::Zeroizing;

use crate::archive::ExtractWarning;
use crate::backends::{ArchiveBackend, StreamingExtractCtx};
use crate::entry::{Entry, HostOs};
use crate::error::{OtterzipError, Result};
use crate::format::{CompressionMethod, EncryptionMethod};
use crate::options::OverwritePolicy;
use crate::progress::{Progress, ProgressPhase};

// === Format constants ================================================

/// End-of-central-directory record signature ("PK\005\006").
const SIG_EOCD: [u8; 4] = [0x50, 0x4b, 0x05, 0x06];
/// ZIP64 end-of-central-directory locator signature ("PK\006\007").
const SIG_ZIP64_LOCATOR: [u8; 4] = [0x50, 0x4b, 0x06, 0x07];
/// ZIP64 end-of-central-directory record signature ("PK\006\006").
const SIG_ZIP64_EOCD: [u8; 4] = [0x50, 0x4b, 0x06, 0x06];
/// Central-directory file header signature ("PK\001\002").
const SIG_CDFH: [u8; 4] = [0x50, 0x4b, 0x01, 0x02];
/// Local file header signature ("PK\003\004") — sits before every
/// entry's payload. Read by [`locate_payload`].
const SIG_LFH: [u8; 4] = [0x50, 0x4b, 0x03, 0x04];

/// Fixed-size portion of an LFH (filename + extra follow).
const LFH_FIXED_SIZE: usize = 30;

/// Cap on the inflated bytes a single `extract_entry` call will write
/// when the CDFH-declared `uncompressed_size` is missing or
/// untrustworthy (a malformation pattern fuzz corpora love). 4 GiB
/// matches the practical upper bound of a single Deflate stream and
/// keeps a hostile entry from running away with all of RAM; legitimate
/// large entries should always have a populated CDFH size and bypass
/// this clamp.
const STREAMING_INFLATE_HARD_CAP: u64 = 4 * 1024 * 1024 * 1024;

/// Threshold below which a deflated entry takes the libdeflater
/// one-shot decoder path. Picked so the input buffer (worst case
/// the entry's compressed size) and the output buffer
/// (`uncompressed_size`, known up-front from the CDFH) both fit
/// inside ~32 MiB of RAM per concurrent worker — comfortable for
/// 4-worker parallel extracts on the smallest machines we target.
/// Entries above this size fall back to the `flate2` streaming
/// decoder so we never allocate gigabytes for a single deflate
/// payload.
///
/// libdeflater is ~20 % faster than flate2 (zlib-ng) on the
/// hot-loop deflate path because it skips the streaming
/// state-machine bookkeeping and operates directly on contiguous
/// input/output buffers. Real-world archives we measure (~750 KB
/// average entry on the user's 7 GB corpus) sit comfortably under
/// this threshold, so the lenient extract spends most of its CPU
/// in the fast decoder.
const LIBDEFLATER_ONESHOT_THRESHOLD: u64 = 16 * 1024 * 1024;

/// Fixed-size portion of an EOCD record (variable-length comment follows).
const EOCD_FIXED_SIZE: usize = 22;
/// ZIP64 EOCD locator — always exactly 20 bytes.
const ZIP64_LOCATOR_SIZE: usize = 20;
/// Minimum size of a ZIP64 EOCD record (extensible "v2" records grow it).
const ZIP64_EOCD_FIXED_SIZE: usize = 56;
/// Fixed-size portion of a CDFH (filename / extra / comment follow).
const CDFH_FIXED_SIZE: usize = 46;

/// EOCD comment field is bounded to 65 535 bytes by APPNOTE.TXT §4.3.16,
/// plus the 22-byte fixed record = 65 557. Bandizip / 7-Zip cap their
/// EOCD search at the same value; matching them avoids "you find it but
/// we don't" complaints on archives with maxed-out tails.
const MAX_EOCD_TAIL_SCAN: u64 = 65_557;

/// ZIP64 extra field tag (APPNOTE.TXT §4.5.3).
const EXTRA_TAG_ZIP64: u16 = 0x0001;

// === Backend ==========================================================

/// Per-entry metadata captured from the central directory. The
/// `entry` field is what gets handed out via the public
/// [`ArchiveBackend::entries`] surface; the rest is sidecar carried
/// for Day 2's per-entry payload reads (the LFH offset + the raw
/// CDFH method / GP-flag bits — enough to dispatch without
/// re-parsing the CD).
///
/// Cheaply cloneable: every field is fixed-width or already-`Clone`
/// (`Entry`'s strings are heap-allocated but typically short). The
/// extract path clones one record per `extract_entry` call so the
/// `RefCell<Vec<CdRecord>>` borrow can be released before we take a
/// mutable borrow of the inner reader.
#[derive(Clone)]
struct CdRecord {
    entry: Entry,
    /// Absolute byte offset of this entry's local file header. May be
    /// `u64::MAX` to mean "lenient walk decided this entry is
    /// unrecoverable" — Day 2's extract path will surface those as
    /// per-entry errors instead of failing the whole archive.
    lfh_offset: u64,
    /// Compression method straight from the CDFH (APPNOTE.TXT §4.4.5).
    /// [`decompress`] dispatches on this to pick `flate2` (today) and
    /// will fan out to `bzip2` / `lzma` / `zstd` once those land.
    raw_method: u16,
    /// Raw GP bit flag. Encryption (bit 0) is already projected into
    /// `entry.encryption`, but bit 3 (data descriptor — sizes/CRC live
    /// after the compressed payload instead of in the LFH) and bit 11
    /// (EFS / UTF-8 names) are needed by the per-entry read path
    /// when we tighten LFH parsing. Kept until that lands.
    #[allow(dead_code)]
    raw_gpf: u16,
}

/// Lenient ZIP backend. Holds the source path, the materialised CD
/// records, and a single buffered file handle reused across per-entry
/// reads. The trait surface is `&self`, so the handle lives behind a
/// `RefCell` — matches the strict `ZipBackend`'s borrow discipline and
/// is sound because the public `Archive` type is `!Sync` per
/// `rust-core-api.md` §5.
pub(crate) struct LenientZipBackend {
    /// Original archive path. Used by the parallel-extract
    /// [`ArchiveBackend::extract_all_streaming`] override (each
    /// rayon worker re-opens the file on its own handle so per-entry
    /// I/O doesn't serialise on a single `RefCell<File>`).
    source_path: PathBuf,
    /// Held in zeroized memory so the bytes are wiped on drop. Reserved
    /// for the encrypted-entry dispatch; Day 2 doesn't actually use a
    /// password (GP bit 0 entries still surface `FeatureDisabled`).
    _password: Option<Zeroizing<String>>,
    /// CD records produced by [`walk_central_directory`].
    records: RefCell<Vec<CdRecord>>,
    /// Single buffered reader shared across `extract_entry` /
    /// `open_entry_stream` calls. `BufReader` coalesces the LFH probe
    /// (30 bytes) + filename + extra + payload reads into ~64 KiB
    /// syscalls, matching the strict path's I/O profile.
    inner: RefCell<BufReader<File>>,
    /// Captured at open for diagnostic + per-entry LFH sanity checks.
    file_size: u64,
}

impl LenientZipBackend {
    /// Open `path` and produce a backend with every CDFH already parsed.
    /// Returns [`OtterzipError::Corrupted`] when even the lenient walk
    /// can't find an EOCD or any CDFH at all — the caller's dispatcher
    /// has nothing left to fall back to in that case.
    pub(crate) fn open(path: &Path, password: Option<&Zeroizing<String>>) -> Result<Self> {
        let started = std::time::Instant::now();
        let file = File::open(path)?;
        let file_size = file.metadata()?.len();
        let mut reader = BufReader::with_capacity(64 * 1024, file);

        let cdr = find_central_directory(&mut reader, file_size)?;
        tracing::info!(
            target: "otterzip::lenient",
            path = %path.display(),
            cd_offset = cdr.cd_offset,
            cd_size = cdr.cd_size,
            declared_entries = cdr.total_entries,
            elapsed_us = started.elapsed().as_micros() as u64,
            "lenient: located central directory"
        );

        let records = walk_central_directory(&mut reader, &cdr, file_size)?;
        tracing::info!(
            target: "otterzip::lenient",
            path = %path.display(),
            recovered_entries = records.len(),
            declared_entries = cdr.total_entries,
            elapsed_ms = started.elapsed().as_millis() as u64,
            "lenient: central directory walk complete"
        );

        Ok(Self {
            source_path: path.to_path_buf(),
            _password: password.cloned(),
            records: RefCell::new(records),
            inner: RefCell::new(reader),
            file_size,
        })
    }

    /// Locate the cached record for `entry_path`. Linear scan is fine
    /// here — typical archives top out around 10–100 K entries, an
    /// `O(n)` lookup is sub-millisecond even at that scale and avoids
    /// a parallel HashMap that would need its own lifetime management.
    fn find_record(&self, entry_path: &str) -> Result<CdRecord> {
        self.records
            .borrow()
            .iter()
            .find(|r| r.entry.path == entry_path)
            .cloned()
            .ok_or_else(|| OtterzipError::EntryNotFound(entry_path.to_string()))
    }

    /// rayon-driven entry-level parallel extractor — mirrors
    /// `ZipBackend::extract_all_parallel` so the lenient path enjoys
    /// the same NVMe-saturation profile on the user's 7 GB / 9 674
    /// entry malformed corpus. Each worker re-opens the archive
    /// through [`LenientZipBackend::open`] on its own file descriptor;
    /// the per-thread CD-walk cost (~ms on a typical archive) is
    /// amortised across the entries the worker takes.
    ///
    /// Security gates wired inline because the parallel path can't
    /// reuse the framework's serial monitor state (per-thread
    /// counters wouldn't compose):
    ///
    ///   * Per-entry `__check_bomb_for_streaming` ratio gate.
    ///   * Cumulative output cap via an atomic counter.
    ///   * `__validate_component` per path component.
    ///   * Overwrite policy + symlink follow check.
    ///   * MOTW propagation per output file.
    fn extract_all_parallel(&self, ctx: &mut StreamingExtractCtx<'_>) -> Result<()> {
        let dest_root = ctx.dest_root.to_path_buf();
        let opts = ctx.opts.clone();
        let start = ctx.start;
        let motw_payload: Option<std::sync::Arc<Vec<u8>>> =
            ctx.motw_payload.map(|p| std::sync::Arc::new(p.to_vec()));
        let source_path = self.source_path.clone();
        let password = self._password.clone();

        // Snapshot entry metadata for the worker pool — releasing
        // `self.records`'s RefCell borrow before we cross into rayon.
        let metas: Vec<Entry> = self
            .records
            .borrow()
            .iter()
            .map(|r| r.entry.clone())
            .collect();
        let total_bytes: u64 = metas.iter().map(|m| m.uncompressed_size).sum();
        let total_entries = u32::try_from(metas.len()).unwrap_or(u32::MAX);
        tracing::info!(
            target: "otterzip::extract",
            entries = total_entries,
            total_uncompressed = total_bytes,
            cd_parse_ms = start.elapsed().as_millis() as u64,
            "lenient parallel extract metadata phase done"
        );

        let bytes_done = std::sync::atomic::AtomicU64::new(0);
        let entries_done = std::sync::atomic::AtomicU32::new(0);
        let entries_skipped = std::sync::atomic::AtomicU32::new(0);
        let canceled = std::sync::atomic::AtomicBool::new(false);
        let first_err: Mutex<Option<OtterzipError>> = Mutex::new(None);
        let warnings: Mutex<Vec<ExtractWarning>> = Mutex::new(Vec::new());
        let progress_lock: Mutex<&mut dyn crate::progress::ProgressSink> = Mutex::new(ctx.progress);

        // Same 4-worker NTFS cap the strict backend uses — beyond
        // that, MFT lock contention on file-creation-heavy archives
        // costs us more than the extra parallelism buys.
        let source_for_init = source_path.clone();
        let password_for_init = password.clone();
        let worker_count = std::cmp::min(rayon::current_num_threads(), 4);
        let worker_pool = rayon::ThreadPoolBuilder::new()
            .num_threads(worker_count)
            .build()
            .map_err(|e| {
                OtterzipError::BackendError(format!("rayon thread-pool build failed: {e}"))
            })?;
        let dispatch_start = std::time::Instant::now();
        tracing::info!(
            target: "otterzip::extract",
            workers = worker_count,
            "lenient parallel extract dispatching to rayon"
        );

        worker_pool.install(|| {
            metas.par_iter().for_each_init(
                || open_local(&source_for_init, password_for_init.as_ref()).ok(),
                |local_handle, entry| {
                    if canceled.load(std::sync::atomic::Ordering::Relaxed) {
                        return;
                    }

                    if let Some(err) = crate::archive::__check_bomb_for_streaming(entry, &opts) {
                        set_first_err(&first_err, &canceled, err);
                        return;
                    }

                    if entry.is_symlink && !opts.follow_symlinks {
                        warnings.lock().unwrap().push(ExtractWarning::SymlinkSkipped {
                            path: entry.path.clone(),
                            target: String::new(),
                        });
                        entries_skipped.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        return;
                    }

                    let out_path = match resolve_output_path(
                        &dest_root,
                        &entry.path,
                        &opts,
                        entry.is_directory,
                    ) {
                        Ok(p) => p,
                        Err(orig) => {
                            if opts.block_path_traversal {
                                set_first_err(
                                    &first_err,
                                    &canceled,
                                    OtterzipError::PathTraversalBlocked(orig),
                                );
                            } else {
                                warnings.lock().unwrap().push(
                                    ExtractWarning::PathTraversalClamped {
                                        original: orig,
                                        clamped: dest_root.clone(),
                                    },
                                );
                                entries_skipped
                                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                            }
                            return;
                        }
                    };

                    if entry.is_directory {
                        if let Err(e) = std::fs::create_dir_all(&out_path) {
                            set_first_err(&first_err, &canceled, OtterzipError::Io(e));
                            return;
                        }
                        entries_done.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        return;
                    }

                    if let Some(parent) = out_path.parent() {
                        if let Err(e) = std::fs::create_dir_all(parent) {
                            set_first_err(&first_err, &canceled, OtterzipError::Io(e));
                            return;
                        }
                    }

                    let out_path = if out_path.exists() {
                        match opts.overwrite {
                            OverwritePolicy::Never => {
                                set_first_err(
                                    &first_err,
                                    &canceled,
                                    OtterzipError::Io(std::io::Error::new(
                                        std::io::ErrorKind::AlreadyExists,
                                        out_path.display().to_string(),
                                    )),
                                );
                                return;
                            }
                            OverwritePolicy::Always => out_path,
                            // Reserve the keep-both name atomically (create_new)
                            // so concurrent workers can't truncate each other.
                            OverwritePolicy::Rename => {
                                match crate::archive::reserve_unique_extract_path(&out_path) {
                                    Ok(p) => p,
                                    Err(e) => {
                                        set_first_err(
                                            &first_err,
                                            &canceled,
                                            OtterzipError::Io(e),
                                        );
                                        return;
                                    }
                                }
                            }
                            OverwritePolicy::IfNewer | OverwritePolicy::AskCallback => {
                                entries_skipped
                                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                                return;
                            }
                        }
                    } else {
                        out_path
                    };

                    let local_backend = match local_handle.as_mut() {
                        Some(b) => b,
                        None => {
                            set_first_err(
                                &first_err,
                                &canceled,
                                OtterzipError::BackendError(
                                    "lenient worker failed to open archive handle".into(),
                                ),
                            );
                            return;
                        }
                    };

                    let file = match std::fs::File::create(&out_path) {
                        Ok(f) => f,
                        Err(e) => {
                            set_first_err(&first_err, &canceled, OtterzipError::Io(e));
                            return;
                        }
                    };
                    let mut writer = BufWriter::new(file);
                    // Enforce the absolute output cap DURING the write, sharing
                    // the running total across workers (the parallel path used
                    // to check only AFTER a whole entry was on disk).
                    let written = {
                        let mut capped = crate::archive::__AtomicCappedWriter {
                            inner: &mut writer,
                            written_total: &bytes_done,
                            cap: opts.max_total_output_bytes,
                            tripped: false,
                        };
                        match local_backend.extract_entry(&entry.path, &mut capped) {
                            Ok(n) => n,
                            Err(_) if capped.tripped => {
                                set_first_err(
                                    &first_err,
                                    &canceled,
                                    OtterzipError::ZipBombSuspected {
                                        entry: entry.path.clone(),
                                        ratio: 0,
                                        limit: opts.max_total_compression_ratio,
                                    },
                                );
                                return;
                            }
                            Err(e) => {
                                set_first_err(&first_err, &canceled, e);
                                return;
                            }
                        }
                    };
                    if let Err(e) = writer.flush() {
                        set_first_err(&first_err, &canceled, OtterzipError::Io(e));
                        return;
                    }

                    if let Some(p) = motw_payload.as_ref() {
                        if let Err(e) = crate::motw::write_zone_identifier(&out_path, &p[..]) {
                            tracing::warn!(
                                target: "otterzip::motw",
                                path = %out_path.display(),
                                error = %e,
                                "MOTW propagation skipped (parallel lenient extract)"
                            );
                        }
                    }

                    // `bytes_done` already advanced in-flight via the capped
                    // writer above — do NOT add `written` again (double count).
                    let _ = written;
                    let entries_so_far =
                        entries_done.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;

                    if entries_so_far % 8 == 0 || entries_so_far == total_entries {
                        if let Ok(mut sink) = progress_lock.try_lock() {
                            let snapshot = Progress {
                                bytes_processed: bytes_done
                                    .load(std::sync::atomic::Ordering::Relaxed),
                                bytes_total: total_bytes,
                                entries_processed: entries_so_far,
                                entries_total: total_entries,
                                current_entry: Some(entry.path.clone()),
                                phase: ProgressPhase::Writing,
                                elapsed: start.elapsed(),
                                current_entry_bytes_processed: 0,
                                current_entry_bytes_total: 0,
                            };
                            if !sink.update(&snapshot) {
                                canceled.store(true, std::sync::atomic::Ordering::Relaxed);
                            }
                        }
                    }
                },
            );
        });
        tracing::info!(
            target: "otterzip::extract",
            dispatch_ms = dispatch_start.elapsed().as_millis() as u64,
            entries_done = entries_done.load(std::sync::atomic::Ordering::Relaxed),
            entries_skipped = entries_skipped.load(std::sync::atomic::Ordering::Relaxed),
            bytes_done = bytes_done.load(std::sync::atomic::Ordering::Relaxed),
            canceled = canceled.load(std::sync::atomic::Ordering::Relaxed),
            "lenient parallel extract rayon pool returned"
        );

        if let Some(err) = first_err.into_inner().unwrap() {
            tracing::warn!(
                target: "otterzip::extract",
                error = %err,
                "lenient parallel extract first_err set"
            );
            return Err(err);
        }
        if canceled.load(std::sync::atomic::Ordering::Relaxed) {
            return Err(OtterzipError::Canceled);
        }

        ctx.report.bytes_written += bytes_done.load(std::sync::atomic::Ordering::Relaxed);
        ctx.report.entries_extracted += entries_done.load(std::sync::atomic::Ordering::Relaxed);
        ctx.report.entries_skipped += entries_skipped.load(std::sync::atomic::Ordering::Relaxed);
        ctx.report
            .warnings
            .extend(warnings.into_inner().unwrap());
        Ok(())
    }
}

/// Per-worker re-open of the archive. Returns a fresh backend
/// whose [`extract_entry`](LenientZipBackend::extract_entry) inherits
/// the same CD-walk recovery as the primary handle. Cheap on
/// typical archives (~ms) and amortised across the entries the
/// worker takes thanks to `for_each_init`.
fn open_local(path: &Path, password: Option<&Zeroizing<String>>) -> Result<LenientZipBackend> {
    LenientZipBackend::open(path, password)
}

/// Captured-first error sink shared across rayon workers. Once any
/// worker sets the slot the cancel flag fires and remaining workers
/// short-circuit. Identical pattern to
/// `ZipBackend::extract_all_parallel`.
fn set_first_err(
    slot: &Mutex<Option<OtterzipError>>,
    canceled: &std::sync::atomic::AtomicBool,
    err: OtterzipError,
) {
    let mut guard = slot.lock().unwrap();
    if guard.is_none() {
        *guard = Some(err);
    }
    canceled.store(true, std::sync::atomic::Ordering::Relaxed);
}

// The lenient parser tolerates malformed ZIP *structure*; it must not relax the
// output-path *security* gate. This private copy claimed to mirror the strict
// backend "byte-for-byte" but had drifted: its flatten branch skipped
// `__validate_component` entirely (NTFS ADS / reserved names passed through) and
// fell back to the raw entry_path when `file_name()` was None (a bare `..`
// escaped dest_root — the FFI-M2 hole). Route through the one shared resolver so
// there is a single traversal ruleset for every backend.
use crate::archive::__resolve_output_path_streaming as resolve_output_path;

impl ArchiveBackend for LenientZipBackend {
    fn entries(&self) -> Result<Box<dyn Iterator<Item = Result<Entry>> + '_>> {
        // Materialise an owned snapshot so the returned iterator does
        // not borrow `self.records`; the strict backend does the same
        // for the same reason (see `backends/zip.rs::entries`).
        let cloned: Vec<Result<Entry>> = self
            .records
            .borrow()
            .iter()
            .map(|r| Ok(r.entry.clone()))
            .collect();
        Ok(Box::new(cloned.into_iter()))
    }

    fn extract_entry(&self, entry_path: &str, out: &mut dyn std::io::Write) -> Result<u64> {
        let record = self.find_record(entry_path)?;
        if record.entry.is_directory {
            // Directory entries carry no payload; the caller's
            // `extract_all` already created the dir before reaching us.
            return Ok(0);
        }
        if record.lfh_offset == u64::MAX {
            return Err(OtterzipError::Corrupted {
                reason: "entry marked unreadable during lenient CD walk".into(),
                entry: Some(entry_path.to_string()),
            });
        }
        if record.entry.encryption != EncryptionMethod::None {
            // GP bit 0 was set. Encrypted-entry dispatch (ZipCrypto +
            // WinZip AES) is a follow-up sprint — surface a clean
            // signal so the strict path's password prompt UX is the
            // only thing missing rather than confusing CRC errors.
            return Err(OtterzipError::FeatureDisabled(
                "lenient ZIP: encrypted entries (v1.1 — AES + ZipCrypto)",
            ));
        }
        let mut reader = self.inner.borrow_mut();
        let payload_offset = locate_payload(&mut *reader, &record, self.file_size)?;
        reader.seek(SeekFrom::Start(payload_offset))?;
        let limited = (&mut *reader).take(record.entry.compressed_size);
        decompress(
            record.raw_method,
            limited,
            record.entry.compressed_size,
            record.entry.uncompressed_size,
            entry_path,
            out,
        )
    }

    fn open_entry_stream(&self, entry_path: &str) -> Result<Box<dyn Read + Send + '_>> {
        // Eager-copy into an in-memory buffer so the returned Read
        // doesn't carry a `RefMut` borrow into the inner reader
        // (which would also fail the `Send` bound the trait wants).
        // Matches the strict `ZipBackend::open_entry_stream` shape;
        // large entries pay the same memory cost there. A real
        // streaming path needs the trait contract to grow a
        // generator-style yield, which is out of scope for Day 2.
        let mut buf: Vec<u8> = Vec::new();
        let _ = self.extract_entry(entry_path, &mut buf)?;
        Ok(Box::new(std::io::Cursor::new(buf)))
    }

    fn is_encrypted_fast(&self) -> Result<bool> {
        // Cheap: just walk the cached records. No LFH scan, no
        // password trial — match the strict backend's behaviour.
        Ok(self
            .records
            .borrow()
            .iter()
            .any(|r| r.entry.encryption != EncryptionMethod::None))
    }

    fn extract_all_streaming(
        &self,
        ctx: &mut StreamingExtractCtx<'_>,
    ) -> Option<Result<()>> {
        // Mirrors the strict `ZipBackend::extract_all_streaming` threshold
        // policy. Tiny archives (very few entries / under a few MiB of
        // total payload / sub-32 KiB average compressed size on small
        // corpora) stay on the framework's serial loop because the
        // per-thread `LenientZipBackend::open` overhead (CD scan + walk)
        // doesn't amortise. Real-world workloads — the 7 GB / 9 674
        // entry case the user actually ships — sail past every gate
        // and end up on the parallel path.
        let records = self.records.borrow();
        let entries = records.len();
        if entries == 0 {
            return None;
        }
        let total_compressed: u64 = records.iter().map(|r| r.entry.compressed_size).sum();
        drop(records);

        const PARALLEL_MIN_BYTES: u64 = 4 * 1024 * 1024;
        const PARALLEL_MIN_ENTRIES: usize = 8;
        const PARALLEL_MIN_AVG_ENTRY_BYTES: u64 = 32 * 1024;
        const PARALLEL_LARGE_ARCHIVE_BYTES: u64 = 50 * 1024 * 1024;
        let avg = if entries == 0 {
            0
        } else {
            total_compressed / entries as u64
        };
        let large_enough_to_skip_avg_check = total_compressed >= PARALLEL_LARGE_ARCHIVE_BYTES;
        let go_serial = entries < PARALLEL_MIN_ENTRIES
            || total_compressed < PARALLEL_MIN_BYTES
            || (!large_enough_to_skip_avg_check && avg < PARALLEL_MIN_AVG_ENTRY_BYTES);
        tracing::info!(
            target: "otterzip::extract",
            entries,
            total_compressed,
            avg_entry = avg,
            large = large_enough_to_skip_avg_check,
            decision = if go_serial { "serial" } else { "parallel" },
            "lenient extract_all_streaming threshold decision"
        );
        if go_serial {
            return None;
        }
        Some(self.extract_all_parallel(ctx))
    }
}

// === EOCD / ZIP64 location ===========================================

/// Resolved central-directory geometry produced by [`find_central_directory`].
/// `cd_size` may have been clamped lenient-style — the value is what we
/// will actually read, not the declared one.
struct CdResolved {
    cd_offset: u64,
    cd_size: u64,
    total_entries: u64,
}

/// Locate the EOCD (escalating to ZIP64 when sentinels demand it) and
/// derive the central-directory geometry. Lenient: out-of-range
/// `cd_size` is clamped to the gap between `cd_offset` and the EOCD
/// signature rather than rejected — most real-world malformations of
/// this kind have a few extra bytes claimed past EOF and the actual CD
/// itself is intact.
fn find_central_directory<R: Read + Seek>(
    reader: &mut R,
    file_size: u64,
) -> Result<CdResolved> {
    if file_size < EOCD_FIXED_SIZE as u64 {
        return Err(OtterzipError::Corrupted {
            reason: format!("file too small ({file_size} bytes) for EOCD"),
            entry: None,
        });
    }

    // Read the last (up to) 65 557 bytes and scan backwards for the
    // EOCD signature. APPNOTE caps the comment at 65 535 bytes; we add
    // the 22-byte fixed record for the upper bound.
    let tail_len = file_size.min(MAX_EOCD_TAIL_SCAN);
    let tail_start = file_size - tail_len;
    reader.seek(SeekFrom::Start(tail_start))?;
    let mut tail = vec![0u8; tail_len as usize];
    reader.read_exact(&mut tail)?;

    let eocd_in_tail = tail
        .windows(4)
        .rposition(|w| w == SIG_EOCD)
        .ok_or_else(|| OtterzipError::Corrupted {
            reason: format!(
                "EOCD signature not found in last {tail_len} bytes",
            ),
            entry: None,
        })?;
    if eocd_in_tail + EOCD_FIXED_SIZE > tail.len() {
        return Err(OtterzipError::Corrupted {
            reason: "EOCD record truncated".into(),
            entry: None,
        });
    }
    let eocd_abs = tail_start + eocd_in_tail as u64;
    let eocd = &tail[eocd_in_tail..eocd_in_tail + EOCD_FIXED_SIZE];
    let total_entries_16 = u16::from_le_bytes([eocd[10], eocd[11]]);
    let cd_size_32 = u32::from_le_bytes([eocd[12], eocd[13], eocd[14], eocd[15]]);
    let cd_offset_32 = u32::from_le_bytes([eocd[16], eocd[17], eocd[18], eocd[19]]);

    let needs_zip64 = cd_offset_32 == u32::MAX
        || cd_size_32 == u32::MAX
        || total_entries_16 == u16::MAX;
    if needs_zip64 {
        return resolve_zip64(
            reader,
            &tail,
            tail_start,
            eocd_in_tail,
            eocd_abs,
            file_size,
            ResolvedFallback {
                cd_offset_32,
                total_entries_16,
            },
        );
    }

    let declared_offset = u64::from(cd_offset_32);
    let declared_size = u64::from(cd_size_32);

    // Validate the declared offset. Two failure modes the user actually
    // ships archives for:
    //   1. `cd_offset >= eocd_abs` — pointer past the EOCD (the
    //      common "off-by-N ZIP64 cd_size arithmetic" malformation).
    //   2. `cd_offset` lands inside the file but the bytes there
    //      aren't a CDFH signature — pointer fell into the LFH region
    //      or random padding.
    // In both cases the actual central directory is still intact in
    // the file; we just need to find where it really starts. Scan
    // forward from the start of the file for the first
    // `PK\001\002` and use that as the CD origin.
    let (cd_offset, cd_size) = if declared_offset >= eocd_abs
        || !peek_is_cdfh(reader, declared_offset)?
    {
        let recovered = scan_for_cd_start(reader, eocd_abs)?;
        tracing::warn!(
            target: "otterzip::lenient",
            declared_offset,
            declared_size,
            recovered,
            "lenient: declared CD offset is bogus — using scanned CD start"
        );
        (recovered, eocd_abs - recovered)
    } else {
        // Happy(ish) path: declared offset points at a real CDFH.
        // Still need to clamp the size so the walk doesn't run past
        // the EOCD signature.
        let max_possible = eocd_abs - declared_offset;
        let size = if declared_size == 0 {
            tracing::warn!(
                target: "otterzip::lenient",
                "lenient: EOCD cd_size is zero — using gap to EOCD ({max_possible} bytes)"
            );
            max_possible
        } else if declared_size > max_possible {
            tracing::warn!(
                target: "otterzip::lenient",
                declared = declared_size,
                clamped = max_possible,
                "lenient: clamped CD size to fit between cd_offset and EOCD"
            );
            max_possible
        } else {
            declared_size
        };
        (declared_offset, size)
    };

    Ok(CdResolved {
        cd_offset,
        cd_size,
        total_entries: u64::from(total_entries_16),
    })
}

/// Peek 4 bytes at `offset` and confirm they spell a CDFH signature.
/// Returns `Ok(false)` when the offset is at/past EOF or the bytes
/// aren't a match — used by [`find_central_directory`] to decide
/// whether the declared `cd_offset` is trustworthy.
fn peek_is_cdfh<R: Read + Seek>(reader: &mut R, offset: u64) -> Result<bool> {
    let cur = reader.stream_position()?;
    let restore = |r: &mut R| -> Result<()> {
        r.seek(SeekFrom::Start(cur))?;
        Ok(())
    };
    if reader.seek(SeekFrom::Start(offset)).is_err() {
        let _ = reader.seek(SeekFrom::Start(cur));
        return Ok(false);
    }
    let mut sig = [0u8; 4];
    match reader.read_exact(&mut sig) {
        Ok(()) => {
            restore(reader)?;
            Ok(sig == SIG_CDFH)
        }
        Err(_) => {
            let _ = reader.seek(SeekFrom::Start(cur));
            Ok(false)
        }
    }
}

/// Maximum bytes [`scan_for_cd_start`] reads behind the EOCD when
/// hunting for a recoverable CD origin. Sized to comfortably hold the
/// central directory of a 200 000-entry archive (~16 MB worth of
/// CDFH records, each ~80 bytes); larger archives have to keep the
/// declared `cd_offset` valid (which the strict path would already
/// honour) since unbounded scanning on multi-GB malformed corpora
/// stops being lenient and starts being pathological.
const MAX_CD_SCAN_BYTES: u64 = 64 * 1024 * 1024;

/// Scan the region `[max(0, eocd_abs - MAX_CD_SCAN_BYTES), eocd_abs)`
/// for the first `PK\001\002` signature. Returns the absolute byte
/// offset of that hit. Lenient recovery for archives where the EOCD
/// claims a `cd_offset` we can't trust (past EOF, inside an LFH, or
/// otherwise non-CDFH-aligned).
fn scan_for_cd_start<R: Read + Seek>(reader: &mut R, eocd_abs: u64) -> Result<u64> {
    let scan_len = eocd_abs.min(MAX_CD_SCAN_BYTES);
    let scan_start = eocd_abs - scan_len;
    reader.seek(SeekFrom::Start(scan_start))?;
    let mut buf = vec![0u8; scan_len as usize];
    reader.read_exact(&mut buf)?;
    let off_in_buf = buf
        .windows(4)
        .position(|w| w == SIG_CDFH)
        .ok_or_else(|| OtterzipError::Corrupted {
            reason: format!(
                "no CDFH signature found in last {scan_len} bytes before EOCD"
            ),
            entry: None,
        })?;
    Ok(scan_start + off_in_buf as u64)
}

/// 32-bit EOCD values kept for the lenient ZIP64 fallback path —
/// when the locator is missing but the EOCD's small fields are still
/// usable we honour them rather than failing the whole open. `cd_size_32`
/// isn't carried because the recovery path always derives size from
/// `eocd_abs - cd_offset` (the original 32-bit `cd_size` is the field
/// that was lying when we landed here).
struct ResolvedFallback {
    cd_offset_32: u32,
    total_entries_16: u16,
}

/// Follow the ZIP64 EOCD locator → ZIP64 EOCD record chain. Lenient:
/// when the locator signature is missing but the small-EOCD fields are
/// in-range, fall back to the 32-bit values rather than fail.
fn resolve_zip64<R: Read + Seek>(
    reader: &mut R,
    tail: &[u8],
    tail_start: u64,
    eocd_in_tail: usize,
    eocd_abs: u64,
    file_size: u64,
    fallback: ResolvedFallback,
) -> Result<CdResolved> {
    // The locator sits immediately before the EOCD. If our tail
    // buffer captured it, slice; otherwise issue a small targeted
    // read for those 20 bytes.
    let locator: [u8; ZIP64_LOCATOR_SIZE] = if eocd_in_tail >= ZIP64_LOCATOR_SIZE {
        let mut buf = [0u8; ZIP64_LOCATOR_SIZE];
        buf.copy_from_slice(&tail[eocd_in_tail - ZIP64_LOCATOR_SIZE..eocd_in_tail]);
        let _ = tail_start;
        buf
    } else {
        let loc_abs = eocd_abs
            .checked_sub(ZIP64_LOCATOR_SIZE as u64)
            .ok_or_else(|| OtterzipError::Corrupted {
                reason: "ZIP64 locator slot underflows file start".into(),
                entry: None,
            })?;
        reader.seek(SeekFrom::Start(loc_abs))?;
        let mut buf = [0u8; ZIP64_LOCATOR_SIZE];
        reader.read_exact(&mut buf)?;
        buf
    };

    if locator[..4] != SIG_ZIP64_LOCATOR {
        // No locator. Some toolchains emit the ZIP64 sentinels in the
        // small EOCD but never bother with the locator + ZIP64 record;
        // others have a `cd_size == u32::MAX` field that triggers the
        // ZIP64 branch here but the rest of the archive is otherwise
        // 32-bit. Try to recover via the 32-bit `cd_offset` (when it
        // points at a real CDFH) or by scanning the bytes before the
        // EOCD for the actual CD origin.
        let declared_offset = u64::from(fallback.cd_offset_32);
        let trust_declared = fallback.cd_offset_32 != u32::MAX
            && declared_offset < eocd_abs
            && peek_is_cdfh(reader, declared_offset)?;
        if trust_declared {
            let cd_size = eocd_abs - declared_offset;
            tracing::warn!(
                target: "otterzip::lenient",
                cd_offset = declared_offset,
                cd_size,
                "lenient: ZIP64 sentinel without locator — honouring 32-bit cd_offset"
            );
            return Ok(CdResolved {
                cd_offset: declared_offset,
                cd_size,
                total_entries: u64::from(fallback.total_entries_16),
            });
        }
        // Last resort: scan for the CD start. Same code path the
        // non-ZIP64 lenient branch uses for the "cd_offset bogus" case.
        let recovered = scan_for_cd_start(reader, eocd_abs)?;
        tracing::warn!(
            target: "otterzip::lenient",
            recovered,
            "lenient: ZIP64 sentinel without locator — recovered CD start via scan"
        );
        return Ok(CdResolved {
            cd_offset: recovered,
            cd_size: eocd_abs - recovered,
            total_entries: u64::from(fallback.total_entries_16),
        });
    }

    let z64_eocd_offset = u64::from_le_bytes([
        locator[8], locator[9], locator[10], locator[11],
        locator[12], locator[13], locator[14], locator[15],
    ]);
    if z64_eocd_offset >= file_size {
        return Err(OtterzipError::Corrupted {
            reason: format!(
                "ZIP64 EOCD offset {z64_eocd_offset} >= file size {file_size}"
            ),
            entry: None,
        });
    }

    reader.seek(SeekFrom::Start(z64_eocd_offset))?;
    let mut record = [0u8; ZIP64_EOCD_FIXED_SIZE];
    reader.read_exact(&mut record)?;
    if record[..4] != SIG_ZIP64_EOCD {
        return Err(OtterzipError::Corrupted {
            reason: "ZIP64 EOCD record signature missing at locator target".into(),
            entry: None,
        });
    }
    let total_entries = u64::from_le_bytes([
        record[32], record[33], record[34], record[35],
        record[36], record[37], record[38], record[39],
    ]);
    let cd_size = u64::from_le_bytes([
        record[40], record[41], record[42], record[43],
        record[44], record[45], record[46], record[47],
    ]);
    let cd_offset = u64::from_le_bytes([
        record[48], record[49], record[50], record[51],
        record[52], record[53], record[54], record[55],
    ]);

    if cd_offset > file_size {
        return Err(OtterzipError::Corrupted {
            reason: format!(
                "ZIP64 CD offset {cd_offset} > file size {file_size}"
            ),
            entry: None,
        });
    }

    let max_possible = z64_eocd_offset.saturating_sub(cd_offset);
    let cd_size = if cd_size == 0 {
        tracing::warn!(
            target: "otterzip::lenient",
            "lenient: ZIP64 EOCD cd_size is zero — using gap to ZIP64 EOCD ({max_possible} bytes)"
        );
        max_possible
    } else if cd_size > max_possible {
        tracing::warn!(
            target: "otterzip::lenient",
            declared = cd_size,
            clamped = max_possible,
            "lenient: clamped ZIP64 CD size to fit between cd_offset and ZIP64 EOCD"
        );
        max_possible
    } else {
        cd_size
    };

    Ok(CdResolved {
        cd_offset,
        cd_size,
        total_entries,
    })
}

// === Central directory walk ==========================================

/// Read `cd_size` bytes at `cd_offset` and split them into one CdRecord
/// per CDFH signature. Lenient: malformed records that don't fit the
/// header arithmetic get logged + skipped via a resync to the next
/// `PK\001\002`. Returns the recovered records — possibly fewer than
/// the EOCD's declared count, which is the whole point of this path.
fn walk_central_directory<R: Read + Seek>(
    reader: &mut R,
    cdr: &CdResolved,
    file_size: u64,
) -> Result<Vec<CdRecord>> {
    if cdr.cd_size == 0 {
        return Ok(Vec::new());
    }

    reader.seek(SeekFrom::Start(cdr.cd_offset))?;
    let mut cd_buf = vec![0u8; cdr.cd_size as usize];
    // The lenient path doesn't insist on `read_exact` — a truncated CD
    // is one of the malformations we want to handle. Read whatever
    // fits and operate on the prefix.
    let mut filled = 0usize;
    while filled < cd_buf.len() {
        match reader.read(&mut cd_buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(OtterzipError::Io(e)),
        }
    }
    cd_buf.truncate(filled);

    // Pre-allocate roughly the declared count, but cap to a sane number
    // to avoid OOM on EOCDs that lie about entry counts (a known
    // adversarial pattern in fuzz corpora).
    let cap = usize::try_from(cdr.total_entries.min(1_048_576)).unwrap_or(0);
    let mut out: Vec<CdRecord> = Vec::with_capacity(cap);
    let mut skipped = 0usize;
    let mut cursor = 0usize;

    while cursor + CDFH_FIXED_SIZE <= cd_buf.len() {
        if cd_buf[cursor..cursor + 4] != SIG_CDFH {
            match find_next_cdfh(&cd_buf, cursor + 1) {
                Some(next) => {
                    tracing::warn!(
                        target: "otterzip::lenient",
                        from = cursor,
                        to = next,
                        "lenient: resynchronising to next CDFH signature"
                    );
                    cursor = next;
                    continue;
                }
                None => break,
            }
        }

        match parse_cdfh(&cd_buf[cursor..], file_size) {
            CdfhParse::Ok { record, consumed } => {
                out.push(record);
                cursor += consumed;
            }
            CdfhParse::Resync => {
                skipped += 1;
                match find_next_cdfh(&cd_buf, cursor + 4) {
                    Some(next) => cursor = next,
                    None => break,
                }
            }
        }
    }

    if skipped > 0 {
        tracing::warn!(
            target: "otterzip::lenient",
            skipped,
            kept = out.len(),
            "lenient: skipped malformed CDFH records"
        );
    }

    Ok(out)
}

/// Locate the next CDFH signature in `buf` starting from `from`. Used
/// when the current cursor lands on garbage and we need to resync.
fn find_next_cdfh(buf: &[u8], from: usize) -> Option<usize> {
    if from >= buf.len() {
        return None;
    }
    buf[from..]
        .windows(4)
        .position(|w| w == SIG_CDFH)
        .map(|rel| from + rel)
}

enum CdfhParse {
    Ok { record: CdRecord, consumed: usize },
    /// Header arithmetic was bogus; caller should resync to the next
    /// CDFH signature.
    Resync,
}

/// Parse one CDFH starting at `buf[0]` (signature already verified by
/// the caller). On success, returns the produced [`CdRecord`] and the
/// total bytes consumed (fixed header + name + extra + comment).
fn parse_cdfh(buf: &[u8], file_size: u64) -> CdfhParse {
    if buf.len() < CDFH_FIXED_SIZE {
        return CdfhParse::Resync;
    }

    let gpf = u16::from_le_bytes([buf[8], buf[9]]);
    let method = u16::from_le_bytes([buf[10], buf[11]]);
    let mtime = u16::from_le_bytes([buf[12], buf[13]]);
    let mdate = u16::from_le_bytes([buf[14], buf[15]]);
    let crc32 = u32::from_le_bytes([buf[16], buf[17], buf[18], buf[19]]);
    let comp_size_32 = u32::from_le_bytes([buf[20], buf[21], buf[22], buf[23]]);
    let uncomp_size_32 = u32::from_le_bytes([buf[24], buf[25], buf[26], buf[27]]);
    let name_len = u16::from_le_bytes([buf[28], buf[29]]) as usize;
    let extra_len = u16::from_le_bytes([buf[30], buf[31]]) as usize;
    let comment_len = u16::from_le_bytes([buf[32], buf[33]]) as usize;
    let external_attr = u32::from_le_bytes([buf[38], buf[39], buf[40], buf[41]]);
    let lfh_offset_32 = u32::from_le_bytes([buf[42], buf[43], buf[44], buf[45]]);

    let consumed = CDFH_FIXED_SIZE + name_len + extra_len + comment_len;
    if consumed > buf.len() {
        // Lengths overshoot the remaining CD bytes — typical resync
        // trigger. Caller will skip past this signature byte and
        // hunt for the next one.
        return CdfhParse::Resync;
    }

    let name_start = CDFH_FIXED_SIZE;
    let extra_start = name_start + name_len;
    let comment_start = extra_start + extra_len;
    let name_bytes = &buf[name_start..name_start + name_len];
    let extra_bytes = &buf[extra_start..extra_start + extra_len];
    let comment_bytes = &buf[comment_start..comment_start + comment_len];

    // ZIP64 escalation: any of these slots may carry a 0xFFFF...FF
    // sentinel meaning "look in the 0x0001 extra field for the real
    // value". Order inside the extra is fixed: uncomp_size, comp_size,
    // lfh_offset, disk_number — each present only when its 32-bit
    // slot is the sentinel.
    let mut uncompressed_size = u64::from(uncomp_size_32);
    let mut compressed_size = u64::from(comp_size_32);
    let mut lfh_offset = u64::from(lfh_offset_32);
    let needs_zip64_uncomp = uncomp_size_32 == u32::MAX;
    let needs_zip64_comp = comp_size_32 == u32::MAX;
    let needs_zip64_lfh = lfh_offset_32 == u32::MAX;
    if needs_zip64_uncomp || needs_zip64_comp || needs_zip64_lfh {
        if let Some((u_v, c_v, lfh_v)) = read_zip64_extra(
            extra_bytes,
            needs_zip64_uncomp,
            needs_zip64_comp,
            needs_zip64_lfh,
        ) {
            if let Some(v) = u_v {
                uncompressed_size = v;
            }
            if let Some(v) = c_v {
                compressed_size = v;
            }
            if let Some(v) = lfh_v {
                lfh_offset = v;
            }
        }
        // If the extra didn't carry a sentinel-required field we keep
        // the 32-bit value — that's still useful for partial recovery
        // and Day 2's read path will surface a per-entry error if it
        // actually overshoots the file.
    }

    // Lenient: an LFH offset past EOF means we can't read this entry,
    // but other entries in the CD may still be salvageable. Mark the
    // record with u64::MAX so Day 2 fails this one cleanly instead of
    // mis-seeking into garbage.
    let lfh_offset_marker = if lfh_offset >= file_size {
        tracing::warn!(
            target: "otterzip::lenient",
            lfh_offset,
            file_size,
            "lenient: CDFH points past EOF — entry marked unreadable"
        );
        u64::MAX
    } else {
        lfh_offset
    };

    let path = decode_name(name_bytes, gpf);
    let is_directory = path.ends_with('/');
    let is_symlink = ((external_attr >> 16) & 0o170_000) == 0o120_000;
    let compression = map_method(method);
    let encryption = if gpf & 0x0001 != 0 {
        // GP bit 0 set just means "this entry is encrypted". The exact
        // algorithm (ZipCrypto / AES-128 / AES-256) lives in the
        // optional 0x9901 WinZip AES extra; Day 2 will refine the
        // detection. For Day 1 we surface ZipCrypto as the conservative
        // "encrypted, unknown variant" placeholder — `is_encrypted_fast`
        // only cares whether the variant is non-`None`.
        EncryptionMethod::ZipCrypto
    } else {
        EncryptionMethod::None
    };
    let modified = dos_to_systime(mdate, mtime);
    let comment = if comment_bytes.is_empty() {
        None
    } else {
        Some(String::from_utf8_lossy(comment_bytes).into_owned())
    };

    let entry = Entry {
        path,
        is_directory,
        is_symlink,
        uncompressed_size,
        compressed_size,
        compression,
        encryption,
        crc32: Some(crc32),
        modified,
        accessed: None,
        created: None,
        attributes: external_attr,
        comment,
        host_os: HostOs::Unknown,
    };

    CdfhParse::Ok {
        record: CdRecord {
            entry,
            lfh_offset: lfh_offset_marker,
            raw_method: method,
            raw_gpf: gpf,
        },
        consumed,
    }
}

/// Pull ZIP64-escalated values from a CDFH's extra field. Returns
/// `Some((uncomp, comp, lfh))` when the 0x0001 tag was found — each
/// inner `Option` is populated only when the caller asked for it (i.e.
/// the matching 32-bit slot held the sentinel). Returns `None` when
/// no ZIP64 extra is present at all.
fn read_zip64_extra(
    extra: &[u8],
    want_uncomp: bool,
    want_comp: bool,
    want_lfh: bool,
) -> Option<(Option<u64>, Option<u64>, Option<u64>)> {
    let mut cursor = 0usize;
    while cursor + 4 <= extra.len() {
        let tag = u16::from_le_bytes([extra[cursor], extra[cursor + 1]]);
        let len = u16::from_le_bytes([extra[cursor + 2], extra[cursor + 3]]) as usize;
        cursor += 4;
        if cursor + len > extra.len() {
            return None;
        }
        if tag != EXTRA_TAG_ZIP64 {
            cursor += len;
            continue;
        }
        // Found the ZIP64 extra. Pull the 8-byte fields in order, only
        // for the slots the caller flagged.
        let body = &extra[cursor..cursor + len];
        let mut inner = 0usize;
        let mut out_uncomp = None;
        let mut out_comp = None;
        let mut out_lfh = None;
        if want_uncomp {
            if inner + 8 > body.len() {
                return Some((out_uncomp, out_comp, out_lfh));
            }
            out_uncomp = Some(u64::from_le_bytes([
                body[inner], body[inner + 1], body[inner + 2], body[inner + 3],
                body[inner + 4], body[inner + 5], body[inner + 6], body[inner + 7],
            ]));
            inner += 8;
        }
        if want_comp {
            if inner + 8 > body.len() {
                return Some((out_uncomp, out_comp, out_lfh));
            }
            out_comp = Some(u64::from_le_bytes([
                body[inner], body[inner + 1], body[inner + 2], body[inner + 3],
                body[inner + 4], body[inner + 5], body[inner + 6], body[inner + 7],
            ]));
            inner += 8;
        }
        if want_lfh {
            if inner + 8 > body.len() {
                return Some((out_uncomp, out_comp, out_lfh));
            }
            out_lfh = Some(u64::from_le_bytes([
                body[inner], body[inner + 1], body[inner + 2], body[inner + 3],
                body[inner + 4], body[inner + 5], body[inner + 6], body[inner + 7],
            ]));
        }
        return Some((out_uncomp, out_comp, out_lfh));
    }
    None
}

// === Field decoders ==================================================

/// Decode an entry name. Tier-1 only for Day 1: trust GP bit 11 →
/// UTF-8; otherwise fall back to `from_utf8_lossy` which preserves
/// ASCII (the regression-test fixtures we ship today are ASCII-only).
/// The full CP949 / chardetng cascade from `src/encoding.rs` lands
/// when Day 2 wires the read path — the archive-level decision needs
/// every name at once and is too heavyweight for the metadata-only
/// path.
fn decode_name(bytes: &[u8], gpf: u16) -> String {
    let utf8_flag = gpf & 0x0800 != 0;
    if utf8_flag {
        if let Ok(s) = std::str::from_utf8(bytes) {
            return s.to_string();
        }
    }
    if let Ok(s) = std::str::from_utf8(bytes) {
        return s.to_string();
    }
    String::from_utf8_lossy(bytes).into_owned()
}

/// Map a CDFH compression method code (APPNOTE.TXT §4.4.5) to our
/// public enum. Codes we don't ship support for collapse to `Unknown`;
/// Day 2's dispatch surfaces those as `FeatureDisabled` per-entry
/// rather than failing the whole open.
fn map_method(method: u16) -> CompressionMethod {
    match method {
        0 => CompressionMethod::Store,
        8 => CompressionMethod::Deflate,
        9 => CompressionMethod::Deflate64,
        12 => CompressionMethod::Bzip2,
        14 => CompressionMethod::Lzma,
        93 => CompressionMethod::Zstd,
        98 => CompressionMethod::Ppmd,
        _ => CompressionMethod::Unknown,
    }
}

/// Convert an MS-DOS date + time pair into a `SystemTime`. DOS time is
/// the same format the `zip` crate decodes — using the same Howard
/// Hinnant civil-from-days algorithm keeps the lenient backend's
/// `Entry.modified` byte-identical to the strict backend's for the
/// regression-test parity assertion.
fn dos_to_systime(dos_date: u16, dos_time: u16) -> Option<SystemTime> {
    if dos_date == 0 && dos_time == 0 {
        return None;
    }
    let year = ((dos_date >> 9) as i32) + 1980;
    let month = u32::from((dos_date >> 5) & 0x0f);
    let day = u32::from(dos_date & 0x1f);
    let hour = u32::from(dos_time >> 11);
    let minute = u32::from((dos_time >> 5) & 0x3f);
    let second = u32::from((dos_time & 0x1f) * 2);

    let days = days_from_civil(year, month, day)?;
    let secs = i64::from(days) * 86_400
        + i64::from(hour) * 3600
        + i64::from(minute) * 60
        + i64::from(second);
    let secs_u64 = u64::try_from(secs).ok()?;
    Some(UNIX_EPOCH + Duration::from_secs(secs_u64))
}

/// Days since 1970-01-01 for a (proleptic Gregorian) date. Copied from
/// `backends/zip.rs` so the lenient and strict backends agree on the
/// epoch arithmetic (the parity test wants byte-identical `Entry`s,
/// including `modified`).
fn days_from_civil(year: i32, month: u32, day: u32) -> Option<i32> {
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let civil_year = if month <= 2 { year - 1 } else { year };
    let era = civil_year.div_euclid(400);
    let yoe = u32::try_from(civil_year.rem_euclid(400)).ok()?;
    let month_offset = if month > 2 { month - 3 } else { month + 9 };
    let doy = (153 * month_offset + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era.checked_mul(146_097)?
        .checked_add(i32::try_from(doe).ok()?)?
        .checked_sub(719_468)
}

// === Internal probe (Day 1 test surface) ==============================

/// Test-only entry point — opens a lenient backend and returns the
/// `(path, uncompressed_size)` pairs the CD walk produced. The Day 1
/// regression test in `tests/lenient_zip.rs` uses this to compare
/// lenient vs strict parity without depending on the
/// `libarchive-fallback` feature being on (the dispatcher's fallback
/// arm only fires for malformed archives, but parity should hold for
/// healthy ones too).
#[doc(hidden)]
pub fn __probe_entries(path: &Path) -> Result<Vec<(String, u64)>> {
    let backend = LenientZipBackend::open(path, None)?;
    let iter = backend.entries()?;
    iter.map(|r| r.map(|e| (e.path, e.uncompressed_size))).collect()
}

/// Day-2 sibling of [`__probe_entries`]. Opens the archive through
/// the lenient backend and extracts a single named entry into a
/// `Vec<u8>` for byte-level test assertions. The wrapper in
/// `lib.rs::__probe_lenient_extract` re-exports this to integration
/// tests; the dispatcher's full `Archive::open → extract_all` path is
/// covered separately by `tests/libarchive_fallback.rs`.
#[doc(hidden)]
pub fn __probe_extract(path: &Path, entry_name: &str) -> Result<Vec<u8>> {
    let backend = LenientZipBackend::open(path, None)?;
    let mut sink = Vec::new();
    backend.extract_entry(entry_name, &mut sink)?;
    Ok(sink)
}

// === Per-entry payload read (Day 2) ===================================

/// Seek to `record.lfh_offset`, validate the LFH signature, and return
/// the absolute byte offset of the entry's payload start (i.e. just
/// past the LFH's name + extra fields).
///
/// Lenient: we trust the LFH's own `name_len` / `extra_len` for the
/// arithmetic — those can legitimately differ from the CDFH fields
/// (some authoring tools emit a UTF-8 name in the CDFH and a CP437
/// fallback in the LFH, with different lengths) and the LFH values
/// are what govern where the payload actually starts. The CDFH-side
/// values are still used for everything else: compression method,
/// sizes, encryption flag — see [`CdRecord`].
fn locate_payload<R: Read + Seek>(
    reader: &mut R,
    record: &CdRecord,
    file_size: u64,
) -> Result<u64> {
    if record.lfh_offset >= file_size {
        return Err(OtterzipError::Corrupted {
            reason: format!(
                "LFH offset {} >= file size {}",
                record.lfh_offset, file_size
            ),
            entry: Some(record.entry.path.clone()),
        });
    }
    reader.seek(SeekFrom::Start(record.lfh_offset))?;
    let mut header = [0u8; LFH_FIXED_SIZE];
    reader.read_exact(&mut header)?;
    if header[..4] != SIG_LFH {
        return Err(OtterzipError::Corrupted {
            reason: format!(
                "LFH at offset {} missing signature (got {:02x?})",
                record.lfh_offset,
                &header[..4]
            ),
            entry: Some(record.entry.path.clone()),
        });
    }
    let name_len = u64::from(u16::from_le_bytes([header[26], header[27]]));
    let extra_len = u64::from(u16::from_le_bytes([header[28], header[29]]));
    let payload = record
        .lfh_offset
        .checked_add(LFH_FIXED_SIZE as u64)
        .and_then(|v| v.checked_add(name_len))
        .and_then(|v| v.checked_add(extra_len))
        .ok_or_else(|| OtterzipError::Corrupted {
            reason: "LFH name/extra arithmetic overflowed u64".into(),
            entry: Some(record.entry.path.clone()),
        })?;
    if payload > file_size {
        return Err(OtterzipError::Corrupted {
            reason: format!(
                "LFH payload start {payload} > file size {file_size}"
            ),
            entry: Some(record.entry.path.clone()),
        });
    }
    Ok(payload)
}

/// Dispatch the compressed bytes in `input` to the right decoder and
/// copy the inflated bytes into `out`. Returns the number of bytes
/// written. `entry_path` is used purely for error context.
///
/// `compressed_size` and `uncompressed_size` come from the entry's
/// CDFH — the deflate path uses them to decide between the
/// `libdeflater` one-shot decoder (faster, needs both sizes
/// up-front) and the `flate2` streaming decoder (works without
/// reliable size hints, no allocation cap).
///
/// Supported methods are listed in the module-level doc. Anything
/// else returns [`OtterzipError::FeatureDisabled`] with a clear hint
/// so the caller (the framework's per-entry loop) can record the
/// skip in the `ExtractReport`.
fn decompress<R: Read>(
    method: u16,
    mut input: R,
    compressed_size: u64,
    uncompressed_size: u64,
    entry_path: &str,
    out: &mut dyn std::io::Write,
) -> Result<u64> {
    match method {
        0 => {
            // Stored — compressed == uncompressed, straight copy.
            let n = std::io::copy(&mut input, out)?;
            Ok(n)
        }
        8 => {
            // Deflate — pick the fastest available decoder for the
            // entry's size class. The one-shot libdeflater path is
            // ~20 % faster than streaming flate2 on the deflate hot
            // loop, but only safe when we know both buffer sizes
            // up-front (otherwise we'd over-allocate or trip the
            // crate's "output too small" error).
            //
            // Entry-size guard: both compressed and uncompressed
            // sizes must be under the threshold so the worker's
            // working set stays bounded. Real-world ZIPs we ship
            // for (~750 KB average entry on the 7 GB user corpus)
            // pass; pathological archives with multi-GB single
            // entries fall through to the streaming path.
            let oneshot_eligible = uncompressed_size > 0
                && compressed_size > 0
                && uncompressed_size <= LIBDEFLATER_ONESHOT_THRESHOLD
                && compressed_size <= LIBDEFLATER_ONESHOT_THRESHOLD;
            if oneshot_eligible {
                let comp_cap = usize::try_from(compressed_size).unwrap_or(usize::MAX);
                let uncomp_cap = usize::try_from(uncompressed_size).unwrap_or(usize::MAX);
                let mut in_buf = Vec::with_capacity(comp_cap);
                let read_n = input.take(compressed_size).read_to_end(&mut in_buf)?;
                if read_n as u64 != compressed_size {
                    // CDFH lied about the compressed size (truncated
                    // archive). Fall back to streaming so we don't
                    // hand libdeflater a short buffer; the streaming
                    // path stops at the real EOF cleanly.
                    return decompress_streaming(
                        std::io::Cursor::new(in_buf),
                        out,
                    );
                }
                let mut out_buf = vec![0u8; uncomp_cap];
                let mut dec = libdeflater::Decompressor::new();
                match dec.deflate_decompress(&in_buf, &mut out_buf) {
                    Ok(n) => {
                        out.write_all(&out_buf[..n])?;
                        Ok(n as u64)
                    }
                    Err(e) => {
                        // libdeflater is strict — if the CDFH's
                        // uncompressed_size was wrong or the stream
                        // has trailing junk, the one-shot path
                        // errors out. Retry through the streaming
                        // decoder which tolerates both.
                        tracing::debug!(
                            target: "otterzip::lenient",
                            entry = %entry_path,
                            error = ?e,
                            "lenient: libdeflater rejected one-shot, falling back to streaming"
                        );
                        decompress_streaming(std::io::Cursor::new(in_buf), out)
                    }
                }
            } else {
                decompress_streaming(input, out)
            }
        }
        other => Err(OtterzipError::FeatureDisabled(
            // The static-str variant requires a `&'static str`; we
            // log the method + entry separately so the message in
            // the user's logs still has the diagnostic info.
            match () {
                () => {
                    tracing::warn!(
                        target: "otterzip::lenient",
                        method = other,
                        entry = %entry_path,
                        "lenient: compression method not yet supported"
                    );
                    "lenient ZIP: compression method beyond Store/Deflate (v1.1)"
                }
            },
        )),
    }
}

/// flate2 streaming decoder path. Used directly for large entries
/// (where libdeflater's one-shot buffer would balloon RAM) and as a
/// fallback when the libdeflater path rejects the stream (CDFH size
/// lie, stream with trailing bytes, etc.). The hard cap protects
/// against runaway inflate when the CDFH's uncompressed_size is
/// missing or malicious; healthy entries stop at the Take reader's
/// EOF long before reaching the cap.
fn decompress_streaming<R: Read>(input: R, out: &mut dyn std::io::Write) -> Result<u64> {
    let mut decoder = flate2::read::DeflateDecoder::new(input);
    let mut capped = (&mut decoder).take(STREAMING_INFLATE_HARD_CAP);
    let n = std::io::copy(&mut capped, out)?;
    Ok(n)
}

// === Tests ============================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;
    use tempfile::tempdir;

    fn build_simple_zip(path: &Path) {
        let f = fs::File::create(path).unwrap();
        let mut w = zip::ZipWriter::new(f);
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        w.start_file("hello.txt", opts).unwrap();
        w.write_all(b"hello world").unwrap();
        w.add_directory("sub/", opts).unwrap();
        w.start_file("sub/nested.bin", opts).unwrap();
        w.write_all(&[0u8; 1024]).unwrap();
        w.finish().unwrap();
    }

    #[test]
    fn open_healthy_zip_lists_three_entries() {
        let td = tempdir().unwrap();
        let p = td.path().join("a.zip");
        build_simple_zip(&p);
        let pairs = __probe_entries(&p).unwrap();
        assert_eq!(pairs.len(), 3);
        assert!(pairs.iter().any(|(n, _)| n == "hello.txt"));
        assert!(pairs.iter().any(|(n, _)| n == "sub/"));
        assert!(pairs.iter().any(|(n, sz)| n == "sub/nested.bin" && *sz == 1024));
    }

    #[test]
    fn empty_archive_too_small_returns_corrupted() {
        let td = tempdir().unwrap();
        let p = td.path().join("tiny.zip");
        fs::write(&p, b"\x00\x00\x00").unwrap();
        // `LenientZipBackend` doesn't impl Debug (no need on the
        // public surface), so the canonical `unwrap_err()` doesn't
        // type-check here — match on the result instead.
        match LenientZipBackend::open(&p, None) {
            Err(OtterzipError::Corrupted { .. }) => {}
            Err(other) => panic!("expected Corrupted, got {other:?}"),
            Ok(_) => panic!("3-byte file should not parse as a ZIP"),
        }
    }

    #[test]
    fn dos_time_round_trip_is_sane() {
        // 1990-06-15 12:30:00 UTC. Hand-encode the DOS date/time bit
        // layout (APPNOTE.TXT §4.4.6) and decode it; the result must
        // round-trip to the matching epoch second.
        let dos_date = ((1990u16 - 1980) << 9) | (6u16 << 5) | 15;
        let dos_time = (12u16 << 11) | (30u16 << 5);
        let st = dos_to_systime(dos_date, dos_time).unwrap();
        let secs = st.duration_since(UNIX_EPOCH).unwrap().as_secs();
        // 1990-01-01 00:00:00 UTC = 631_152_000 epoch seconds; add 165
        // days (Jan 31 + Feb 28 + Mar 31 + Apr 30 + May 31 + 14) for
        // 1990-06-15 00:00:00 = 645_408_000, then +12h30m = 645_453_000.
        assert_eq!(secs, 645_453_000);
    }
}
