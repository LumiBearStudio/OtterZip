//! TAR family backend — plain `.tar`, `.tar.gz`, `.tar.bz2`, `.tar.xz`.
//!
//! TAR is a streaming format with **no central directory** — random access
//! requires re-reading from byte zero of the (possibly compressed) archive.
//! That mismatch with `ArchiveBackend::extract_entry`'s name-based lookup
//! would degrade `extract_all` to O(n²) decompression. We work around this
//! by overriding [`ArchiveBackend::extract_all_streaming`] so every entry
//! is written in one forward pass.
//!
//! Backend-specific notes (per `performance.md` §3):
//! * Decompression uses C-backed crates (`flate2 + zlib-ng`, `bzip2`, `xz2`)
//!   to keep parity with reference CLI tools.
//! * `entries()` re-opens the file but only walks header blocks via
//!   `Entries::raw(false)`, so the cost is metadata-only.

use std::cell::RefCell;
use std::fs::File;
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

use crate::archive::ExtractWarning;
use crate::backends::{ArchiveBackend, StreamingExtractCtx};
use crate::encoding::{self, LegacyCodepageOverride, NameEncoding, NameInput};
use crate::entry::{Entry, HostOs};
use crate::error::{Result, OtterzipError};
use crate::format::{CompressionMethod, EncryptionMethod};
use crate::options::OverwritePolicy;
use crate::progress::{Progress, ProgressPhase};

#[derive(Copy, Clone, Debug)]
pub(crate) enum Compression {
    None,
    Gzip,
    Bzip2,
    Xz,
    /// `.tlz` — legacy GNU naming for tar + LZMA1 (alone-format, NOT
    /// LZMA2 / .xz). PR-F4. Routed via the same `ArchiveFormat::TarXz`
    /// enum slot as Xz, distinguished by path extension at dispatch
    /// time so we don't have to grow the public enum for an alias.
    Lzma,
    /// `.tar.zst` — Zstandard frame around a tarball. PR-F2.
    Zstd,
    /// `.tar.lz4` — LZ4 frame around a tarball. PR-F2.
    Lz4,
}

pub(crate) struct TarBackend {
    path: PathBuf,
    compression: Compression,
    /// Cached metadata for `entries()`. We materialise once per backend so
    /// repeated metadata calls (e.g. an outer `entry_count`) don't re-stream.
    /// Wrapped in `RefCell` to honour the `&self` API contract.
    cache: RefCell<Option<Vec<Entry>>>,
    /// The one filename encoding chosen for this archive. tar carries NO
    /// encoding indicator (unlike ZIP's GP bit 11 / 0x7075), so names are raw
    /// locale bytes; `read_metadata_uncached` runs the detector over ALL names
    /// once and caches the verdict here so `entries()`, the streaming extract
    /// and random-access `extract_entry` all decode identically.
    name_encoding: RefCell<Option<NameEncoding>>,
}

impl TarBackend {
    pub(crate) fn open(path: &Path, compression: Compression) -> Result<Self> {
        // Validate openability up front so `Archive::open` reports I/O
        // errors at the same point ZIP does.
        let _ = File::open(path)?;
        Ok(Self {
            path: path.to_path_buf(),
            compression,
            cache: RefCell::new(None),
            name_encoding: RefCell::new(None),
        })
    }

    /// Decode one raw tar name with the archive's chosen encoding. tar sets no
    /// UTF-8 flag and has no Unicode-path extra, so the cascade relies on
    /// tier-2 (valid UTF-8) then tier-3 (the detected legacy codepage).
    fn decode_name(bytes: &[u8], enc: NameEncoding) -> String {
        encoding::decode_one(
            &NameInput {
                raw_bytes: bytes,
                utf8_flag: false,
                unicode_path_extra: None,
            },
            enc,
        )
    }

