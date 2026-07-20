//! Archive handle and operations. See `rust-core-api.md` §1.1.
//!
//! Sprint 1 scope: ZIP read path only. `open_with_password`, `create`,
//! `open_multi`, and mutation APIs return `OtterzipError::UnsupportedFormat`
//! or `FeatureDisabled` — they will be filled in on subsequent sprints.

use std::fs;
use std::io::{BufWriter, Write};
use std::path::{Component, Path, PathBuf};
use std::time::Instant;

use zeroize::Zeroizing;

use crate::backends::{
    add_dir_recursive_through, open_backend, open_writer, ArchiveBackend, ArchiveWriter,
    StreamingExtractCtx,
};
use crate::entry::EntryIter;
use crate::error::{Result, OtterzipError};
use crate::format::{detect, ArchiveFormat};
use crate::options::{CreateOptions, ExtractOptions, OverwritePolicy};
use crate::progress::{Progress, ProgressPhase, ProgressSink};

#[repr(u32)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum OpenMode {
    Read = 0,
    CreateNew = 1,
    CreateOrOverwrite = 2,
    Update = 3,
}

/// Top-level archive handle. Opaque to external consumers.
pub struct Archive {
    path: PathBuf,
    format: ArchiveFormat,
    mode: OpenMode,
    /// Held with `zeroize` semantics so the bytes are wiped on drop. The
    /// password lives here for the duration of the handle and is forwarded
    /// to the backend on read; on write it is reserved for future
    /// encrypted-archive creation (Sprint 4+ depending on format).
    #[allow(dead_code)]
    password: Option<Zeroizing<String>>,
    /// Sibling-volume paths discovered by `open_multi`. `None` means
    /// the handle came from `open` (single-volume) — `volume_count`
    /// reports `Some(1)` in that case.
    volumes: Option<Vec<VolumeInfo>>,
    /// Write-mode only. `Some(n)` when [`CreateOptions::volume_size_bytes`]
    /// requested split output: the writer targets a single temp file and
    /// `commit` slices it into `.001/.002/…` segments of ≤ `n` bytes.
    /// `None` for read-mode handles and for un-split creation.
    volume_size_bytes: Option<u64>,
    /// Phase 6+ — captured from `CreateOptions::exclude_system_metadata`
    /// at writer creation. Only consulted by `add_dir_recursive`; reader
    /// handles ignore it. `false` for read-mode archives.
    exclude_system_metadata: bool,
    inner: ArchiveInner,
}

/// Internal split between read-side and write-side backends. Public API
/// callers see [`Archive`] uniformly; runtime errors surface when a
/// caller invokes the wrong family for the current mode (e.g. `add_file`
/// on a read-mode archive).
enum ArchiveInner {
    Reader(Box<dyn ArchiveBackend + Send>),
    Writer(Box<dyn ArchiveWriter + Send>),
}

impl std::fmt::Debug for Archive {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mode_kind = match self.inner {
            ArchiveInner::Reader(_) => "reader",
            ArchiveInner::Writer(_) => "writer",
        };
        f.debug_struct("Archive")
            .field("path", &self.path)
            .field("format", &self.format)
            .field("mode", &self.mode)
            .field("kind", &mode_kind)
            .field("password", &self.password.as_ref().map(|_| "***"))
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone)]
pub struct ExtractReport {
    pub entries_extracted: u32,
    pub entries_skipped: u32,
    pub bytes_written: u64,
    pub warnings: Vec<ExtractWarning>,
    pub elapsed: std::time::Duration,
}

/// Volume metadata for split-archive read (`schema.md` §2.3,
/// `rust-core-api.md` §1.1 `Archive::open_multi`).
#[derive(Debug, Clone)]
pub struct VolumeInfo {
    pub index: u32,
    pub total: Option<u32>,
    pub path: PathBuf,
    pub size_bytes: u64,
}

/// Outcome of [`Archive::test`]. One row per entry visited; corrupted
/// entries surface in `corrupted_entries` so callers can re-prompt the
/// user with specifics rather than a single yes/no flag.
#[derive(Debug, Clone, Default)]
pub struct TestReport {
    pub entries_tested: u32,
    pub entries_corrupted: u32,
    pub corrupted_entries: Vec<String>,
    pub elapsed: std::time::Duration,
}

/// Top-level layout of an archive's entries, used by Smart Extract to
/// decide between a flat extract and a `<stem>/`-wrapped extract.
///
/// Mirrors Bandizip's `알아서 풀기` heuristic — a single top-level
/// directory means the archive creator already wrapped everything in a
/// folder, so extracting into the current directory produces a single
/// tidy folder. Any deviation (multiple roots, or files at the root)
/// indicates the archive expects to be wrapped before being unpacked.
#[derive(Debug, Clone)]
pub enum RootLayout {
    /// Every probed entry lives under the same top-level component
    /// (the carried string, without the trailing slash).
    SingleFolder(String),
    /// Either multiple top-level components, or at least one entry at
    /// the archive root.
    Flat,
}

#[derive(Debug, Clone)]
pub enum ExtractWarning {
    PathTraversalClamped { original: String, clamped: PathBuf },
    EncodingFallback { lossy: String },
    DuplicateEntry { path: String },
    SymlinkSkipped { path: String, target: String },
    /// The strict (fast-path) backend rejected the archive; we fell
    /// back to libarchive's lenient parser. Carries the backend name
    /// for UI display and the original rejection reason for audit
    /// logs / bug reports.
    RecoveredWithFallback { backend: String, original_error: String },
}

impl Archive {
    /// Open an existing archive for reading.
    ///
    /// Dispatches to the appropriate backend after running magic-byte
    /// format detection. Sprint 3 wires ZIP / 7z / TAR / .tar.gz / .tar.bz2
    /// / .tar.xz; RAR remains unsupported until Sprint 4 (licence review).
    pub fn open(path: impl AsRef<Path>, mode: OpenMode) -> Result<Self> {
        Self::open_inner(path.as_ref(), mode, None)
    }

    /// Open an archive whose entries (or central directory) are encrypted.
    ///
    /// The password is held in [`Zeroizing<String>`] so its bytes are wiped
    /// when the `Archive` drops. Per `domain-model.md` §5 we never log or
    /// format the password value, even on error.
    pub fn open_with_password(
        path: impl AsRef<Path>,
        mode: OpenMode,
        password: impl Into<Zeroizing<String>>,
    ) -> Result<Self> {
        let pw = password.into();
        Self::open_inner(path.as_ref(), mode, Some(pw))
    }

    fn open_inner(
        path: &Path,
        mode: OpenMode,
        password: Option<Zeroizing<String>>,
    ) -> Result<Self> {
        if mode != OpenMode::Read {
            return Err(OtterzipError::FeatureDisabled(
                "open() requires OpenMode::Read; use Archive::create() for write modes",
            ));
        }
        let format = detect(path)?;
        let backend = open_backend(path, format, password.as_ref())?;
        Ok(Self {
            path: path.to_path_buf(),
            format,
            mode,
            password,
            volumes: None,
            volume_size_bytes: None,
            exclude_system_metadata: false,
            inner: ArchiveInner::Reader(backend),
        })
    }

