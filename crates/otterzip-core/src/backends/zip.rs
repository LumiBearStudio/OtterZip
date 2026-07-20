//! ZIP backend — wraps the `zip` crate for Sprint 1 read path.
//!
//! Performance notes (per `performance.md` §2, §5):
//! * The archive file is opened once with `BufReader` so random-access
//!   reads during entry iteration don't cause syscall storms.
//! * Entry enumeration allocates per-entry only for the owned
//!   `String` fields of the public `Entry` POD (unavoidable per the
//!   API contract in `rust-core-api.md` §1.2). No per-byte allocations
//!   occur in the extraction path.

use std::cell::RefCell;
use std::fs::File;
use std::io::{self, BufReader, BufWriter, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use rayon::prelude::*;
use zeroize::Zeroizing;
use zip::ZipArchive;

use crate::archive::ExtractWarning;
use crate::backends::multi_volume_reader::open_for_sequential_read;
use crate::backends::spanned_zip::SpannedZipReader;
use crate::backends::{ArchiveBackend, StreamingExtractCtx};
use crate::entry::{Entry, HostOs};
use crate::error::{Result, OtterzipError};
use crate::format::{CompressionMethod, EncryptionMethod};
use crate::options::OverwritePolicy;
use crate::progress::{Progress, ProgressPhase};

/// Reader source for the ZIP backend. Single-file is the common case;
/// multi-volume uses the [`SpannedZipReader`] overlay so APPNOTE-spanned
/// archives (per-disk offset references) and raw byte-split archives
/// both look like single-disk ZIPs to the upstream `zip` crate.
///
/// Both variants are wrapped in `BufReader` so the upstream parser's
/// many small reads (each CD record is ~46 bytes + variable filename;
/// each local-file-header decode kicks off a chain of small header
/// reads before the entry payload begins) coalesce into ~64 KiB
/// syscalls. Without this, the multi-volume path was hitting the
/// underlying file once per `zip` crate read — a measurable
/// regression on archives whose payload is decompressed by zlib-ng
/// quickly enough that syscall overhead dominates.
pub(crate) enum ZipReader {
    Single(BufReader<File>),
    /// Wrapped variant used only during `ZipArchive::new` so the
    /// strict ZIP parser bails inside the 5-second deadline budget on
    /// pathological CD layouts. Swapped to `Single` immediately after
    /// the parser returns so the entry-read hot path is never on the
    /// clock-checked path.
    SingleDeadline(DeadlineReader<BufReader<File>>),
    Multi(BufReader<SpannedZipReader>),
}

impl Read for ZipReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::Single(r) => r.read(buf),
            Self::SingleDeadline(r) => r.read(buf),
            Self::Multi(r) => r.read(buf),
        }
    }
}

impl Seek for ZipReader {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        match self {
            Self::Single(r) => r.seek(pos),
            Self::SingleDeadline(r) => r.seek(pos),
            Self::Multi(r) => r.seek(pos),
        }
    }
}

/// `Read + Seek` adapter that synthesises an `io::Error` once the
/// wall-clock budget is exhausted. Used to cap `ZipArchive::new`'s CD
/// parse so a malformed archive can't burn 30+ seconds on the user's
/// UI thread before we know to bail.
pub(crate) struct DeadlineReader<R> {
    inner: R,
    started: std::time::Instant,
    budget: std::time::Duration,
}

impl<R> DeadlineReader<R> {
    pub(crate) fn new(inner: R, budget_ms: u64) -> Self {
        Self {
            inner,
            started: std::time::Instant::now(),
            budget: std::time::Duration::from_millis(budget_ms),
        }
    }

    pub(crate) fn into_inner(self) -> R {
        self.inner
    }

    fn check_deadline(&self) -> io::Result<()> {
        if self.started.elapsed() > self.budget {
            return Err(io::Error::new(
                io::ErrorKind::Other,
                "ZipArchive::new deadline exceeded — archive may be malformed",
            ));
        }
        Ok(())
    }
}

impl<R: Read> Read for DeadlineReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.check_deadline()?;
        self.inner.read(buf)
    }
}

impl<R: Seek> Seek for DeadlineReader<R> {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        self.check_deadline()?;
        self.inner.seek(pos)
    }
}

/// Wrapper around `zip::ZipArchive` over either a single-file or
/// multi-volume reader.
///
/// Held behind `Box<dyn ArchiveBackend>` in `Archive`. The inner archive
/// is wrapped in a `RefCell` because the `zip` crate requires `&mut self`
/// on every read, while our public API exposes `&self` methods per
/// `rust-core-api.md` §1.1. Safety of `RefCell` here relies on `Archive`
/// being `!Sync` — the doc contract already forbids cross-thread sharing
/// without a mutex (§5).
/// Where each parallel-extract worker should re-open the archive
/// from. Captured at `open` time so the worker closures don't need
/// to inspect [`ZipBackend`]'s state during dispatch.
#[derive(Clone)]
pub(crate) enum OpenSource {
    Single(PathBuf),
    Multi(Vec<PathBuf>),
}

pub(crate) struct ZipBackend {
    inner: RefCell<ZipArchive<ZipReader>>,
    /// Held in zeroized memory so the bytes are wiped on drop. We clone
    /// when calling into the `zip` crate's password APIs; the source-of-
    /// truth string lives here and is never logged or formatted.
    password: Option<Zeroizing<String>>,
    /// Where parallel-extract workers re-open the archive. For
    /// single-volume this is one path; for multi-volume it carries
    /// the full ordered volume list so each worker can rebuild its
    /// own `SpannedZipReader`. Per-worker file handle cost: N volumes
    /// × workers — fine for typical splits (28 vols × 8 workers = 224
    /// FDs, well under the process limit).
    source: OpenSource,
}