    /// Return the archive's filename encoding, running (and caching) the
    /// detector over every name if it hasn't been decided yet. Random-access
    /// `extract_entry` uses this so it matches the names `entries()` reported
    /// even when the caller never listed first.
    fn ensure_encoding(&self) -> Result<NameEncoding> {
        if let Some(enc) = *self.name_encoding.borrow() {
            return Ok(enc);
        }
        self.read_metadata_uncached()?; // populates name_encoding as a side effect
        Ok(self.name_encoding.borrow().unwrap_or(NameEncoding::Utf8))
    }

    fn open_decompressed(&self) -> Result<Box<dyn Read + Send>> {
        let file = File::open(&self.path)?;
        let reader = BufReader::new(file);
        match self.compression {
            Compression::None => Ok(Box::new(reader)),
            Compression::Gzip => Ok(Box::new(flate2::read::GzDecoder::new(reader))),
            Compression::Bzip2 => Ok(Box::new(bzip2::read::BzDecoder::new(reader))),
            Compression::Xz => Ok(Box::new(xz2::read::XzDecoder::new(reader))),
            // PR-F4 — .tlz = tar + raw LZMA1. xz2 ships an explicit
            // LZMA1 (alone-format) decoder via `Stream::new_lzma_decoder`,
            // wrapped through `XzDecoder::new_stream` to keep the
            // io::Read API. u64::MAX memlimit matches the single-stream
            // backend; ZIP-bomb defence sits one layer up.
            Compression::Lzma => {
                let stream = xz2::stream::Stream::new_lzma_decoder(u64::MAX).map_err(|e| {
                    OtterzipError::BackendError(format!("tlz decoder init: {e}"))
                })?;
                Ok(Box::new(xz2::read::XzDecoder::new_stream(reader, stream)))
            }
            // PR-F2 — Zstd / LZ4 wrappers. Same shape as the others;
            // failures bubble up as BackendError to keep the open path
            // uniform.
            Compression::Zstd => {
                let dec = zstd::stream::read::Decoder::new(reader).map_err(|e| {
                    OtterzipError::BackendError(format!("tar.zst decoder init: {e}"))
                })?;
                Ok(Box::new(dec))
            }
            Compression::Lz4 => Ok(Box::new(lz4_flex::frame::FrameDecoder::new(reader))),
        }
    }

    fn read_metadata_uncached(&self) -> Result<Vec<Entry>> {
        let stream = self.open_decompressed()?;
        let mut archive = tar::Archive::new(stream);
        // Two-phase: `tar::Entry` is consumed as we iterate, so collect the raw
        // name bytes + the rest of each pod first, THEN pick one encoding for
        // the whole archive and decode. `entry.path()` (which we previously
        // used) hard-errors on Windows for any non-UTF-8 name and the backend
        // turned that into a silent skip — a CP949/Shift_JIS tarball's members
        // vanished with the report still reading success.
        let mut raw_names: Vec<Vec<u8>> = Vec::new();
        let mut pods: Vec<Entry> = Vec::new();
        for entry in archive.entries().map_err(map_tar_err)? {
            let entry = entry.map_err(map_tar_err)?;
            raw_names.push(entry.path_bytes().into_owned());
            pods.push(tar_entry_to_pod(&entry)?); // path filled in below
        }

        let inputs: Vec<NameInput<'_>> = raw_names
            .iter()
            .map(|b| NameInput {
                raw_bytes: b,
                utf8_flag: false,
                unicode_path_extra: None,
            })
            .collect();
        let enc = encoding::detect_archive_encoding(&inputs, LegacyCodepageOverride::Auto);
        *self.name_encoding.borrow_mut() = Some(enc);

        for (pod, bytes) in pods.iter_mut().zip(&raw_names) {
            pod.path = Self::decode_name(bytes, enc);
        }
        Ok(pods)
    }
}