    /// Open a split / spanned archive given an ordered list of every
    /// volume path (disk 0 → last). The volumes are presented to the
    /// underlying ZIP backend as a single virtual byte stream so the
    /// central directory and per-entry local headers in the last
    /// volume can resolve absolute byte positions across the whole
    /// concatenation. Works for the "split byte-stream" shape of
    /// spanned ZIPs produced by WinRAR / 7-Zip / Info-ZIP `zip -s`;
    /// real APPNOTE-spanned archives with `disk_number_start > 0`
    /// references in the central directory may still trip the upstream
    /// crate's single-disk assertion (v1.1).
    ///
    /// 7z spanning (`name.7z.001..NNN`) is not supported on this path
    /// yet — `sevenz-rust2` does not expose a multi-volume reader for
    /// our feature flags. Such inputs return `UnsupportedFormat`.
    pub fn open_multi(volumes: &[PathBuf], mode: OpenMode) -> Result<Self> {
        if mode != OpenMode::Read {
            return Err(OtterzipError::FeatureDisabled(
                "open_multi() requires OpenMode::Read",
            ));
        }
        let first = match volumes.first() {
            Some(p) => p.as_path(),
            None => {
                return Err(OtterzipError::InvalidArgument(
                    "open_multi() requires at least one volume",
                ));
            }
        };
        let format = detect(first)?;
        match format {
            ArchiveFormat::Zip | ArchiveFormat::Zipx => {
                let backend = crate::backends::zip::ZipBackend::open_multi(volumes, None)?;
                let volumes_meta: Vec<VolumeInfo> = volumes
                    .iter()
                    .enumerate()
                    .map(|(i, p)| VolumeInfo {
                        index: u32::try_from(i + 1).unwrap_or(u32::MAX),
                        total: u32::try_from(volumes.len()).ok(),
                        path: p.clone(),
                        size_bytes: fs::metadata(p).map(|m| m.len()).unwrap_or(0),
                    })
                    .collect();
                Ok(Self {
                    path: first.to_path_buf(),
                    format,
                    mode,
                    password: None,
                    volumes: Some(volumes_meta),
                    volume_size_bytes: None,
                    exclude_system_metadata: false,
                    inner: ArchiveInner::Reader(Box::new(backend)),
                })
            }
            _ => Err(OtterzipError::UnsupportedFormat(Some(format!(
                "multi-volume read for format {format:?} is not supported (planned for v1.1)"
            )))),
        }
    }

    /// Convenience wrapper over [`Archive::open_multi`] that
    /// auto-discovers sibling volumes around `first_volume` using the
    /// extension conventions Rust knows about (ZIP `.z01..zN + .zip`,
    /// 7z `.7z.001..NNN`). C# already runs its own discovery via
    /// `SplitArchiveDetector` so the FFI side typically calls
    /// `open_multi` with an explicit list; this entry point exists for
    /// direct Rust API consumers and for the round-trip test path.
    pub fn open_multi_auto(first_volume: impl AsRef<Path>, mode: OpenMode) -> Result<Self> {
        let first = first_volume.as_ref();
        let discovered = discover_split_volumes(first);
        let paths: Vec<PathBuf> = if discovered.is_empty() {
            vec![first.to_path_buf()]
        } else {
            discovered.iter().map(|v| v.path.clone()).collect()
        };
        Self::open_multi(&paths, mode)
    }

    /// Number of volumes discovered. `Some(1)` when the archive is
    /// single-volume (the common case), `Some(n)` when `open_multi`
    /// found siblings, `None` when the format does not encode the count.
    #[must_use]
    pub fn volume_count(&self) -> Option<u32> {
        self.volumes
            .as_ref()
            .map(|v| u32::try_from(v.len()).unwrap_or(u32::MAX))
            .or(Some(1))
    }

    /// Per-volume metadata. Empty when the handle came from `open`.
    #[must_use]
    pub fn volumes(&self) -> &[VolumeInfo] {
        self.volumes.as_deref().unwrap_or(&[])
    }