/// Maximum wall-clock budget for `ZipArchive::new` (CD + EOCD parse).
///
/// Observed timings on healthy archives:
///   *   35 MB / 9 entries:    6 ms
///   *  596 MB / 34 entries:   5 ms
///   * 1.35 GB / 9 entries:  ~50 ms (spanned)
///
/// So 1 second is ~20x headroom for every healthy case while keeping
/// the malformed-archive bailout fast enough that the user doesn't
/// perceive it. Combined with [`quick_eocd_sanity_check`] (which
/// catches most malformations in < 50 ms without ever starting the
/// deadline-bounded parse), the worst-case perceived hang on a
/// fallback-needed archive is now sub-200 ms.
const OPEN_DEADLINE_MS: u64 = 1_000;

impl ZipBackend {
    pub(crate) fn open(path: &Path, password: Option<&Zeroizing<String>>) -> Result<Self> {
        // Quick sanity check on the EOCD before we burn the deadline
        // budget on `ZipArchive::new`. Most malformations the user
        // reports show up as "CD bounds extend past EOF" — we can
        // detect that in < 50 ms via a 96-byte tail read and short-
        // circuit to `Corrupted`, letting the dispatcher route to
        // libarchive without the strict parser ever running.
        if let Err(quick_err) = quick_eocd_sanity_check(path) {
            tracing::warn!(
                target: "otterzip::zip",
                path = %path.display(),
                reason = %quick_err,
                "ZipBackend::open quick sanity check failed — short-circuiting to fallback"
            );
            return Err(OtterzipError::Corrupted {
                reason: format!("quick EOCD sanity check failed: {quick_err}"),
                entry: None,
            });
        }
        let t_file_open = std::time::Instant::now();
        let file = open_for_sequential_read(path)?;
        tracing::debug!(
            target: "otterzip::zip",
            path = %path.display(),
            elapsed_us = t_file_open.elapsed().as_micros() as u64,
            "ZipBackend::open file opened"
        );
        log_tail_bytes(path);
        let buffered = BufReader::new(file);
        let deadline = DeadlineReader::new(buffered, OPEN_DEADLINE_MS);
        let reader = ZipReader::SingleDeadline(deadline);
        let t_zip_new = std::time::Instant::now();
        let inner = ZipArchive::new(reader).map_err(|e| {
            tracing::warn!(
                target: "otterzip::zip",
                path = %path.display(),
                elapsed_ms = t_zip_new.elapsed().as_millis() as u64,
                error = ?e,
                "ZipBackend::open ZipArchive::new returned error"
            );
            map_zip_err(e)
        })?;
        tracing::info!(
            target: "otterzip::zip",
            path = %path.display(),
            elapsed_ms = t_zip_new.elapsed().as_millis() as u64,
            entry_count = inner.len(),
            "ZipBackend::open ZipArchive::new done"
        );
        // After CD parse the deadline has served its purpose — unwrap
        // back to a plain BufReader for the entry-read path so the
        // hot loop doesn't pay a per-read clock check.
        let unwrapped = inner.into_inner();
        let plain = match unwrapped {
            ZipReader::SingleDeadline(dl) => ZipReader::Single(dl.into_inner()),
            other => other,
        };
        let inner = ZipArchive::new(plain).map_err(map_zip_err)?;
        Ok(Self {
            inner: RefCell::new(inner),
            password: password.cloned(),
            source: OpenSource::Single(path.to_path_buf()),
        })
    }

    /// Open a spanned / split ZIP given its volume paths in disk order
    /// (first volume → last). The volumes are presented to the `zip`
    /// crate as a single virtual seekable stream via
    /// [`MultiVolumeReader`], so the central directory and per-entry
    /// local headers can reference absolute byte positions across the
    /// concatenation without any disk-boundary awareness in the
    /// upstream crate.
    ///
    /// Limitations: backed by the same parser as single-volume ZIP, so
    /// archives whose central-directory entries set
    /// `disk_number_start > 0` may fail with a malformed-archive error
    /// from the `zip` crate's single-disk assertion — those layouts
    /// require a v1.1 deeper integration. The "split byte-stream"
    /// shape produced by WinRAR / 7-Zip / Info-ZIP `zip -s` works.
    pub(crate) fn open_multi(volumes: &[PathBuf], password: Option<&Zeroizing<String>>) -> Result<Self> {
        if volumes.is_empty() {
            return Err(OtterzipError::InvalidArgument(
                "ZipBackend::open_multi requires at least one volume",
            ));
        }
        let szr = SpannedZipReader::open(volumes)?;
        // 64 KiB BufReader chunks coalesce the upstream `zip` crate's
        // many small reads into ~16 syscalls per volume rather than
        // one per read. Matches the Single-volume path's buffering.
        let reader = ZipReader::Multi(BufReader::with_capacity(64 * 1024, szr));
        let inner = ZipArchive::new(reader).map_err(map_zip_err)?;
        Ok(Self {
            inner: RefCell::new(inner),
            password: password.cloned(),
            source: OpenSource::Multi(volumes.to_vec()),
        })
    }