impl ArchiveBackend for TarBackend {
    fn entries(&self) -> Result<Box<dyn Iterator<Item = Result<Entry>> + '_>> {
        if self.cache.borrow().is_none() {
            let v = self.read_metadata_uncached()?;
            *self.cache.borrow_mut() = Some(v);
        }
        let snapshot = self.cache.borrow().as_ref().unwrap().clone();
        Ok(Box::new(snapshot.into_iter().map(Ok)))
    }

    fn extract_entry(&self, entry_path: &str, out: &mut dyn std::io::Write) -> Result<u64> {
        // Decode names with the same archive-wide encoding `entries()` used, so
        // a CP949/Shift_JIS name the caller got from listing still matches here.
        let enc = self.ensure_encoding()?;
        // Linear scan from the head; tar has no index.
        let stream = self.open_decompressed()?;
        let mut archive = tar::Archive::new(stream);
        for entry in archive.entries().map_err(map_tar_err)? {
            let mut entry = entry.map_err(map_tar_err)?;
            let path = Self::decode_name(&entry.path_bytes(), enc);
            if path == entry_path {
                let written = io::copy(&mut entry, out)?;
                return Ok(written);
            }
        }
        Err(OtterzipError::EntryNotFound(entry_path.to_string()))
    }

    fn open_entry_stream(&self, entry_path: &str) -> Result<Box<dyn Read + Send + '_>> {
        // Single-shot copy — see ZIP backend rationale.
        let mut buf = Vec::new();
        self.extract_entry(entry_path, &mut buf)?;
        Ok(Box::new(io::Cursor::new(buf)))
    }

    fn extract_all_streaming(
        &self,
        ctx: &mut StreamingExtractCtx<'_>,
    ) -> Option<Result<()>> {
        Some(self.extract_all_inner(ctx))
    }
}

impl TarBackend {
    fn extract_all_inner(&self, ctx: &mut StreamingExtractCtx<'_>) -> Result<()> {
        // We can't know totals up front without a metadata pass, so we
        // do one. It's metadata-only (no decompressed entry payloads
        // emerge) — for tar.gz on Silesia this is ~1% of full extract
        // time, which is an acceptable price for accurate progress.
        let entries_meta = self.read_metadata_uncached()?;
        let total_bytes: u64 = entries_meta.iter().map(|e| e.uncompressed_size).sum();
        let total_entries = u32::try_from(entries_meta.len()).unwrap_or(u32::MAX);

        let stream = self.open_decompressed()?;
        let mut archive = tar::Archive::new(stream);
        let mut iter = archive.entries().map_err(map_tar_err)?;

        // The metadata pass above cached the archive's name encoding; decode
        // each streamed entry the same way so the on-disk names match what
        // `entries()` reported (and non-UTF-8 names are no longer skipped).
        let enc = self.name_encoding.borrow().unwrap_or(NameEncoding::Utf8);
        let mut idx: u32 = 0;
        while let Some(entry_res) = iter.next() {
            let mut entry = entry_res.map_err(map_tar_err)?;
            let path_str = Self::decode_name(&entry.path_bytes(), enc);

            let mut pod = tar_entry_to_pod(&entry)?;
            pod.path = path_str.clone();

            // Progress tick before doing work for this entry.
            let snapshot = Progress {
                bytes_processed: ctx.report.bytes_written,
                bytes_total: total_bytes,
                entries_processed: idx,
                entries_total: total_entries,
                current_entry: Some(path_str.clone()),
                phase: ProgressPhase::Writing,
                elapsed: ctx.start.elapsed(),
                current_entry_bytes_processed: 0,
                current_entry_bytes_total: 0,
            };
            if !ctx.progress.update(&snapshot) {
                return Err(OtterzipError::Canceled);
            }
            idx = idx.saturating_add(1);

            // ZIP-bomb gate (uses the same structural rule as the random-
            // access path so behaviour is consistent across formats).
            if let Some(err) = crate::archive::__check_bomb_for_streaming(&pod, ctx.opts) {
                return Err(err);
            }

            // Symlink filter.
            if pod.is_symlink && !ctx.opts.follow_symlinks {
                ctx.report.warnings.push(ExtractWarning::SymlinkSkipped {
                    path: path_str.clone(),
                    target: String::new(),
                });
                ctx.report.entries_skipped += 1;
                continue;
            }

            let out_path =
                match crate::archive::__resolve_output_path_streaming(
                    ctx.dest_root,
                    &path_str,
                    ctx.opts,
                    pod.is_directory,
                ) {
                    Ok(p) => p,
                    Err(orig) => {
                        if ctx.opts.block_path_traversal {
                            return Err(OtterzipError::PathTraversalBlocked(orig));
                        }
                        ctx.report
                            .warnings
                            .push(ExtractWarning::PathTraversalClamped {
                                original: orig,
                                clamped: ctx.dest_root.to_path_buf(),
                            });
                        ctx.report.entries_skipped += 1;
                        continue;
                    }
                };

            if pod.is_directory {
                std::fs::create_dir_all(&out_path)?;
                ctx.report.entries_extracted += 1;
                continue;
            }

            if let Some(parent) = out_path.parent() {
                std::fs::create_dir_all(parent)?;
            }

            let out_path = if out_path.exists() {
                match ctx.opts.overwrite {
                    OverwritePolicy::Never => {
                        return Err(OtterzipError::Io(io::Error::new(
                            io::ErrorKind::AlreadyExists,
                            out_path.display().to_string(),
                        )));
                    }
                    OverwritePolicy::Always => out_path,
                    // Keep both: divert this file to `name (2).ext`.
                    OverwritePolicy::Rename => crate::archive::unique_extract_path(&out_path),
                    OverwritePolicy::IfNewer | OverwritePolicy::AskCallback => {
                        ctx.report.entries_skipped += 1;
                        continue;
                    }
                }
            } else {
                out_path
            };

            let file = File::create(&out_path)?;
            let mut writer = BufWriter::new(file);
            // Bounded copy: enforces the absolute output-byte cap. tar's
            // per-entry ratio is a constant 1:1 (no per-entry compressed
            // size exists behind the gzip/bzip2/xz stream), so the byte cap
            // is the only bomb defense on this path.
            let written = crate::archive::__copy_capped(
                &mut entry,
                &mut writer,
                &path_str,
                ctx.report.bytes_written,
                ctx.opts,
            )?;
            writer.flush()?;
            // PR-7A: MOTW propagation. Best-effort.
            if let Some(payload) = ctx.motw_payload {
                if let Err(e) = crate::motw::write_zone_identifier(&out_path, payload) {
                    tracing::warn!(
                        target: "otterzip::motw",
                        path = %out_path.display(),
                        error = %e,
                        "MOTW propagation skipped (tar.* extract)"
                    );
                }
            }
            ctx.report.bytes_written += written;
            ctx.report.entries_extracted += 1;
        }

        Ok(())
    }
}