    /// Create a new archive in `CreateNew` or `CreateOrOverwrite` mode.
    ///
    /// The returned handle is in **write mode** — calling [`Archive::extract_all`]
    /// or [`Archive::entries`] on it returns [`OtterzipError::InvalidArgument`].
    /// Use [`Archive::add_file`] / [`Archive::add_dir_recursive`] /
    /// [`Archive::commit`] to populate.
    pub fn create(path: impl AsRef<Path>, mut opts: CreateOptions) -> Result<Self> {
        // Fail closed: a password supplied with no explicit encryption method
        // must NEVER silently produce a PLAINTEXT archive. Treat "password
        // present, method unset" as a request for the strong default (AES-256)
        // rather than dropping encryption. The shell UI / CLI already pair the
        // two, but this guards any other FFI consumer from the silent-plaintext
        // footgun (the backends gate on `password.is_some() && encryption !=
        // None`, so without this they'd write an unencrypted archive).
        if opts.password.is_some()
            && opts.encryption == crate::format::EncryptionMethod::None
        {
            opts.encryption = crate::format::EncryptionMethod::Aes256;
        }
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent)?;
            }
        }
        let password = opts.password.clone();
        let exclude_system_metadata = opts.exclude_system_metadata;
        // When a volume size is requested the backend writes one contiguous
        // temp archive; `commit` then slices it into `name.ext.001/.002/…`
        // segments. 7-Zip / Bandizip read that `.NNN` set natively and the
        // GUI's raw-split concatenator reassembles it on extract.
        let volume_size_bytes = opts.volume_size_bytes.filter(|&v| v > 0);
        let write_path = match volume_size_bytes {
            Some(_) => split_write_path(path),
            None => path.to_path_buf(),
        };
        let writer = open_writer(&write_path, &opts, password.as_ref())?;
        Ok(Self {
            path: path.to_path_buf(),
            format: opts.format,
            mode: OpenMode::CreateOrOverwrite,
            password,
            volumes: None,
            volume_size_bytes,
            exclude_system_metadata,
            inner: ArchiveInner::Writer(writer),
        })
    }

    /// Append a single file to a write-mode archive.
    pub fn add_file(&mut self, src: impl AsRef<Path>, entry_path: &str) -> Result<()> {
        let writer = self.writer_mut()?;
        let src = src.as_ref();
        let meta = fs::metadata(src)?;
        let file = fs::File::open(src)?;
        let mut reader = std::io::BufReader::new(file);
        writer.add_entry(entry_path, &mut reader, Some(meta.len()), false)
    }

    /// Remove an entry by name from a write-mode archive. Phase 8 G7
    /// scope is **stage-and-commit**: removals queue inside the writer
    /// and apply during `commit`. ZIP/7z writers honour the queue;
    /// streaming TAR writers reject removal with `FeatureDisabled`
    /// because rewinding a streaming sink is not supported.
    pub fn remove_entry(&mut self, entry_path: &str) -> Result<()> {
        let writer = self.writer_mut()?;
        writer.queue_removal(entry_path)
    }

    /// Append a directory recursively. `entry_prefix` is empty for "files
    /// directly at archive root", or e.g. `"docs"` to nest the source under
    /// `docs/...` inside the archive.
    pub fn add_dir_recursive<S: ProgressSink>(
        &mut self,
        src: impl AsRef<Path>,
        entry_prefix: &str,
        progress: Option<&mut S>,
    ) -> Result<()> {
        let exclude_meta = self.exclude_system_metadata;
        let writer = self.writer_mut()?;
        let src = src.as_ref();
        // Promote the user's `&mut S` (statically typed) into a
        // `&mut dyn ProgressSink` so the helper can stay format-agnostic
        // without dragging the generic parameter through every layer.
        let sink_dyn: Option<&mut dyn ProgressSink> =
            progress.map(|s| s as &mut dyn ProgressSink);
        add_dir_recursive_through(writer, src, entry_prefix, false, exclude_meta, sink_dyn)
    }

    /// Finalise a write-mode archive and flush to disk. Consumes the handle.
    ///
    /// In split mode (`CreateOptions::volume_size_bytes`) the backend has
    /// been writing to a temp file; this slices it into `.001/.002/…`
    /// segments and removes the temp. Any partial segments are cleaned up
    /// if slicing fails midway.
    pub fn commit(mut self) -> Result<()> {
        let split = self.volume_size_bytes;
        let final_path = self.path.clone();
        let writer = match std::mem::replace(
            &mut self.inner,
            ArchiveInner::Reader(Box::new(EmptyBackend)),
        ) {
            ArchiveInner::Writer(w) => w,
            ArchiveInner::Reader(_) => {
                return Err(OtterzipError::InvalidArgument(
                    "commit called on a read-mode archive",
                ))
            }
        };
        match split {
            None => writer.commit(),
            Some(volume_size) => {
                let temp = split_write_path(&final_path);
                let result = writer
                    .commit()
                    .and_then(|()| split_into_volumes(&temp, &final_path, volume_size));
                // The temp archive is transient regardless of outcome.
                let _ = fs::remove_file(&temp);
                result.map(|_segments| ())
            }
        }
    }

    /// Discard the in-progress writer. Removes the partially-written file
    /// so callers can retry without manual cleanup.
    pub fn rollback(mut self) -> Result<()> {
        // Drop the writer so the file handle releases.
        let path = self.path.clone();
        let split = self.volume_size_bytes;
        self.inner = ArchiveInner::Reader(Box::new(EmptyBackend));
        if split.is_some() {
            // Split writers target a temp file; segments only materialise on
            // a successful commit. Clear the temp plus any segments a partial
            // commit may have left, best-effort.
            let _ = fs::remove_file(split_write_path(&path));
            for idx in 1u32..=999 {
                let seg = volume_segment_path(&path, idx);
                if !seg.exists() {
                    break;
                }
                let _ = fs::remove_file(seg);
            }
            return Ok(());
        }
        match fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(OtterzipError::Io(e)),
        }
    }

    fn writer_mut(&mut self) -> Result<&mut (dyn ArchiveWriter + Send)> {
        match &mut self.inner {
            ArchiveInner::Writer(w) => Ok(w.as_mut()),
            ArchiveInner::Reader(_) => Err(OtterzipError::InvalidArgument(
                "operation requires write mode (Archive::create)",
            )),
        }
    }

    fn reader(&self) -> Result<&(dyn ArchiveBackend + Send)> {
        match &self.inner {
            ArchiveInner::Reader(r) => Ok(r.as_ref()),
            ArchiveInner::Writer(_) => Err(OtterzipError::InvalidArgument(
                "operation requires read mode (Archive::open)",
            )),
        }
    }

    #[must_use]
    pub const fn format(&self) -> ArchiveFormat {
        self.format
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    #[must_use]
    pub const fn mode(&self) -> OpenMode {
        self.mode
    }

    /// Number of entries this archive declares. Cheap on backends that
    /// keep the central directory in memory (ZIP / 7z), `O(n)` linear
    /// scan on streaming-only formats (TAR family).
    pub fn entry_count(&self) -> Result<u64> {
        let n = self.reader()?.entries()?.count();
        Ok(n as u64)
    }

    /// `true` if any entry in the archive declares an encryption method
    /// other than `None`. Walks the entry list (cheap for ZIP/7z, linear
    /// scan for tar.* — but `tar` doesn't carry encryption metadata
    /// anyway, so this returns `false` immediately on those formats).
    pub fn is_encrypted(&self) -> Result<bool> {
        self.reader()?.is_encrypted_fast()
    }

    /// `Some(true)` for 7z archives marked solid; `Some(false)` for
    /// non-solid 7z; `None` for any other format (the metric is
    /// 7z-specific). Phase 8 keeps this as a forward-looking metadata
    /// hook — the 7z backend currently always reports `None` because
    /// `sevenz-rust` 0.6 does not surface the solid flag through its
    /// public API. When it does (or we swap to a fork that exposes it),
    /// the implementation lights up here without consumer changes.
    #[must_use]
    pub fn is_solid(&self) -> Option<bool> {
        match self.format {
            ArchiveFormat::SevenZ => None,
            _ => None,
        }
    }

    /// Archive-level comment, if any. ZIP carries an optional EOCD
    /// comment; 7z and TAR do not. Returns `None` for unsupported
    /// formats. Sprint 8: ZIP comment retrieval is best-effort — the
    /// `zip` 2.x crate does not currently expose the EOCD comment via
    /// its high-level API, so this returns `None` until we either
    /// upstream that or parse the EOCD ourselves.
    #[must_use]
    pub fn comment(&self) -> Option<String> {
        None
    }

    /// Walk every entry, decompress its body into a CRC32 sink, and
    /// report which entries fail their stored checksum. For formats that
    /// don't carry per-entry CRCs (TAR family — checksums live in the
    /// outer compressor's frame instead) we still decompress as a
    /// "does it parse" smoke check.
    ///
    /// Per `rust-core-api.md` §1.1 / `ffi-api.md` §10.
    pub fn test<S: ProgressSink>(&self, mut progress: Option<&mut S>) -> Result<TestReport> {
        let start = Instant::now();
        let mut report = TestReport::default();

        let entries: Vec<_> = self
            .reader()?
            .entries()?
            .collect::<Result<Vec<_>>>()?;
        let total_entries = u32::try_from(entries.len()).unwrap_or(u32::MAX);

        for (i, entry) in entries.iter().enumerate() {
            if let Some(sink) = progress.as_deref_mut() {
                let snapshot = Progress {
                    bytes_processed: 0,
                    bytes_total: 0,
                    entries_processed: u32::try_from(i).unwrap_or(u32::MAX),
                    entries_total: total_entries,
                    current_entry: Some(entry.path.clone()),
                    phase: ProgressPhase::Reading,
                    elapsed: start.elapsed(),
                    current_entry_bytes_processed: 0,
                    current_entry_bytes_total: 0,
                };
                if !sink.update(&snapshot) {
                    return Err(OtterzipError::Canceled);
                }
            }

            if entry.is_directory || entry.is_symlink {
                continue;
            }

            let mut crc_sink = Crc32Sink::new();
            match self.reader()?.extract_entry(&entry.path, &mut crc_sink) {
                Ok(_) => {
                    report.entries_tested += 1;
                    // Skip the CRC comparison for encrypted entries. AES (AE-2)
                    // ZIP stores a 0 CRC placeholder — the AES HMAC provides
                    // integrity and was already validated by extract_entry
                    // above (a tampered ciphertext fails there and lands in the
                    // Err arm). Comparing the placeholder 0 against the real
                    // decrypted CRC would false-positive every encrypted entry
                    // as corrupt, which also made compress+verify falsely fail
                    // on password-protected archives (CLI-C1 / app verify).
                    if entry.encryption == crate::format::EncryptionMethod::None {
                        if let Some(expected) = entry.crc32 {
                            if crc_sink.finalize() != expected {
                                report.entries_corrupted += 1;
                                report.corrupted_entries.push(entry.path.clone());
                            }
                        }
                    }
                }
                // A missing/incorrect password is an archive-level auth
                // failure, NOT per-entry corruption. Surfacing it as
                // "N entries CORRUPTED" tells the user their (fine)
                // encrypted archive is damaged (CLI-C1). Propagate it so
                // callers can prompt for / report the password distinctly.
                Err(OtterzipError::WrongPassword) => {
                    return Err(OtterzipError::WrongPassword);
                }
                Err(_) => {
                    // Couldn't even decompress — definitely corrupted.
                    report.entries_corrupted += 1;
                    report.corrupted_entries.push(entry.path.clone());
                }
            }
        }

        report.elapsed = start.elapsed();
        Ok(report)
    }

    /// Inspect the archive's top-level layout to decide whether a
    /// destination-side "smart extract" should drop entries into the
    /// current directory or wrap them in `<stem>/`.
    ///
    /// Returns:
    ///   * [`RootLayout::SingleFolder(name)`] when every entry probed
    ///     lives under the same top-level component `name/` — a flat
    ///     extract into the current directory will yield a single tidy
    ///     `name/` subfolder, mirroring what the archive creator
    ///     intended.
    ///   * [`RootLayout::Flat`] when probed entries include either a
    ///     root-level file, or multiple top-level directories. Smart
    ///     extract then creates a `<stem>/` wrapper to avoid littering
    ///     the destination with loose files.
    ///
    /// Probes up to 1024 entries — sufficient sample for the
    /// single-root vs flat decision on every real archive we've seen,
    /// and cheap because the iteration touches metadata only (no
    /// decompression). If the archive has more entries and the first
    /// 1024 are consistent, we trust the pattern; this matches
    /// Bandizip's `알아서 풀기` behavior, which also samples rather
    /// than scans exhaustively.
    pub fn detect_root_layout(&self) -> Result<RootLayout> {
        const MAX_ENTRIES_PROBED: usize = 1024;
        let mut common_root: Option<String> = None;
        let mut probed = 0usize;

        for entry_result in self.entries()?.take(MAX_ENTRIES_PROBED) {
            let entry = entry_result?;
            probed += 1;

            // OtterZip paths are forward-slash normalised at backend
            // ingest (see `glossary.md`). The split below relies on
            // that invariant.
            let top = match entry.path.find('/') {
                Some(idx) => &entry.path[..idx],
                None => {
                    // Entry at archive root with no sub-component —
                    // by definition flat. Bail early; no need to
                    // examine more entries.
                    return Ok(RootLayout::Flat);
                }
            };

            // Defensive: empty top-level component (would happen for
            // a leading-slash entry, which our path normaliser rejects
            // but third-party tools sometimes emit).
            if top.is_empty() {
                return Ok(RootLayout::Flat);
            }

            match &common_root {
                None => common_root = Some(top.to_string()),
                Some(root) => {
                    if root != top {
                        return Ok(RootLayout::Flat);
                    }
                }
            }
        }

        match common_root {
            Some(root) if probed > 0 => Ok(RootLayout::SingleFolder(root)),
            // Empty archive (no entries at all) — treat as flat so the
            // smart-extract caller creates a `<stem>/` wrapper rather
            // than silently doing nothing.
            _ => Ok(RootLayout::Flat),
        }
    }

    /// Stream a single entry's decompressed bytes. Implements the
    /// `read_entry` slot from `rust-core-api.md` §1.1.
    ///
    /// The returned reader buffers the entry content in memory for
    /// archives where random access requires consuming the underlying
    /// file (ZIP encrypted entries, sevenz, tar.*); for the plain-ZIP
    /// path it remains a thin Cursor over the decompressed bytes. Phase
    /// 8 ships this as the documented API; true streaming for >1 GiB
    /// entries lands when the backend trait grows a lifetime-aware hook.
    pub fn read_entry(&self, entry_path: &str) -> Result<Box<dyn std::io::Read + Send + '_>> {
        self.reader()?.open_entry_stream(entry_path)
    }

    /// Iterate over all entries in the archive.
    pub fn entries(&self) -> Result<EntryIter<'_>> {
        let inner = self.reader()?.entries()?;
        Ok(EntryIter { inner })
    }

    /// Extract every entry under `opts.destination`.
    ///
    /// Sequential for Sprint 1; Sprint 3 will swap in `rayon` parallelism.
    /// Respects `overwrite`, `block_path_traversal`, and `follow_symlinks`
    /// (the latter currently defaults to *skip* for symlink entries — we
    /// will restore symlinks in Sprint 2 once the platform abstraction
    /// lands).
    #[allow(clippy::too_many_lines)]
    pub fn extract_all<S: ProgressSink>(
        &self,
        opts: &ExtractOptions,
        mut progress: Option<&mut S>,
    ) -> Result<ExtractReport> {
        let start = Instant::now();
        let dest_root = canonical_root(&opts.destination)?;
        tracing::info!(
            target: "otterzip::extract",
            source = %self.path.display(),
            destination = %dest_root.display(),
            format = ?self.format,
            volumes = self.volumes.as_ref().map_or(1, |v| v.len()),
            "extract_all begin"
        );

        let mut report = ExtractReport {
            entries_extracted: 0,
            entries_skipped: 0,
            bytes_written: 0,
            warnings: Vec::new(),
            elapsed: std::time::Duration::ZERO,
        };

        // Streaming-only backends (tar.*) install their own extractor — saves
        // an O(n²) re-scan per entry on big tarballs. We adapt the caller's
        // `Option<&mut S>` to a stable `&mut dyn ProgressSink` by routing
        // through a no-op shim when the caller didn't pass one.
        let mut noop_sink = NoopSink;
        let sink_dyn: &mut dyn ProgressSink = match progress.as_deref_mut() {
            Some(s) => s,
            None => &mut noop_sink,
        };
        // PR-7A: capture source archive's Zone.Identifier once. Cheap
        // (≤1 KB usually). Skipped when the user disabled the toggle so
        // we don't even open the ADS handle.
        let motw_payload = if opts.preserve_zone_identifier {
            crate::motw::read_zone_identifier(&self.path)
        } else {
            None
        };

        let mut ctx = StreamingExtractCtx {
            dest_root: &dest_root,
            opts,
            progress: sink_dyn,
            report: &mut report,
            start,
            motw_payload: motw_payload.as_deref(),
        };
        if let Some(streamed) = self.reader()?.extract_all_streaming(&mut ctx) {
            streamed?;
            report.elapsed = start.elapsed();
            tracing::info!(
                target: "otterzip::extract",
                elapsed_ms = report.elapsed.as_millis() as u64,
                entries = report.entries_extracted,
                bytes = report.bytes_written,
                "extract_all done (streaming path)"
            );
            return Ok(report);
        }
        tracing::debug!(
            target: "otterzip::extract",
            "extract_all_streaming returned None — falling back to serial per-entry loop"
        );

        // Default per-entry path (random-access backends: ZIP / 7z).
        let mut entries = Vec::new();
        {
            let iter = self.reader()?.entries()?;
            for item in iter {
                entries.push(item?);
            }
        }

        let total_bytes: u64 = entries.iter().map(|e| e.uncompressed_size).sum();
        let total_entries = u32::try_from(entries.len()).unwrap_or(u32::MAX);
        let mut bomb_monitor = __BombMonitor::new(opts);

        for (i, entry) in entries.iter().enumerate() {
            if let Some(sink) = progress.as_deref_mut() {
                let snapshot = Progress {
                    bytes_processed: report.bytes_written,
                    bytes_total: total_bytes,
                    entries_processed: u32::try_from(i).unwrap_or(u32::MAX),
                    entries_total: total_entries,
                    current_entry: Some(entry.path.clone()),
                    phase: ProgressPhase::Writing,
                    elapsed: start.elapsed(),
                    current_entry_bytes_processed: 0,
                    current_entry_bytes_total: 0,
                };
                if !sink.update(&snapshot) {
                    return Err(OtterzipError::Canceled);
                }
            }

            // ZIP-bomb gate. `domain-model.md` §5 specifies a pre-extraction
            // ratio check; we only enforce it on real (data-bearing) entries
            // and skip directories / zero-sized files where the metric is
            // either undefined (compressed=0) or trivially huge.
            if let Some(err) = check_zip_bomb(entry, opts) {
                if let OtterzipError::ZipBombSuspected { entry: name, ratio, limit } = &err {
                    tracing::warn!(
                        target: "otterzip::security",
                        event = "zip_bomb_per_entry",
                        entry = %name,
                        ratio = *ratio,
                        limit = *limit,
                        "per-entry compression ratio exceeded threshold"
                    );
                }
                return Err(err);
            }

            if entry.is_symlink && !opts.follow_symlinks {
                report.warnings.push(ExtractWarning::SymlinkSkipped {
                    path: entry.path.clone(),
                    target: String::new(),
                });
                report.entries_skipped += 1;
                continue;
            }

            let out_path = match resolve_output_path(&dest_root, &entry.path, opts, entry.is_directory)
        {
                Ok(p) => p,
                Err(Skipped::Traversal(orig)) => {
                    if opts.block_path_traversal {
                        // OWASP A09 trace site — path-traversal block. The
                        // event is rare and security-relevant, so we log
                        // at WARN with the original entry path (not the
                        // resolved one — that's the attacker-controlled
                        // string we want to surface).
                        tracing::warn!(
                            target: "otterzip::security",
                            event = "path_traversal_blocked",
                            entry = %orig,
                            "rejected entry escaping destination root"
                        );
                        return Err(OtterzipError::PathTraversalBlocked(orig));
                    }
                    tracing::warn!(
                        target: "otterzip::security",
                        event = "path_traversal_clamped",
                        entry = %orig,
                        "clamped entry escaping destination root"
                    );
                    report.warnings.push(ExtractWarning::PathTraversalClamped {
                        original: orig,
                        clamped: dest_root.clone(),
                    });
                    report.entries_skipped += 1;
                    continue;
                }
            };

            if entry.is_directory {
                fs::create_dir_all(&out_path)?;
                report.entries_extracted += 1;
                continue;
            }

            if let Some(parent) = out_path.parent() {
                fs::create_dir_all(parent)?;
            }

            let out_path = if out_path.exists() {
                match opts.overwrite {
                    OverwritePolicy::Never => {
                        return Err(OtterzipError::Io(std::io::Error::new(
                            std::io::ErrorKind::AlreadyExists,
                            out_path.display().to_string(),
                        )));
                    }
                    OverwritePolicy::Always => out_path,
                    // Keep both: divert this file to `name (2).ext`.
                    OverwritePolicy::Rename => unique_extract_path(&out_path),
                    OverwritePolicy::IfNewer | OverwritePolicy::AskCallback => {
                        report.entries_skipped += 1;
                        continue;
                    }
                }
            } else {
                out_path
            };

            let file = fs::File::create(&out_path)?;
            let mut writer = BufWriter::new(file);
            let cap = opts.max_total_output_bytes;
            let written = if cap > 0 {
                // Enforce the absolute output-byte cap DURING the write. The
                // post-entry __BombMonitor check below only fires after the
                // whole entry is written — too late for a single-stream
                // backend (.gz/.xz/.bz2/.zst/.lz4) whose one entry IS the
                // entire payload and whose per-entry ratio gate is inert
                // (uncompressed_size == 0), so an unbounded copy would fill
                // the disk before any check ran (BK-M1).
                let remaining = cap.saturating_sub(report.bytes_written);
                let mut capped = CappedWriter { inner: &mut writer, remaining, tripped: false };
                match self.reader()?.extract_entry(&entry.path, &mut capped) {
                    Ok(n) => n,
                    Err(_) if capped.tripped => {
                        return Err(OtterzipError::ZipBombSuspected {
                            entry: entry.path.clone(),
                            ratio: 0,
                            limit: opts.max_total_compression_ratio,
                        });
                    }
                    Err(e) => return Err(e),
                }
            } else {
                self.reader()?.extract_entry(&entry.path, &mut writer)?
            };
            writer.flush()?;
            // PR-7A: propagate Zone.Identifier from source archive.
            // Best-effort — log + continue on ADS failure (likely
            // non-NTFS or owner mismatch). Never aborts extraction.
            if let Some(payload) = motw_payload.as_deref() {
                if let Err(e) = crate::motw::write_zone_identifier(&out_path, payload) {
                    tracing::warn!(
                        target: "otterzip::motw",
                        path = %out_path.display(),
                        error = %e,
                        "failed to propagate Zone.Identifier — file extracted without MOTW"
                    );
                }
            }
            // Cumulative gate. Per-entry `check_zip_bomb` already fired
            // for this entry; here we additionally watch the aggregate so
            // a "many small entries" bomb still trips somewhere in the
            // middle of extraction rather than after exhausting the disk.
            if let Err(err) = bomb_monitor.record(written, entry.compressed_size) {
                if let OtterzipError::ZipBombSuspected { ratio, limit, .. } = &err {
                    tracing::warn!(
                        target: "otterzip::security",
                        event = "zip_bomb_cumulative",
                        ratio = *ratio,
                        limit = *limit,
                        bytes_written = report.bytes_written + written,
                        "cumulative compression ratio or output cap exceeded"
                    );
                }
                return Err(err);
            }
            report.bytes_written += written;
            report.entries_extracted += 1;
        }

        report.elapsed = start.elapsed();
        tracing::info!(
            target: "otterzip::extract",
            elapsed_ms = report.elapsed.as_millis() as u64,
            entries = report.entries_extracted,
            skipped = report.entries_skipped,
            bytes = report.bytes_written,
            warnings = report.warnings.len(),
            "extract_all done (serial path)"
        );
        Ok(report)
    }
}