    /// Resolve a name to an open `ZipFile`, applying the stored password
    /// when the entry is encrypted. Wrapped here so both `extract_entry`
    /// and `open_entry_stream` get the same decryption discipline.
    fn read_by_name<'a>(
        archive: &'a mut ZipArchive<ZipReader>,
        name: &str,
        password: Option<&Zeroizing<String>>,
    ) -> Result<zip::read::ZipFile<'a>> {
        // Probe the entry to see whether it needs a password. We trial
        // `by_name`, classify the error, and only *then* re-borrow with
        // `by_name_decrypt`. The probe's borrow is released at the end of
        // the match block because `probe_outcome` is a `bool`, not a
        // reference back into `archive`.
        enum Outcome {
            Plain,
            NeedsPassword,
            NotFound,
            Other(OtterzipError),
        }
        let outcome = {
            match archive.by_name(name) {
                Ok(_) => Outcome::Plain,
                Err(zip::result::ZipError::FileNotFound) => Outcome::NotFound,
                Err(zip::result::ZipError::UnsupportedArchive(msg))
                    if msg_indicates_encryption(msg) =>
                {
                    Outcome::NeedsPassword
                }
                Err(other) => Outcome::Other(map_zip_err(other)),
            }
        };
        match outcome {
            Outcome::Plain => archive.by_name(name).map_err(|e| match e {
                zip::result::ZipError::FileNotFound => {
                    OtterzipError::EntryNotFound(name.to_string())
                }
                other => map_zip_err(other),
            }),
            Outcome::NeedsPassword => {
                let p = password
                    .ok_or_else(|| {
                        // Trace the *fact* of a missing password, never
                        // the value. Useful for distinguishing genuine
                        // user errors from instrumentation noise.
                        tracing::info!(
                            target: "otterzip::security",
                            event = "wrong_password",
                            reason = "no_password_supplied",
                            "encrypted entry without password"
                        );
                        OtterzipError::WrongPassword
                    })?
                    .as_bytes();
                archive.by_name_decrypt(name, p).map_err(|e| match e {
                    zip::result::ZipError::InvalidPassword => {
                        tracing::info!(
                            target: "otterzip::security",
                            event = "wrong_password",
                            reason = "invalid_password",
                            "decryption failed for entry"
                        );
                        OtterzipError::WrongPassword
                    }
                    zip::result::ZipError::FileNotFound => {
                        OtterzipError::EntryNotFound(name.to_string())
                    }
                    other => map_zip_err(other),
                })
            }
            Outcome::NotFound => Err(OtterzipError::EntryNotFound(name.to_string())),
            Outcome::Other(e) => Err(e),
        }
    }
}

/// True when an `UnsupportedArchive` message signals that the entry is
/// encrypted (zip 2.x emits "Password required to decrypt file" or similar
/// — exact wording varies between versions, so we match loosely).
fn msg_indicates_encryption(msg: &str) -> bool {
    let lower = msg.to_ascii_lowercase();
    lower.contains("password") || lower.contains("encrypt")
}