/// Build the pod for one tar entry EXCEPT its `path`, which the caller fills
/// in after the archive-wide encoding has been decided (tar names are raw
/// locale bytes; see `read_metadata_uncached`). Leaving it empty here keeps the
/// name decode in exactly one place.
fn tar_entry_to_pod<R: Read>(entry: &tar::Entry<'_, R>) -> Result<Entry> {
    let header = entry.header();
    let path = String::new();
    let entry_type = header.entry_type();
    let is_directory = entry_type.is_dir();
    let is_symlink = entry_type.is_symlink();
    let uncompressed_size = header.size().map_err(map_tar_err)?;
    let attributes = u32::try_from(header.mode().map_err(map_tar_err)?).unwrap_or(0);
    let modified = header
        .mtime()
        .ok()
        .and_then(|t| UNIX_EPOCH.checked_add(Duration::from_secs(t)));

    Ok(Entry {
        path,
        is_directory,
        is_symlink,
        uncompressed_size,
        // tar stores files uncompressed in the data block; the outer
        // compressor (gz/bz2/xz) compresses the *whole* archive, not
        // individual entries. We surface "compressed_size = uncompressed".
        compressed_size: uncompressed_size,
        compression: CompressionMethod::Store,
        encryption: EncryptionMethod::None,
        crc32: None,
        modified,
        accessed: None,
        created: None,
        attributes,
        comment: None,
        host_os: HostOs::Unix,
    })
}

fn map_tar_err(e: io::Error) -> OtterzipError {
    OtterzipError::Io(e)
}