/// No-op sink used when the caller didn't provide one. Keeps the dyn-trait
/// dispatch in the streaming hook simple.
struct NoopSink;
impl ProgressSink for NoopSink {
    fn update(&mut self, _progress: &Progress) -> bool {
        true
    }
}

/// Look for split-archive siblings next to `first`. Recognises the
/// two common conventions:
///   - ZIP: `name.z01`, `name.z02`, ..., `name.zip` (last)
///   - 7z:  `name.7z.001`, `name.7z.002`, ...
/// Phase 8 G6 stops at *discovery* — the first-volume backend handles
/// extraction. Cross-volume read support depends on backend capability
/// upgrades (tracked in the gap report).
/// Temp path the writer targets while building a to-be-split archive.
/// `dir/name.7z` → `dir/name.7z.otzpart`. Chosen so it can never collide
/// with a `.NNN` segment or be picked up by [`discover_split_volumes`].
fn split_write_path(final_path: &Path) -> PathBuf {
    let mut s = final_path.as_os_str().to_os_string();
    s.push(".otzpart");
    PathBuf::from(s)
}

/// `dir/name.7z` + index 1 → `dir/name.7z.001` (7-Zip / Bandizip split
/// convention, 3-digit, 1-based).
fn volume_segment_path(base: &Path, index: u32) -> PathBuf {
    let mut s = base.as_os_str().to_os_string();
    s.push(format!(".{index:03}"));
    PathBuf::from(s)
}