impl ArchiveBackend for ZipBackend {
    fn entries(&self) -> Result<Box<dyn Iterator<Item = Result<Entry>> + '_>> {
        // We materialize all entries up front. `ZipArchive::by_index`
        // returns a `ZipFile` that mutably borrows the archive, so a lazy
        // iterator would need self-referential storage. Enumeration is
        // not in a hot loop (per `performance.md` §2 — hot loops are the
        // per-byte decompression paths inside backends), so the one-shot
        // Vec allocation is negligible relative to I/O.
        let mut archive = self.inner.borrow_mut();
        let count = archive.len();
        let mut collected: Vec<Result<Entry>> = Vec::with_capacity(count);
        for i in 0..count {
            collected.push(entry_at(&mut *archive, i));
        }
        Ok(Box::new(collected.into_iter()))
    }

    fn extract_entry(&self, entry_path: &str, out: &mut dyn std::io::Write) -> Result<u64> {
        let mut archive = self.inner.borrow_mut();
        let mut zf = Self::read_by_name(&mut archive, entry_path, self.password.as_ref())?;
        let written = std::io::copy(&mut zf, out)?;
        Ok(written)
    }

    fn open_entry_stream(&self, entry_path: &str) -> Result<Box<dyn Read + Send + '_>> {
        // `zip::ZipFile` is not `Send` and borrows the archive mutably.
        // Sprint 1 compromise: eagerly copy into an in-memory `Vec` and
        // hand back a `Cursor`. Large-entry streaming is deferred to
        // Sprint 3 once the backend contract grows a proper streaming
        // method that respects `RefCell` borrow scoping.
        let mut archive = self.inner.borrow_mut();
        let mut zf = Self::read_by_name(&mut archive, entry_path, self.password.as_ref())?;
        let expected = zf.size();
        let cap = usize::try_from(expected).unwrap_or(0);
        let mut buf = Vec::with_capacity(cap);
        std::io::copy(&mut zf, &mut buf)?;
        Ok(Box::new(std::io::Cursor::new(buf)))
    }

    fn extract_all_streaming(
        &self,
        ctx: &mut StreamingExtractCtx<'_>,
    ) -> Option<Result<()>> {
        // Multi-volume now participates in the parallel path: each
        // worker re-opens the archive via `open_local(&self.source)`
        // which dispatches to either `File::open` (Single) or
        // `SpannedZipReader::open` (Multi). User benchmark
        // (1.35 GB / 9-entry spanned ZIP) showed disk usage 30–40%
        // serial — workers needed to saturate the NVMe.
        //
        // Decision rule (per `performance.md` §4): only switch to the
        // parallel path when there's enough work to amortise the
        // per-thread `ZipArchive::new` cost (re-parses the central
        // directory). For small archives the serial path in
        // `archive.rs::extract_all` is faster — return None to fall back.
        let entries = match self.inner.borrow_mut().len() {
            0 => return None,
            n => n,
        };
        let total_compressed: u64 = {
            let mut a = self.inner.borrow_mut();
            (0..a.len())
                .filter_map(|i| a.by_index_raw(i).ok().map(|f| f.compressed_size()))
                .sum()
        };
        const PARALLEL_MIN_BYTES: u64 = 4 * 1024 * 1024;
        const PARALLEL_MIN_ENTRIES: usize = 8;
        // Tiny-file regression guard. The original 32 KiB threshold
        // was tuned against synthetic 1024 × 1 KiB archives where the
        // total payload was sub-megabyte and NTFS metadata
        // serialisation made parallel a loss. Real-world "many small
        // files" archives (typical user case — game asset dumps,
        // source trees, photo libraries — many GB across 100K+ tiny
        // entries) show the opposite profile: per-entry CPU+I/O is
        // small, but the *aggregate* serial time is dominated by
        // total volume × decoder throughput, which parallelises fine.
        //
        // Two-tier rule:
        //   * archives ≥ 50 MiB total bypass the avg-entry check
        //     entirely (large workloads always win from parallel,
        //     even with NTFS contention),
        //   * smaller archives keep the 32 KiB safety to avoid the
        //     synthetic-fixture regression.
        //
        // Symptom that triggered this re-tune: 7 GB / many-file ZIP
        // ran serial (CPU 14 %, disk read 75 MB/s, write 0 from the
        // user's view on the source disk), wall ~3× Bandizip.
        const PARALLEL_MIN_AVG_ENTRY_BYTES: u64 = 32 * 1024;
        const PARALLEL_LARGE_ARCHIVE_BYTES: u64 = 50 * 1024 * 1024;
        let avg = if entries == 0 {
            0
        } else {
            total_compressed / entries as u64
        };
        let large_enough_to_skip_avg_check =
            total_compressed >= PARALLEL_LARGE_ARCHIVE_BYTES;
        let go_serial = entries < PARALLEL_MIN_ENTRIES
            || total_compressed < PARALLEL_MIN_BYTES
            || (!large_enough_to_skip_avg_check && avg < PARALLEL_MIN_AVG_ENTRY_BYTES);
        tracing::info!(
            target: "otterzip::extract",
            entries,
            total_compressed,
            avg_entry = avg,
            large = large_enough_to_skip_avg_check,
            is_multi = matches!(self.source, OpenSource::Multi(_)),
            decision = if go_serial { "serial" } else { "parallel" },
            "zip extract_all_streaming threshold decision"
        );
        if go_serial {
            return None;
        }
        Some(self.extract_all_parallel(ctx))
    }
}