/// Removes a set of segment files on drop unless explicitly disarmed.
/// Guards the multi-volume slice so a mid-stream I/O error leaves no
/// orphaned `.NNN` files behind.
struct SegmentGuard {
    paths: Vec<PathBuf>,
    armed: bool,
}

impl SegmentGuard {
    fn disarm(mut self) -> Vec<PathBuf> {
        self.armed = false;
        std::mem::take(&mut self.paths)
    }
}

impl Drop for SegmentGuard {
    fn drop(&mut self) {
        if self.armed {
            for p in &self.paths {
                let _ = fs::remove_file(p);
            }
        }
    }
}

/// Slice the contiguous archive at `src` into `base.001`, `base.002`, …
/// each at most `volume_size` bytes (the last holds the remainder).
/// Returns the produced segment paths. `volume_size` must be > 0 (the
/// caller filters zero). Caps at 999 segments — the `.NNN` convention.
fn split_into_volumes(src: &Path, base: &Path, volume_size: u64) -> Result<Vec<PathBuf>> {
    use std::io::{Read, Write};

    let mut input = std::io::BufReader::new(fs::File::open(src)?);
    let mut buf = vec![0u8; 256 * 1024];
    let mut guard = SegmentGuard {
        paths: Vec::new(),
        armed: true,
    };
    let mut out: Option<std::io::BufWriter<fs::File>> = None;
    let mut written_in_vol: u64 = 0;
    let mut index: u32 = 1;

    loop {
        let n = input.read(&mut buf)?;
        if n == 0 {
            break;
        }
        let mut data = &buf[..n];
        while !data.is_empty() {
            if out.is_none() || written_in_vol >= volume_size {
                if let Some(mut w) = out.take() {
                    w.flush()?;
                }
                if index > 999 {
                    return Err(OtterzipError::InvalidArgument(
                        "split would exceed 999 volumes; increase the volume size",
                    ));
                }
                let seg = volume_segment_path(base, index);
                out = Some(std::io::BufWriter::new(fs::File::create(&seg)?));
                guard.paths.push(seg);
                index = index.saturating_add(1);
                written_in_vol = 0;
            }
            let space = (volume_size - written_in_vol) as usize;
            let take = space.min(data.len());
            out.as_mut()
                .expect("segment writer set just above")
                .write_all(&data[..take])?;
            written_in_vol += take as u64;
            data = &data[take..];
        }
    }
    if let Some(mut w) = out.take() {
        w.flush()?;
    }
    // An empty source still yields a single empty `.001` so the volume
    // set is never absent (real archives always carry headers, so this
    // is purely defensive).
    if guard.paths.is_empty() {
        let seg = volume_segment_path(base, 1);
        std::io::BufWriter::new(fs::File::create(&seg)?).flush()?;
        guard.paths.push(seg);
    }
    Ok(guard.disarm())
}

fn discover_split_volumes(first: &Path) -> Vec<VolumeInfo> {
    let mut out = vec![first_volume_info(first, 1)];

    let parent = match first.parent() {
        Some(p) => p,
        None => return out,
    };
    let stem = match first.file_stem().and_then(|s| s.to_str()) {
        Some(s) => s,
        None => return out,
    };

    // 7z: scan stem.7z.NNN for NNN >= 002.
    let mut idx: u32 = 2;
    loop {
        let candidate = parent.join(format!("{stem}.7z.{idx:03}"));
        if !candidate.exists() {
            break;
        }
        out.push(volume_info_at(&candidate, idx));
        idx = idx.saturating_add(1);
        if idx > 999 {
            break;
        }
    }

    // ZIP-style: name.z01, name.z02, ... (the .zip itself counts as last).
    let mut idx: u32 = 1;
    loop {
        let candidate = parent.join(format!("{stem}.z{idx:02}"));
        if !candidate.exists() {
            break;
        }
        out.push(volume_info_at(&candidate, idx + 1));
        idx = idx.saturating_add(1);
        if idx > 99 {
            break;
        }
    }
    out
}

fn first_volume_info(p: &Path, index: u32) -> VolumeInfo {
    VolumeInfo {
        index,
        total: None,
        path: p.to_path_buf(),
        size_bytes: fs::metadata(p).map(|m| m.len()).unwrap_or(0),
    }
}

fn volume_info_at(p: &Path, index: u32) -> VolumeInfo {
    VolumeInfo {
        index,
        total: None,
        path: p.to_path_buf(),
        size_bytes: fs::metadata(p).map(|m| m.len()).unwrap_or(0),
    }
}

/// `std::io::Write` adapter that hashes incoming bytes with CRC-32
/// (IEEE polynomial — same as ZIP / GZip / PNG). Phase 8 G2 keeps a
/// minimal table-free implementation rather than pulling in `crc32fast`
/// for one call site; the test path is not a hot loop and 90 LoC of
/// branchless table init keep the dep graph small.
struct Crc32Sink {
    state: u32,
    table: [u32; 256],
}

impl Crc32Sink {
    fn new() -> Self {
        let mut table = [0u32; 256];
        for (i, slot) in table.iter_mut().enumerate() {
            let mut crc = i as u32;
            for _ in 0..8 {
                crc = if crc & 1 != 0 {
                    0xedb8_8320 ^ (crc >> 1)
                } else {
                    crc >> 1
                };
            }
            *slot = crc;
        }
        Self {
            state: 0xffff_ffff,
            table,
        }
    }
    fn finalize(&self) -> u32 {
        self.state ^ 0xffff_ffff
    }
}

impl std::io::Write for Crc32Sink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        for &b in buf {
            let idx = ((self.state ^ u32::from(b)) & 0xff) as usize;
            self.state = self.table[idx] ^ (self.state >> 8);
        }
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
/// `ArchiveInner::Writer(_)` slot can be consumed without leaving an
/// invalid value behind. None of its methods are reachable from the
/// public API after the transition.
struct EmptyBackend;
impl ArchiveBackend for EmptyBackend {
    fn entries(&self) -> Result<Box<dyn Iterator<Item = Result<crate::Entry>> + '_>> {
        Err(OtterzipError::InvalidArgument(
            "archive handle has been committed or rolled back",
        ))
    }
    fn extract_entry(&self, _entry_path: &str, _out: &mut dyn std::io::Write) -> Result<u64> {
        Err(OtterzipError::InvalidArgument(
            "archive handle has been committed or rolled back",
        ))
    }
    fn open_entry_stream(
        &self,
        _entry_path: &str,
    ) -> Result<Box<dyn std::io::Read + Send + '_>> {
        Err(OtterzipError::InvalidArgument(
            "archive handle has been committed or rolled back",
        ))
    }
}

/// Crate-internal alias used by streaming backends so they share a single
/// implementation of the bomb heuristic.
#[doc(hidden)]
pub(crate) fn __check_bomb_for_streaming(
    entry: &crate::Entry,
    opts: &ExtractOptions,
) -> Option<OtterzipError> {
    check_zip_bomb(entry, opts)
}

/// Streaming-path enforcement of the absolute output-byte cap — the
/// decompression-bomb backstop for formats where the per-entry ratio gate
/// can't help. `tar.*` reports `compressed_size == uncompressed_size` (the
/// gzip/bzip2/xz layer wraps the whole stream, so per-entry compressed sizes
/// don't exist), which makes the ratio a constant 1:1; solid 7z blocks report
/// `compressed_size == 0`. In both cases `__check_bomb_for_streaming` is inert,
/// so without this a crafted `evil.tar.gz` (e.g. 10 TB of zeros in a 10 MB
/// stream) would extract until the disk fills.
///
/// Copies `reader` → `writer` but writes at most `budget + 1` bytes, where
/// `budget` is the remaining cap (`max_total_output_bytes - already_written`).
/// The `+ 1` lets us detect an entry that *would* push the running total past
/// the cap without first writing the entire (bomb) payload — so a single
/// pathological entry is bounded, not just multi-entry aggregates. Returns
/// [`OtterzipError::ZipBombSuspected`] the moment the cap would be exceeded.
/// `max_total_output_bytes == 0` disables the cap (unbounded copy).
#[doc(hidden)]
pub(crate) fn __copy_capped<R: std::io::Read + ?Sized, W: std::io::Write + ?Sized>(
    reader: &mut R,
    writer: &mut W,
    entry_path: &str,
    already_written: u64,
    opts: &ExtractOptions,
) -> Result<u64> {
    use std::io::Read;
    let cap = opts.max_total_output_bytes;
    if cap == 0 {
        return Ok(std::io::copy(reader, writer)?);
    }
    let budget = cap.saturating_sub(already_written);
    let written = std::io::copy(&mut reader.take(budget.saturating_add(1)), writer)?;
    if written > budget {
        return Err(OtterzipError::ZipBombSuspected {
            entry: entry_path.to_string(),
            ratio: 0,
            limit: opts.max_total_compression_ratio,
        });
    }
    Ok(written)
}