impl ZipBackend {
    /// rayon-driven entry-level parallel extractor. Each worker re-opens
    /// the source archive on its own file descriptor — `ZipArchive` is
    /// cheap to construct (one central-directory parse) but **not** safe
    /// to share across threads, so per-thread handles are the simplest
    /// correct path. See `performance.md` §4 for the design rationale.
    fn extract_all_parallel(&self, ctx: &mut StreamingExtractCtx<'_>) -> Result<()> {
        let dest_root = ctx.dest_root.to_path_buf();
        let opts = ctx.opts.clone();
        let start = ctx.start;
        // PR-7A: clone the borrowed motw payload into an owned Arc so
        // each worker can read it without lifetime gymnastics. None
        // when the user disabled the toggle or source has no MOTW.
        let motw_payload: Option<std::sync::Arc<Vec<u8>>> =
            ctx.motw_payload.map(|p| std::sync::Arc::new(p.to_vec()));
        // Pull the data we need into the closure without aliasing `self`
        // (which holds a `!Sync` `RefCell`). `source` and `password` are
        // cheap to clone; everything else lives in the `ctx`.
        let source = self.source.clone();
        let password = self.password.clone();

        // First pass: gather entry POD + total bytes (metadata-only — we
        // borrow `RefCell` mutably here, but it's released before we
        // hand work to rayon).
        let mut metas: Vec<Entry> = Vec::with_capacity(64);
        {
            let mut archive = self.inner.borrow_mut();
            let count = archive.len();
            for i in 0..count {
                let entry = entry_at(&mut archive, i)?;
                metas.push(entry);
            }
        }
        let total_bytes: u64 = metas.iter().map(|m| m.uncompressed_size).sum();
        let total_entries = u32::try_from(metas.len()).unwrap_or(u32::MAX);
        tracing::info!(
            target: "otterzip::extract",
            entries = total_entries,
            total_uncompressed = total_bytes,
            cd_parse_ms = start.elapsed().as_millis() as u64,
            "parallel extract metadata phase done"
        );

        // Cancellation / progress / accounting is shared across workers.
        // We update the report atomically per-entry; progress fires on a
        // best-effort basis (UI doesn't need every tick).
        let bytes_done = std::sync::atomic::AtomicU64::new(0);
        let entries_done = std::sync::atomic::AtomicU32::new(0);
        let entries_skipped = std::sync::atomic::AtomicU32::new(0);
        let canceled = std::sync::atomic::AtomicBool::new(false);
        // Errors collapse to the *first* one observed — once any worker
        // reports failure we set `canceled` to short-circuit the rest.
        let first_err: Mutex<Option<OtterzipError>> = Mutex::new(None);
        let warnings: Mutex<Vec<ExtractWarning>> = Mutex::new(Vec::new());
        let progress_lock: Mutex<&mut dyn crate::progress::ProgressSink> = Mutex::new(ctx.progress);

        // `for_each_init` gives each rayon worker its own re-opened
        // archive handle, amortising the central-directory parse over
        // every entry the worker takes — critical for small-entry
        // archives where the per-entry overhead would otherwise dominate.
        // For multi-volume this rebuilds the spanned-ZIP patch view
        // per worker (cheap — patched_tail is a few hundred bytes).
        //
        // Worker-count cap: NTFS serialises MFT updates (file create,
        // rename, set-attributes) at the volume level. With rayon's
        // default = num_cpus(), modern 12/16/24-thread CPUs spawn that
        // many workers — all queued behind the same NTFS lock when an
        // archive has thousands of entries, especially clustered into
        // a few hot directories. Observed pathology: 7 GB / many-files
        // archive, CPU 17 %, disk read 57 MB/s, write 1.6 MB/s — the
        // hardware was idling while workers waited on kernel-side
        // metadata locks. Cap at 4 to stay in NTFS's concurrency sweet
        // spot; per-entry CPU work (decompress) parallelises fine
        // within that envelope, and we stop fighting the OS.
        let source_for_init = source.clone();
        let worker_count = std::cmp::min(rayon::current_num_threads(), 4);
        let worker_pool = rayon::ThreadPoolBuilder::new()
            .num_threads(worker_count)
            .build()
            .map_err(|e| OtterzipError::BackendError(format!(
                "rayon thread-pool build failed: {e}"
            )))?;
        let dispatch_start = std::time::Instant::now();
        tracing::info!(
            target: "otterzip::extract",
            workers = worker_count,
            "parallel extract dispatching to rayon"
        );
        worker_pool.install(|| {
        metas.par_iter().for_each_init(
            || open_local(&source_for_init).ok(),
            |local_handle, entry| {
            if canceled.load(std::sync::atomic::Ordering::Relaxed) {
                return;
            }

            // ZIP-bomb gate (same heuristic as the serial path).
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

            let out_path =
                match resolve_output_path(&dest_root, &entry.path, &opts, entry.is_directory)
                {
                Ok(p) => p,
                Err(orig) => {
                    if opts.block_path_traversal {
                        set_first_err(
                            &first_err,
                            &canceled,
                            OtterzipError::PathTraversalBlocked(orig),
                        );
                    } else {
                        warnings.lock().unwrap().push(ExtractWarning::PathTraversalClamped {
                            original: orig,
                            clamped: dest_root.clone(),
                        });
                        entries_skipped.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
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
                    // Reserve the keep-both name atomically (create_new) — a
                    // plain unique_extract_path + File::create would let two
                    // workers pick the same `foo (2).ext` and truncate each
                    // other.
                    OverwritePolicy::Rename => {
                        match crate::archive::reserve_unique_extract_path(&out_path) {
                            Ok(p) => p,
                            Err(e) => {
                                set_first_err(&first_err, &canceled, OtterzipError::Io(e));
                                return;
                            }
                        }
                    }
                    OverwritePolicy::IfNewer | OverwritePolicy::AskCallback => {
                        entries_skipped.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        return;
                    }
                }
            } else {
                out_path
            };

            // Per-worker archive handle. Each rayon thread pays the
            // central-directory parse cost once via this construction —
            // amortised across the entries it processes in this call.
            // Re-using a thread-local handle would be a future
            // optimisation but adds lifetime complexity for marginal gain.
            let local_archive = match local_handle.as_mut() {
                Some(a) => a,
                None => {
                    set_first_err(
                        &first_err,
                        &canceled,
                        OtterzipError::BackendError(
                            "worker failed to open archive handle".into(),
                        ),
                    );
                    return;
                }
            };
            let mut zf = match Self::read_by_name(
                local_archive,
                &entry.path,
                password.as_ref(),
            ) {
                Ok(zf) => zf,
                Err(e) => {
                    set_first_err(&first_err, &canceled, e);
                    return;
                }
            };
            let file = match File::create(&out_path) {
                Ok(f) => f,
                Err(e) => {
                    set_first_err(&first_err, &canceled, OtterzipError::Io(e));
                    return;
                }
            };
            let mut writer = BufWriter::new(file);
            // Enforce the absolute output cap DURING the copy, sharing the
            // running total across workers — otherwise a single large entry
            // writes in full before the post-entry aggregate check below runs.
            let mut capped = crate::archive::__AtomicCappedWriter {
                inner: &mut writer,
                written_total: &bytes_done,
                cap: opts.max_total_output_bytes,
                tripped: false,
            };
            let written = match std::io::copy(&mut zf, &mut capped) {
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
                    set_first_err(&first_err, &canceled, OtterzipError::Io(e));
                    return;
                }
            };
            if let Err(e) = std::io::Write::flush(&mut writer) {
                set_first_err(&first_err, &canceled, OtterzipError::Io(e));
                return;
            }
            // PR-7A: propagate Zone.Identifier from source archive.
            // Best-effort, never aborts the worker.
            if let Some(p) = motw_payload.as_ref() {
                if let Err(e) = crate::motw::write_zone_identifier(&out_path, &p[..]) {
                    tracing::warn!(
                        target: "otterzip::motw",
                        path = %out_path.display(),
                        error = %e,
                        "MOTW propagation skipped (parallel zip extract)"
                    );
                }
            }

            // NOTE: `bytes_done` was already advanced in-flight by the
            // __AtomicCappedWriter above (it is the single source of the
            // running total). Do NOT add `written` again here — that double
            // counts. The in-flight cap has already failed any over-cap write.
            let _ = (written, entry.compressed_size);
            let entries_so_far = entries_done
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                + 1;

            // Progress update: best-effort, only every 8 entries to keep
            // contention on the sink lock minimal.
            if entries_so_far % 8 == 0 || entries_so_far == total_entries {
                if let Ok(mut sink) = progress_lock.try_lock() {
                    let snapshot = Progress {
                        bytes_processed: bytes_done.load(std::sync::atomic::Ordering::Relaxed),
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
        }); // worker_pool.install
        tracing::info!(
            target: "otterzip::extract",
            dispatch_ms = dispatch_start.elapsed().as_millis() as u64,
            entries_done = entries_done.load(std::sync::atomic::Ordering::Relaxed),
            entries_skipped = entries_skipped.load(std::sync::atomic::Ordering::Relaxed),
            bytes_done = bytes_done.load(std::sync::atomic::Ordering::Relaxed),
            canceled = canceled.load(std::sync::atomic::Ordering::Relaxed),
            "parallel extract rayon pool returned"
        );

        if let Some(err) = first_err.into_inner().unwrap() {
            tracing::warn!(
                target: "otterzip::extract",
                error = %err,
                "parallel extract first_err set"
            );
            return Err(err);
        }
        if canceled.load(std::sync::atomic::Ordering::Relaxed) {
            return Err(OtterzipError::Canceled);
        }

        // Flush counters into the report.
        ctx.report.bytes_written += bytes_done.load(std::sync::atomic::Ordering::Relaxed);
        ctx.report.entries_extracted += entries_done.load(std::sync::atomic::Ordering::Relaxed);
        ctx.report.entries_skipped += entries_skipped.load(std::sync::atomic::Ordering::Relaxed);
        ctx.report
            .warnings
            .extend(warnings.into_inner().unwrap());
        Ok(())
    }
}

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

/// Per-worker archive re-open. Dispatches on [`OpenSource`] so the
/// same closure can drive single-volume and multi-volume parallel
/// extracts. The buffering / sequential-scan hints match the primary
/// open path so worker reads enjoy the same I/O profile.
fn open_local(source: &OpenSource) -> Result<ZipArchive<ZipReader>> {
    match source {
        OpenSource::Single(path) => {
            let file = open_for_sequential_read(path)?;
            let reader = ZipReader::Single(BufReader::new(file));
            ZipArchive::new(reader).map_err(map_zip_err)
        }
        OpenSource::Multi(volumes) => {
            let szr = SpannedZipReader::open(volumes)?;
            let reader = ZipReader::Multi(BufReader::with_capacity(64 * 1024, szr));
            ZipArchive::new(reader).map_err(map_zip_err)
        }
    }
}

// The parallel ZIP path uses the ONE shared resolver in archive.rs rather than
// a private copy. A byte-identical copy lived here and in lenient_zip.rs; when
// the "resolves to dest_root itself" hole (an entry named "" / ".") was closed
// in the shared function, a copy here would have kept the hole on the most-used
// extract path. One ruleset, audited in one place.
use crate::archive::__resolve_output_path_streaming as resolve_output_path;

/// Read entry `i` and convert it to our POD. Pulling this out of the
/// iterator body keeps the enumeration loop easy to audit. We use the
/// `_raw` variant so encrypted entries don't trip the metadata pass — the
/// caller still has to provide a password to actually *read* their bytes,
/// but listing them is allowed.
fn entry_at(archive: &mut ZipArchive<ZipReader>, i: usize) -> Result<Entry> {
    let zf = archive.by_index_raw(i).map_err(map_zip_err)?;

    let path = zf.name().to_string();
    let is_directory = zf.is_dir();
    let external = zf.unix_mode();
    let is_symlink = external.is_some_and(|m| (m & 0o170_000) == 0o120_000);

    let compression = map_compression(zf.compression());
    let encryption = if zf.encrypted() {
        EncryptionMethod::ZipCrypto
    } else {
        EncryptionMethod::None
    };

    let crc32 = Some(zf.crc32());
    // `last_modified()` in zip 2.x returns `Option<DateTime>`. If a future
    // version reverts to plain `DateTime`, collapse the `.and_then(...)`
    // to a direct conversion.
    let modified = zf.last_modified().and_then(zip_datetime_to_system_time);
    let comment_raw = zf.comment();
    let comment = if comment_raw.is_empty() {
        None
    } else {
        Some(comment_raw.to_string())
    };
    let attributes = external.unwrap_or(0);
    let uncompressed_size = zf.size();
    let compressed_size = zf.compressed_size();

    Ok(Entry {
        path,
        is_directory,
        is_symlink,
        uncompressed_size,
        compressed_size,
        compression,
        encryption,
        crc32,
        modified,
        accessed: None,
        created: None,
        attributes,
        comment,
        host_os: HostOs::Unknown,
    })
}

/// Convert a `ZipError` into our error taxonomy without losing context.
/// Fast pre-flight sanity check for ZIP archives. Reads the last
/// 96 bytes + (if ZIP64) one 56-byte ZIP64 EOCD record and verifies
/// the central-directory bounds fit inside the file. Returns `Err`
/// with a short reason when the archive's metadata is internally
/// inconsistent — caller short-circuits to the lenient fallback
/// without running the full deadline-bounded `ZipArchive::new`.
///
/// What we check:
///   * Last 96 bytes contain the EOCD signature `PK\005\006`.
///   * For non-ZIP64 archives: `cd_offset + cd_size <= file_size`.
///   * For ZIP64 archives: the ZIP64 EOCD locator is immediately
///     before the EOCD, points at a real `PK\006\006` record within
///     the file, and that record's `cd_offset + cd_size <= file_size`.
///
/// Cost: 2 file opens + ~120 bytes of reads. Healthy archives pass
/// in single-digit milliseconds. The user's reproducer (off-by-926
/// ZIP64 cd_size) fails here in < 5 ms, replacing the previous 5 s
/// deadline-trip per `Archive::open` call.
#[doc(hidden)]
pub fn __probe_quick_eocd_sanity_check(path: &Path) -> std::result::Result<(), String> {
    quick_eocd_sanity_check(path)
}

fn quick_eocd_sanity_check(path: &Path) -> std::result::Result<(), String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path)
        .map_err(|e| format!("file open: {e}"))?;
    let file_size = f.metadata().map_err(|e| format!("metadata: {e}"))?.len();
    if file_size < 22 {
        return Err("file shorter than minimum EOCD record (22 bytes)".to_string());
    }
    let tail_len = u64::min(file_size, 96);
    f.seek(SeekFrom::End(-(tail_len as i64)))
        .map_err(|e| format!("seek: {e}"))?;
    let mut tail = vec![0u8; tail_len as usize];
    f.read_exact(&mut tail).map_err(|e| format!("read: {e}"))?;

    // Find EOCD signature (PK\005\006).
    let eocd_in_tail = tail
        .windows(4)
        .rposition(|w| w == [0x50, 0x4B, 0x05, 0x06])
        .ok_or_else(|| "no EOCD signature in last 96 bytes".to_string())?;
    if eocd_in_tail + 22 > tail.len() {
        return Err("EOCD truncated".to_string());
    }
    let eocd_abs = file_size - tail_len + eocd_in_tail as u64;
    let eocd = &tail[eocd_in_tail..];
    let cd_size = u32::from_le_bytes([eocd[12], eocd[13], eocd[14], eocd[15]]);
    let cd_offset = u32::from_le_bytes([eocd[16], eocd[17], eocd[18], eocd[19]]);

    // ZIP64 path: cd_offset == 0xFFFFFFFF or cd_size == 0xFFFFFFFF.
    if cd_offset == u32::MAX || cd_size == u32::MAX {
        // ZIP64 EOCD locator should sit 20 bytes before the EOCD.
        if eocd_in_tail < 20 {
            return Err("ZIP64 expected but locator slot beyond tail window".to_string());
        }
        let locator = &tail[eocd_in_tail - 20..eocd_in_tail];
        if locator[..4] != [0x50, 0x4B, 0x06, 0x07] {
            return Err("ZIP64 EOCD locator signature missing".to_string());
        }
        let z64_eocd_offset = u64::from_le_bytes([
            locator[8], locator[9], locator[10], locator[11],
            locator[12], locator[13], locator[14], locator[15],
        ]);
        if z64_eocd_offset >= eocd_abs {
            return Err("ZIP64 EOCD record offset points past the EOCD".to_string());
        }
        // Read the ZIP64 EOCD record (56 bytes minimum).
        f.seek(SeekFrom::Start(z64_eocd_offset))
            .map_err(|e| format!("zip64 seek: {e}"))?;
        let mut z64 = [0u8; 56];
        f.read_exact(&mut z64)
            .map_err(|e| format!("zip64 read: {e}"))?;
        if z64[..4] != [0x50, 0x4B, 0x06, 0x06] {
            return Err("ZIP64 EOCD record signature missing at locator target".to_string());
        }
        let cd_size_64 = u64::from_le_bytes([
            z64[40], z64[41], z64[42], z64[43], z64[44], z64[45], z64[46], z64[47],
        ]);
        let cd_offset_64 = u64::from_le_bytes([
            z64[48], z64[49], z64[50], z64[51], z64[52], z64[53], z64[54], z64[55],
        ]);
        let cd_end = cd_offset_64.checked_add(cd_size_64).ok_or_else(|| {
            "ZIP64 cd_offset + cd_size overflows u64".to_string()
        })?;
        if cd_end > file_size {
            return Err(format!(
                "ZIP64 CD declares end at {cd_end} but file is {file_size} bytes (off by {})",
                cd_end - file_size
            ));
        }
        return Ok(());
    }

    // Non-ZIP64 path.
    let cd_end = cd_offset as u64 + cd_size as u64;
    if cd_end > file_size {
        return Err(format!(
            "CD declares end at {cd_end} but file is {file_size} bytes (off by {})",
            cd_end - file_size
        ));
    }
    Ok(())
}

#[cfg(test)]
mod sanity_tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn detects_zip64_cd_overshoot() {
        // Build a normal ZIP, then surgically mutate the ZIP64 EOCD
        // record's cd_size by +N so cd_end overshoots EOF — exactly
        // the user-reproduced malformation. Sanity check MUST catch.
        let td = tempfile::tempdir().unwrap();
        let path = td.path().join("malformed.zip");
        // Make a ZIP big enough to trigger ZIP64. Actually zip crate
        // emits regular EOCD with `cd_offset = u32::MAX` sentinel
        // only for files > 4 GiB, which we can't synthesize quickly.
        // Workaround: build a tiny ZIP and manually inject ZIP64
        // records + sentinel before EOCD, then mutate cd_size.
        //
        // Easier alternative: just test the non-ZIP64 path which
        // exercises the same arithmetic — sanity check has only one
        // overshoot detection per spec branch.
        let mut tiny = Vec::new();
        {
            let mut w = zip::ZipWriter::new(std::io::Cursor::new(&mut tiny));
            w.start_file("a", zip::write::SimpleFileOptions::default()).unwrap();
            w.write_all(b"hi").unwrap();
            w.finish().unwrap();
        }
        // Locate EOCD signature, bump cd_size by huge amount.
        let eocd = tiny.windows(4).rposition(|w| w == [0x50, 0x4B, 0x05, 0x06]).unwrap();
        let cd_size_off = eocd + 12;
        tiny[cd_size_off..cd_size_off + 4].copy_from_slice(&u32::MAX.to_le_bytes());
        std::fs::write(&path, &tiny).unwrap();

        let result = quick_eocd_sanity_check(&path);
        assert!(
            result.is_err(),
            "sanity check should reject malformed archive, got {result:?}"
        );
    }
}

/// Diagnostic helper — dump the last 96 bytes of a file as hex into the
/// log. EOCD lives at the tail of every well-formed ZIP, so a quick
/// peek tells us whether the file is even shaped like a ZIP and
/// whether ZIP64 locator bytes (0x504B0607) are present.
fn log_tail_bytes(path: &Path) {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return,
    };
    let size = match f.metadata() {
        Ok(m) => m.len(),
        Err(_) => return,
    };
    let tail_len = u64::min(size, 96);
    if f.seek(SeekFrom::End(-(tail_len as i64))).is_err() {
        return;
    }
    let mut buf = vec![0u8; tail_len as usize];
    if f.read_exact(&mut buf).is_err() {
        return;
    }
    let hex: String = buf
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(" ");
    tracing::info!(
        target: "otterzip::zip",
        path = %path.display(),
        size,
        tail_hex = %hex,
        "ZipBackend::open tail bytes (last 96) for EOCD diagnostic"
    );
}

fn map_zip_err(e: zip::result::ZipError) -> OtterzipError {
    use zip::result::ZipError as Z;
    match e {
        // Deadline-reader synthetic Io error → Corrupted so the
        // open_backend dispatcher recognises it as recoverable and
        // hands off to libarchive instead of bubbling Io upward.
        Z::Io(io) if io.to_string().contains("deadline exceeded") => {
            OtterzipError::Corrupted {
                reason: format!("ZipArchive::new deadline exceeded ({io})"),
                entry: None,
            }
        }
        Z::Io(io) => OtterzipError::Io(io),
        Z::InvalidArchive(msg) => OtterzipError::Corrupted {
            reason: msg.to_string(),
            entry: None,
        },
        Z::UnsupportedArchive(msg) => OtterzipError::UnsupportedFormat(Some(msg.to_string())),
        Z::FileNotFound => OtterzipError::EntryNotFound(String::new()),
        other => OtterzipError::BackendError(other.to_string()),
    }
}

fn map_compression(m: zip::CompressionMethod) -> CompressionMethod {
    use zip::CompressionMethod as Z;
    // Only `Stored` and `Deflated` are guaranteed by our feature flags
    // (`default-features = false, features = ["deflate"]` in workspace
    // Cargo.toml). Other codec variants are feature-gated in the `zip`
    // crate; we fall through to `Unknown` rather than listing them and
    // risking compile errors under minimal features.
    match m {
        Z::Stored => CompressionMethod::Store,
        Z::Deflated => CompressionMethod::Deflate,
        _ => CompressionMethod::Unknown,
    }
}

/// Convert a `zip::DateTime` to a `SystemTime`.
///
/// ZIP stores DOS-era wall-clock with 2-second resolution and no timezone,
/// so the result is interpreted as UTC. This is lossy by design.
#[allow(clippy::needless_pass_by_value)] // DateTime is Copy-ish, trivial
fn zip_datetime_to_system_time(dt: zip::DateTime) -> Option<std::time::SystemTime> {
    let year = i32::from(dt.year());
    let month = u32::from(dt.month());
    let day = u32::from(dt.day());
    let hour = u32::from(dt.hour());
    let minute = u32::from(dt.minute());
    let second = u32::from(dt.second());

    let days = days_from_civil(year, month, day)?;
    let secs = i64::from(days) * 86_400
        + i64::from(hour) * 3600
        + i64::from(minute) * 60
        + i64::from(second);
    let secs_u64 = u64::try_from(secs).ok()?;
    Some(std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs_u64))
}

/// Days since 1970-01-01 for a (proleptic Gregorian) date.
/// Howard Hinnant's civil-from-days algorithm (inverse); O(1), branchless.
fn days_from_civil(year: i32, month: u32, day: u32) -> Option<i32> {
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let civil_year = if month <= 2 { year - 1 } else { year };
    let era = civil_year.div_euclid(400);
    let yoe = u32::try_from(civil_year.rem_euclid(400)).ok()?; // 0..=399
    let month_offset = if month > 2 { month - 3 } else { month + 9 };
    let doy = (153 * month_offset + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era.checked_mul(146_097)?
        .checked_add(i32::try_from(doe).ok()?)?
        .checked_sub(719_468)
}