/// Writer wrapper that enforces the absolute output-byte cap DURING a copy.
/// The serial extract loop wraps each entry's output in this so a single
/// unbounded entry — e.g. a single-stream `.gz`/`.xz` bomb whose per-entry
/// ratio gate is inert (`uncompressed_size == 0`) — can't fill the disk
/// before the post-entry [`__BombMonitor`] check runs. Writes at most
/// `remaining` bytes, then fails; `tripped` records that the cap (not an I/O
/// fault) stopped it, so the caller surfaces [`OtterzipError::ZipBombSuspected`]
/// rather than a generic I/O error (BK-M1).
struct CappedWriter<'a, W: std::io::Write + ?Sized> {
    inner: &'a mut W,
    remaining: u64,
    tripped: bool,
}

impl<W: std::io::Write + ?Sized> std::io::Write for CappedWriter<'_, W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if buf.len() as u64 > self.remaining {
            // Write what still fits under the cap, then stop hard.
            let fit = self.remaining as usize;
            if fit > 0 {
                self.inner.write_all(&buf[..fit])?;
                self.remaining = 0;
            }
            self.tripped = true;
            return Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                "otterzip: extract output-byte cap exceeded",
            ));
        }
        let n = self.inner.write(buf)?;
        self.remaining -= n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

/// Phase 7 — cumulative bomb gate. Streaming backends call this after
/// each entry write to track aggregate ratio + total output bytes. A
/// fresh `BombMonitor` is created per `extract_all` invocation.
#[doc(hidden)]
pub(crate) struct __BombMonitor {
    pub uncompressed_total: u64,
    pub compressed_total: u64,
    pub max_total_ratio: u32,
    pub max_total_bytes: u64,
}

impl __BombMonitor {
    pub fn new(opts: &ExtractOptions) -> Self {
        Self {
            uncompressed_total: 0,
            compressed_total: 0,
            max_total_ratio: opts.max_total_compression_ratio,
            max_total_bytes: opts.max_total_output_bytes,
        }
    }
    pub fn record(
        &mut self,
        uncompressed: u64,
        compressed: u64,
    ) -> std::result::Result<(), OtterzipError> {
        self.uncompressed_total = self.uncompressed_total.saturating_add(uncompressed);
        self.compressed_total = self.compressed_total.saturating_add(compressed);

        // Absolute cap — the real disk-exhaustion defence. Correct as-is.
        if self.max_total_bytes > 0 && self.uncompressed_total > self.max_total_bytes {
            return Err(OtterzipError::ZipBombSuspected {
                entry: "<aggregate>".to_string(),
                ratio: self.uncompressed_total / self.compressed_total.max(1),
                limit: self.max_total_ratio,
            });
        }
        // Cumulative ratio — same false-positive as the per-entry gate
        // (`check_zip_bomb`): benign compressible data reaches a codec's max
        // ratio, which exceeds the 1000 default. It is only meaningful once the
        // TOTAL output is large enough to matter; below the floor the absolute
        // cap above is the guard. Without this, a single 64 MiB zero-filled file
        // (ratio ~1028) aborted the whole extract as `<aggregate>`.
        if self.max_total_ratio > 0
            && self.compressed_total > 0
            && self.uncompressed_total >= RATIO_GATE_MIN_UNCOMPRESSED
        {
            let ratio = self.uncompressed_total / self.compressed_total.max(1);
            if ratio > u64::from(self.max_total_ratio) {
                return Err(OtterzipError::ZipBombSuspected {
                    entry: "<aggregate>".to_string(),
                    ratio,
                    limit: self.max_total_ratio,
                });
            }
        }
        Ok(())
    }
}

/// Returns `Some(err)` when the entry's compression ratio crosses the
/// caller-supplied threshold. Threshold `0` disables the check.
///
/// The per-entry ratio is a poor bomb signal on its own: benign data —
/// zero-filled VM disks, preallocated DB/log files, repetitive text — reaches
/// a codec's *maximum* ratio legitimately (DEFLATE ~1032:1, LZMA far higher),
/// which sits ABOVE the shipped default of 1000. Treating ratio alone as an
/// abort therefore refused ordinary files, and because the check runs before
/// the write it took the whole extract down with them.
///
/// The real defence against disk exhaustion is the absolute cumulative cap
/// (`max_total_output_bytes`, enforced by `CappedWriter` DURING every write),
/// which bounds total output regardless of ratio. So this gate is only an
/// *early-fail optimisation* — worth firing when an entry both claims a large
/// expansion AND has an implausible ratio, pointless (and harmful) on a small
/// output the cap would comfortably absorb. Below the absolute floor we defer
/// entirely to the cumulative cap.
const RATIO_GATE_MIN_UNCOMPRESSED: u64 = 1024 * 1024 * 1024; // 1 GiB

fn check_zip_bomb(
    entry: &crate::Entry,
    opts: &ExtractOptions,
) -> Option<OtterzipError> {
    if opts.max_compression_ratio == 0 {
        return None;
    }
    if entry.is_directory || entry.is_symlink {
        return None;
    }
    if entry.compressed_size == 0 || entry.uncompressed_size == 0 {
        return None;
    }
    // Small outputs can't exhaust the disk and the cumulative cap covers the
    // rest — don't let a legitimately-compressible file (zeros, logs) trip here.
    if entry.uncompressed_size < RATIO_GATE_MIN_UNCOMPRESSED {
        return None;
    }
    let ratio = entry.uncompressed_size / entry.compressed_size.max(1);
    if ratio > u64::from(opts.max_compression_ratio) {
        Some(OtterzipError::ZipBombSuspected {
            entry: entry.path.clone(),
            ratio,
            limit: opts.max_compression_ratio,
        })
    } else {
        None
    }
}

/// Outcome of resolving an entry path to an on-disk output path.
enum Skipped {
    /// The entry's path escapes `destination` via `..` or absolute segments.
    Traversal(String),
}

/// Canonicalize (or create+canonicalize) the destination root so we have a
/// stable prefix to anchor path-traversal checks against. We avoid making
/// the directory if it already exists.
fn canonical_root(dest: &Path) -> Result<PathBuf> {
    if !dest.exists() {
        fs::create_dir_all(dest)?;
    }
    // `canonicalize` on Windows returns `\\?\` extended-length paths, which
    // `Path::starts_with` still handles correctly — the prefix form is
    // consistent across the two paths we compare.
    Ok(fs::canonicalize(dest)?)
}

/// Phase 7 hardening — reject path components that look benign to a Path
/// parser but become weapons on Windows. Returns `Err(reason)` so the
/// caller can map to either `PathTraversalBlocked` (when block flag is on)
/// or a clamped warning.
///
/// Crate-internal so streaming backends (`tar_family`, `sevenz`, parallel
/// ZIP) can reuse the exact same rule set rather than re-implementing it.
#[doc(hidden)]
pub(crate) fn __validate_component(component: &str) -> std::result::Result<(), &'static str> {
    validate_component_phase7(component)
}

fn validate_component_phase7(component: &str) -> std::result::Result<(), &'static str> {
    if component.is_empty() {
        return Err("empty component");
    }
    // Null / control / DEL bytes never belong in a path.
    if component.bytes().any(|b| b < 0x20 || b == 0x7F) {
        return Err("control character in path");
    }
    // Embedded backslash inside what *should* be a single segment — common
    // attacker trick to smuggle traversal past forward-slash splitters.
    if component.contains('\\') {
        return Err("embedded backslash");
    }
    // NTFS Alternate Data Stream syntax: `file.txt:hidden`. Filesystems
    // accept it; we do not.
    if component.contains(':') {
        return Err("colon (NTFS ADS or drive letter)");
    }
    // Windows reserved device names — open as character devices regardless
    // of extension. `CON.txt` resolves to CON.
    let stem = component
        .split('.')
        .next()
        .unwrap_or(component)
        .to_ascii_uppercase();
    const RESERVED: &[&str] = &[
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7",
        "COM8", "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ];
    if RESERVED.iter().any(|r| *r == stem) {
        return Err("Windows reserved device name");
    }
    // Trailing dot/space silently stripped by Win32 — `evil.txt.` is
    // identical to `evil.txt` once persisted, but the strip happens after
    // path resolution so it can be used to bypass extension blocklists.
    if component.ends_with('.') || component.ends_with(' ') {
        return Err("trailing dot or space");
    }
    Ok(())
}

/// Compute a non-colliding sibling path for the [`OverwritePolicy::Rename`]
/// (keep-both) policy. `foo.txt` → `foo (2).txt`, then `(3)`, … scanning
/// for the first free name. `foo` (no extension) → `foo (2)`. The caller
/// only invokes this when `path` already exists, so the scan starts at 2.
/// Falls back to the original path after an absurd number of collisions
/// (pathological — degrades to overwrite rather than looping forever).
pub(crate) fn unique_extract_path(path: &Path) -> PathBuf {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("");
    let ext = path.extension().and_then(|s| s.to_str());
    for i in 2..10_000u32 {
        let name = match ext {
            Some(e) => format!("{stem} ({i}).{e}"),
            None => format!("{stem} ({i})"),
        };
        let candidate = parent.join(name);
        if !candidate.exists() {
            return candidate;
        }
    }
    path.to_path_buf()
}

/// Atomically reserve a non-colliding sibling name for the keep-both
/// ([`OverwritePolicy::Rename`]) policy in the PARALLEL extract backends.
/// Same naming as [`unique_extract_path`] (`foo (2).ext`, `(3)`, …) but each
/// candidate is opened with `create_new`, so two rayon workers racing on the
/// same name can't both win — the loser gets `AlreadyExists` and advances.
/// This closes the check-then-`File::create` TOCTOU that the pure path-compute
/// version leaves open under concurrency (the reserved empty file is then
/// truncate-opened by the caller's normal `File::create`). Serial backends are
/// single-threaded and keep using [`unique_extract_path`].
pub(crate) fn reserve_unique_extract_path(path: &Path) -> std::io::Result<PathBuf> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
    let ext = path.extension().and_then(|s| s.to_str());
    for i in 2..10_000u32 {
        let name = match ext {
            Some(e) => format!("{stem} ({i}).{e}"),
            None => format!("{stem} ({i})"),
        };
        let candidate = parent.join(name);
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(_) => return Ok(candidate),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    // Pathological collision count — degrade to overwriting the original
    // (matches unique_extract_path's fallback) rather than erroring out.
    Ok(path.to_path_buf())
}

/// Combine an entry's in-archive path with the destination root.
///
/// Rejects absolute paths and `..` components (per the default
/// `block_path_traversal=true`). When `flatten_paths` is set, only the
/// file name is retained.
fn resolve_output_path(
    dest_root: &Path,
    entry_path: &str,
    opts: &ExtractOptions,
    is_dir: bool,
) -> std::result::Result<PathBuf, Skipped> {
    // Normalize: ZIP uses forward-slashes always; replace for OS.
    let as_path = Path::new(entry_path);

    if opts.flatten_paths {
        // Validate the flattened component like the non-flatten path below,
        // and reject `file_name() == None` (entry is `.`/`..`/root) instead
        // of falling back to the raw entry_path — a bare `..` would otherwise
        // resolve to dest_root.join("..") and escape the destination (FFI-M2).
        let name = match as_path.file_name() {
            Some(n) => n,
            None => return Err(Skipped::Traversal(entry_path.to_string())),
        };
        if validate_component_phase7(&name.to_string_lossy()).is_err() {
            return Err(Skipped::Traversal(entry_path.to_string()));
        }
        return Ok(dest_root.join(name));
    }

    let mut out = dest_root.to_path_buf();
    for comp in as_path.components() {
        match comp {
            Component::Normal(c) => {
                let s = c.to_string_lossy();
                if validate_component_phase7(&s).is_err() {
                    return Err(Skipped::Traversal(entry_path.to_string()));
                }
                out.push(c);
            }
            Component::CurDir => {}
            Component::RootDir | Component::Prefix(_) | Component::ParentDir => {
                return Err(Skipped::Traversal(entry_path.to_string()));
            }
        }
    }

    // `""`, `.`, `./` and any all-CurDir path produce zero Normal components,
    // so the loop above inspects nothing and `out == dest_root`. What that
    // means depends on the entry KIND:
    //
    //  * A DIRECTORY is the destination itself. `./` is the first member of
    //    every `tar -cf x.tar -C dir .` — the most common tar idiom there is —
    //    and creating a directory that already exists is a no-op. Accept it, or
    //    those archives extract to nothing at all.
    //  * A FILE cannot be the destination directory. Left to run, the caller
    //    opens the destination as a file, and under the keep-both policy
    //    `unique_extract_path(dest_root)` walks up to the PARENT and writes
    //    `<dest> (2)` OUTSIDE the destination root — with every component
    //    guard skipped, because there were no components to inspect.
    if out == dest_root {
        if is_dir {
            return Ok(out);
        }
        return Err(Skipped::Traversal(entry_path.to_string()));
    }

    // Defense in depth: make sure the final path really starts with the
    // destination root even after OS-level normalization.
    if !out.starts_with(dest_root) {
        return Err(Skipped::Traversal(entry_path.to_string()));
    }
    Ok(out)
}

/// [`resolve_output_path`] for the streaming backends, which want the rejected
/// entry path back as a plain `String` (to choose between
/// [`OtterzipError::PathTraversalBlocked`] and a clamped warning) rather than
/// the serial loop's `Skipped`.
///
/// Crate-internal and shared on purpose: `sevenz` and `tar_family` each carried
/// a byte-identical private copy, and `rar` was about to add a third — three
/// chances for a security check to drift out of sync with the serial path's.
/// There is exactly one rule set, here.
#[doc(hidden)]
pub(crate) fn __resolve_output_path_streaming(
    dest_root: &Path,
    entry_path: &str,
    opts: &ExtractOptions,
    is_dir: bool,
) -> std::result::Result<PathBuf, String> {
    resolve_output_path(dest_root, entry_path, opts, is_dir).map_err(|Skipped::Traversal(p)| p)
}
