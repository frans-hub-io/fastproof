//! Upload readers: single file, directory, zip, tar and tar.gz. Entries are streamed one
//! by one; nothing is extracted to disk.

use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use flate2::read::MultiGzDecoder;
use serde::{Deserialize, Serialize};

use crate::error::{IngestError, LimitIoError};
use crate::limits::{Budget, BudgetReader, LimitedReader, Limits, read_bounded};

/// Kind of the upload container.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum UploadKind {
    File,
    Directory,
    Zip,
    Tar,
    TarGz,
}

pub(crate) struct Upload {
    pub kind: UploadKind,
    path: PathBuf,
    /// Entry name of a single-file upload.
    file_name: String,
}

/// Visitor over entries: upload-relative path, declared size and a bounded reader.
pub(crate) type EntryVisitor<'v> =
    dyn FnMut(&str, u64, &mut dyn Read) -> Result<(), IngestError> + 'v;

/// Validates and normalizes an archive entry path. Rejects absolute paths, drive letters,
/// `..` components, NUL bytes and over-long names.
pub(crate) fn safe_entry_path(raw: &str, limits: &Limits) -> Result<Option<String>, IngestError> {
    if raw.len() as u64 > limits.max_path_bytes {
        return Err(IngestError::limit_at(
            "max_path_bytes",
            limits.max_path_bytes,
            raw.chars().take(80).collect::<String>(),
        ));
    }
    let unsafe_entry = |reason| IngestError::UnsafeEntry {
        entry: raw.chars().take(200).collect(),
        reason,
    };
    if raw.contains('\0') {
        return Err(unsafe_entry("NUL byte in path"));
    }
    let p = raw.replace('\\', "/");
    if p.starts_with('/') {
        return Err(unsafe_entry("absolute path"));
    }
    let b = p.as_bytes();
    if b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':' {
        return Err(unsafe_entry("drive-letter path"));
    }
    let mut parts = Vec::new();
    for part in p.split('/') {
        match part {
            "" | "." => {}
            ".." => return Err(unsafe_entry("parent directory component")),
            other => parts.push(other),
        }
    }
    if parts.is_empty() {
        return Ok(None);
    }
    Ok(Some(parts.join("/")))
}

fn magic(path: &Path) -> Result<Vec<u8>, IngestError> {
    let mut f = File::open(path)?;
    let mut buf = vec![0u8; 512];
    let mut n = 0;
    while n < buf.len() {
        let r = f.read(&mut buf[n..])?;
        if r == 0 {
            break;
        }
        n += r;
    }
    buf.truncate(n);
    Ok(buf)
}

fn is_tar_header(b: &[u8]) -> bool {
    b.len() >= 262 && &b[257..262] == b"ustar"
}

impl Upload {
    pub(crate) fn open(
        path: &Path,
        file_name: Option<&str>,
        limits: &Limits,
    ) -> Result<Upload, IngestError> {
        let meta = std::fs::metadata(path)?;
        let name = file_name
            .map(str::to_string)
            .or_else(|| path.file_name().map(|n| n.to_string_lossy().into_owned()))
            .unwrap_or_else(|| "upload".into());
        if meta.is_dir() {
            return Ok(Upload {
                kind: UploadKind::Directory,
                path: path.to_path_buf(),
                file_name: name,
            });
        }
        if meta.len() > limits.max_upload_bytes {
            return Err(IngestError::limit(
                "max_upload_bytes",
                limits.max_upload_bytes,
            ));
        }
        let head = magic(path)?;
        let kind = if head.starts_with(b"PK\x03\x04") || head.starts_with(b"PK\x05\x06") {
            UploadKind::Zip
        } else if head.starts_with(&[0x1f, 0x8b]) {
            let mut dec = MultiGzDecoder::new(BufReader::new(File::open(path)?));
            let mut inner = vec![0u8; 512];
            let mut n = 0;
            while n < inner.len() {
                match dec.read(&mut inner[n..]) {
                    Ok(0) => break,
                    Ok(r) => n += r,
                    Err(e) => return Err(IngestError::Archive(format!("gzip: {e}"))),
                }
            }
            inner.truncate(n);
            if is_tar_header(&inner) {
                UploadKind::TarGz
            } else {
                return Err(IngestError::Unsupported(
                    "gzip content is not a tar archive".into(),
                ));
            }
        } else if is_tar_header(&head) {
            UploadKind::Tar
        } else {
            UploadKind::File
        };
        Ok(Upload {
            kind,
            path: path.to_path_buf(),
            file_name: name,
        })
    }

    /// Calls `visit` for every regular file (all of them, or only those `wanted` accepts).
    pub(crate) fn for_each(
        &self,
        limits: &Limits,
        budget: &Budget,
        wanted: &dyn Fn(&str) -> bool,
        skipped: &mut dyn FnMut(&str, &'static str),
        visit: &mut EntryVisitor<'_>,
    ) -> Result<(), IngestError> {
        match self.kind {
            UploadKind::File => {
                let name =
                    safe_entry_path(&self.file_name, limits)?.unwrap_or_else(|| "upload".into());
                if !wanted(&name) {
                    return Ok(());
                }
                let f = File::open(&self.path)?;
                let size = f.metadata()?.len();
                let mut r = LimitedReader::new(
                    BufReader::new(f),
                    budget.clone(),
                    limits.max_entry_bytes,
                    &name,
                );
                visit(&name, size, &mut r)
            }
            UploadKind::Directory => {
                let mut count = 0u64;
                walk_dir(
                    &self.path, "", 0, limits, budget, wanted, skipped, visit, &mut count,
                )
            }
            UploadKind::Zip => self.for_each_zip(limits, budget, wanted, skipped, visit),
            UploadKind::Tar | UploadKind::TarGz => {
                self.for_each_tar(limits, budget, wanted, skipped, visit)
            }
        }
    }

    fn for_each_zip(
        &self,
        limits: &Limits,
        budget: &Budget,
        wanted: &dyn Fn(&str) -> bool,
        skipped: &mut dyn FnMut(&str, &'static str),
        visit: &mut EntryVisitor<'_>,
    ) -> Result<(), IngestError> {
        let mut f = File::open(&self.path)?;
        check_zip_directory(&mut f, limits)?;
        // The zip crate reads the whole central directory while opening the archive; bound
        // what it may read (and so build in memory) until it is open.
        let open_cap = Arc::new(AtomicU64::new(
            limits
                .max_zip_directory_bytes
                .saturating_add(ZIP_TAIL_BYTES * 2),
        ));
        let reader = CappedReader {
            inner: f,
            remaining: open_cap.clone(),
            max: limits.max_zip_directory_bytes,
        };
        let mut zip = zip::ZipArchive::new(BufReader::new(reader)).map_err(|e| match e {
            zip::result::ZipError::Io(io) => match IngestError::from_io(io) {
                IngestError::Io(io) => IngestError::Archive(format!("zip: {io}")),
                limit => limit,
            },
            other => IngestError::Archive(format!("zip: {other}")),
        })?;
        open_cap.store(u64::MAX, Ordering::Relaxed);
        if zip.len() as u64 > limits.max_archive_entries {
            return Err(IngestError::limit(
                "max_archive_entries",
                limits.max_archive_entries,
            ));
        }
        for i in 0..zip.len() {
            let (name, is_dir, is_symlink, size) = {
                let entry = zip
                    .by_index_raw(i)
                    .map_err(|e| IngestError::Archive(format!("zip entry {i}: {e}")))?;
                let raw = entry
                    .name()
                    .map_err(|e| IngestError::Archive(format!("zip entry {i}: {e}")))?
                    .into_owned();
                (raw, entry.is_dir(), entry.is_symlink(), entry.size())
            };
            if is_symlink {
                return Err(IngestError::UnsafeEntry {
                    entry: name,
                    reason: "symbolic link",
                });
            }
            let Some(path) = safe_entry_path(&name, limits)? else {
                continue;
            };
            if is_dir {
                continue;
            }
            if size > limits.max_entry_bytes {
                return Err(IngestError::limit_at(
                    "max_entry_bytes",
                    limits.max_entry_bytes,
                    path,
                ));
            }
            if !wanted(&path) {
                continue;
            }
            let entry = match zip.by_index(i) {
                Ok(e) => e,
                Err(e) => {
                    let _ = e;
                    skipped(
                        &path,
                        "unsupported zip entry (encrypted or unknown compression)",
                    );
                    continue;
                }
            };
            let mut r = LimitedReader::new(entry, budget.clone(), limits.max_entry_bytes, &path);
            visit(&path, size, &mut r)?;
        }
        Ok(())
    }

    fn for_each_tar(
        &self,
        limits: &Limits,
        budget: &Budget,
        wanted: &dyn Fn(&str) -> bool,
        skipped: &mut dyn FnMut(&str, &'static str),
        visit: &mut EntryVisitor<'_>,
    ) -> Result<(), IngestError> {
        let file = BufReader::new(File::open(&self.path)?);
        let stream: Box<dyn Read> = if self.kind == UploadKind::TarGz {
            Box::new(MultiGzDecoder::new(file))
        } else {
            Box::new(file)
        };
        let counted = BudgetReader {
            inner: stream,
            budget: budget.clone(),
            context: "tar stream",
        };
        let mut archive = tar::Archive::new(counted);
        let entries = archive
            .entries()
            .map_err(|e| IngestError::Archive(format!("tar: {e}")))?;
        // Raw mode: long names and PAX headers are handled here with explicit bounds
        // instead of being buffered without limit.
        let entries = entries.raw(true);
        let mut count = 0u64;
        let mut pending_name: Option<String> = None;
        let mut pending_size: Option<u64> = None;
        for entry in entries {
            let mut entry = entry.map_err(IngestError::from_io).map_err(|e| match e {
                IngestError::Io(io) => IngestError::Archive(format!("tar: {io}")),
                other => other,
            })?;
            count += 1;
            if count > limits.max_archive_entries {
                return Err(IngestError::limit(
                    "max_archive_entries",
                    limits.max_archive_entries,
                ));
            }
            let header = entry.header();
            let et = header.entry_type();
            let size = entry.size();
            let raw_name = String::from_utf8_lossy(&entry.path_bytes()).into_owned();
            match et.as_byte() {
                b'L' => {
                    let data = read_bounded(
                        &mut entry,
                        limits.max_path_bytes,
                        "max_path_bytes",
                        "tar long name",
                    )?;
                    pending_name = Some(
                        String::from_utf8_lossy(&data)
                            .trim_end_matches('\0')
                            .to_string(),
                    );
                    continue;
                }
                b'x' => {
                    let data =
                        read_bounded(&mut entry, 64 * 1024, "max_path_bytes", "tar pax header")?;
                    for (k, v) in parse_pax(&data) {
                        match k.as_str() {
                            "path" => pending_name = Some(v),
                            "size" => pending_size = v.trim().parse().ok(),
                            _ => {}
                        }
                    }
                    continue;
                }
                b'g' => {
                    read_bounded(
                        &mut entry,
                        64 * 1024,
                        "max_path_bytes",
                        "tar global pax header",
                    )?;
                    continue;
                }
                _ => {}
            }
            let name = pending_name.take().unwrap_or(raw_name);
            let declared = pending_size.take();
            if et.is_symlink() || et.is_hard_link() || et.as_byte() == b'K' {
                return Err(IngestError::UnsafeEntry {
                    entry: name,
                    reason: "link entry",
                });
            }
            if et.is_dir() {
                safe_entry_path(&name, limits)?;
                continue;
            }
            if !(et.is_file() || et.is_contiguous()) {
                return Err(IngestError::UnsafeEntry {
                    entry: name,
                    reason: "unsupported tar entry type",
                });
            }
            if declared.is_some_and(|d| d != size) {
                return Err(IngestError::Archive(format!(
                    "tar entry {name:?}: PAX size does not match header"
                )));
            }
            let Some(path) = safe_entry_path(&name, limits)? else {
                continue;
            };
            if size > limits.max_entry_bytes {
                return Err(IngestError::limit_at(
                    "max_entry_bytes",
                    limits.max_entry_bytes,
                    path,
                ));
            }
            if !wanted(&path) {
                continue;
            }
            // The outer stream is already charged to the budget; only the entry bound here.
            let mut r = LimitedReader::new(
                &mut entry,
                Budget::new(u64::MAX),
                limits.max_entry_bytes,
                &path,
            );
            visit(&path, size, &mut r)?;
            let _ = &skipped;
        }
        Ok(())
    }
}

/// Bytes at the end of a zip file that can hold the end-of-central-directory record: the
/// record itself (22 bytes) and a comment of up to 64 KiB.
const ZIP_TAIL_BYTES: u64 = 22 + 0xFFFF;

const EOCD_SIG: &[u8; 4] = b"PK\x05\x06";
const EOCD64_LOCATOR_SIG: &[u8; 4] = b"PK\x06\x07";
const EOCD64_SIG: &[u8; 4] = b"PK\x06\x06";

fn le16(b: &[u8], at: usize) -> u64 {
    u16::from_le_bytes([b[at], b[at + 1]]) as u64
}

fn le32(b: &[u8], at: usize) -> u64 {
    u32::from_le_bytes(b[at..at + 4].try_into().expect("4 bytes")) as u64
}

fn le64(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().expect("8 bytes"))
}

fn read_at(f: &mut File, offset: u64, len: usize) -> Result<Vec<u8>, IngestError> {
    f.seek(SeekFrom::Start(offset))?;
    let mut buf = Vec::with_capacity(len);
    f.take(len as u64).read_to_end(&mut buf)?;
    Ok(buf)
}

/// Reads the end-of-central-directory records (zip64 included) before the zip crate parses
/// the central directory, and rejects declared entry counts and directory sizes over the
/// limits. Every candidate record in the tail is checked, as a reader may fall back to an
/// earlier one.
fn check_zip_directory(f: &mut File, limits: &Limits) -> Result<(), IngestError> {
    let len = f.metadata()?.len();
    let tail_start = len.saturating_sub(ZIP_TAIL_BYTES);
    let tail = read_at(f, tail_start, (len - tail_start) as usize)?;
    let check = |entries: u64, dir_bytes: u64| -> Result<(), IngestError> {
        if entries > limits.max_archive_entries {
            return Err(IngestError::limit_at(
                "max_archive_entries",
                limits.max_archive_entries,
                format!("zip central directory declares {entries} entries"),
            ));
        }
        if dir_bytes > limits.max_zip_directory_bytes {
            return Err(IngestError::limit_at(
                "max_zip_directory_bytes",
                limits.max_zip_directory_bytes,
                format!("zip central directory declares {dir_bytes} bytes"),
            ));
        }
        Ok(())
    };
    let mut pos = tail.len().saturating_sub(22);
    loop {
        if tail.len() >= 22 && &tail[pos..pos + 4] == EOCD_SIG {
            let rec = &tail[pos..];
            let disk_entries = le16(rec, 8);
            let entries = le16(rec, 10);
            let dir_bytes = le32(rec, 12);
            let dir_offset = le32(rec, 16);
            let zip64 = entries == 0xFFFF
                || disk_entries == 0xFFFF
                || dir_bytes == 0xFFFF_FFFF
                || dir_offset == 0xFFFF_FFFF;
            // Saturated fields are placeholders for the zip64 record checked below.
            let real = |v: u64, placeholder: u64| if v == placeholder { 0 } else { v };
            check(
                real(entries, 0xFFFF).max(real(disk_entries, 0xFFFF)),
                real(dir_bytes, 0xFFFF_FFFF),
            )?;
            let eocd_offset = tail_start + pos as u64;
            if zip64 && eocd_offset >= 20 {
                let loc = read_at(f, eocd_offset - 20, 20)?;
                if loc.len() == 20 && &loc[..4] == EOCD64_LOCATOR_SIG {
                    let eocd64_offset = le64(&loc, 8);
                    if eocd64_offset < eocd_offset {
                        let rec64 = read_at(f, eocd64_offset, 56)?;
                        if rec64.len() == 56 && &rec64[..4] == EOCD64_SIG {
                            let disk_entries = le64(&rec64, 24);
                            let entries = le64(&rec64, 32);
                            check(entries.max(disk_entries), le64(&rec64, 40))?;
                        }
                    }
                }
            }
        }
        if pos == 0 {
            break;
        }
        pos -= 1;
    }
    Ok(())
}

/// Reader that fails once a shared allowance of bytes is used up.
struct CappedReader {
    inner: File,
    remaining: Arc<AtomicU64>,
    /// Limit reported when the allowance is used up.
    max: u64,
}

impl Read for CappedReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        let left = self.remaining.load(Ordering::Relaxed);
        if left != u64::MAX {
            if n as u64 > left {
                return Err(LimitIoError::io(
                    "max_zip_directory_bytes",
                    self.max,
                    "zip central directory",
                ));
            }
            self.remaining.store(left - n as u64, Ordering::Relaxed);
        }
        Ok(n)
    }
}

impl Seek for CappedReader {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        self.inner.seek(pos)
    }
}

fn parse_pax(data: &[u8]) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut rest = data;
    while !rest.is_empty() {
        let Some(sp) = rest.iter().position(|b| *b == b' ') else {
            break;
        };
        let Ok(len) = std::str::from_utf8(&rest[..sp])
            .unwrap_or("")
            .parse::<usize>()
        else {
            break;
        };
        if len <= sp || len > rest.len() {
            break;
        }
        let rec = &rest[sp + 1..len];
        let rec = rec.strip_suffix(b"\n").unwrap_or(rec);
        if let Some(eq) = rec.iter().position(|b| *b == b'=') {
            out.push((
                String::from_utf8_lossy(&rec[..eq]).into_owned(),
                String::from_utf8_lossy(&rec[eq + 1..]).into_owned(),
            ));
        }
        rest = &rest[len..];
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn walk_dir(
    dir: &Path,
    rel: &str,
    depth: u32,
    limits: &Limits,
    budget: &Budget,
    wanted: &dyn Fn(&str) -> bool,
    skipped: &mut dyn FnMut(&str, &'static str),
    visit: &mut EntryVisitor<'_>,
    count: &mut u64,
) -> Result<(), IngestError> {
    if depth > 64 {
        return Err(IngestError::limit_at(
            "max_path_bytes",
            limits.max_path_bytes,
            format!("directory nesting at {rel}"),
        ));
    }
    let mut entries: Vec<(String, PathBuf)> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok())
        .map(|e| (e.file_name().to_string_lossy().into_owned(), e.path()))
        .collect();
    entries.sort();
    for (name, path) in entries {
        let rel_path = if rel.is_empty() {
            name.clone()
        } else {
            format!("{rel}/{name}")
        };
        let meta = std::fs::symlink_metadata(&path)?;
        if meta.file_type().is_symlink() {
            skipped(&rel_path, "symbolic link");
            continue;
        }
        if meta.is_dir() {
            walk_dir(
                &path,
                &rel_path,
                depth + 1,
                limits,
                budget,
                wanted,
                skipped,
                visit,
                count,
            )?;
            continue;
        }
        if !meta.is_file() {
            skipped(&rel_path, "not a regular file");
            continue;
        }
        *count += 1;
        if *count > limits.max_archive_entries {
            return Err(IngestError::limit(
                "max_archive_entries",
                limits.max_archive_entries,
            ));
        }
        let Some(safe) = safe_entry_path(&rel_path, limits)? else {
            continue;
        };
        if meta.len() > limits.max_entry_bytes {
            return Err(IngestError::limit_at(
                "max_entry_bytes",
                limits.max_entry_bytes,
                safe,
            ));
        }
        if !wanted(&safe) {
            continue;
        }
        let f = File::open(&path)?;
        let mut r = LimitedReader::new(
            BufReader::new(f),
            budget.clone(),
            limits.max_entry_bytes,
            &safe,
        );
        visit(&safe, meta.len(), &mut r)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entry_paths() {
        let l = Limits::default();
        assert_eq!(
            safe_entry_path("a/./b//c.json", &l).unwrap().as_deref(),
            Some("a/b/c.json")
        );
        assert_eq!(
            safe_entry_path("a\\b.xml", &l).unwrap().as_deref(),
            Some("a/b.xml")
        );
        assert_eq!(safe_entry_path("dir/", &l).unwrap().as_deref(), Some("dir"));
        for bad in [
            "../x",
            "a/../../x",
            "/etc/passwd",
            "C:/x",
            "c:\\x",
            "a/..",
            "x\0y",
        ] {
            assert!(
                matches!(
                    safe_entry_path(bad, &l),
                    Err(IngestError::UnsafeEntry { .. })
                ),
                "{bad}"
            );
        }
    }

    #[test]
    fn pax_records() {
        let rec = b"19 path=a/b/c.json\n11 size=42\n";
        assert_eq!(
            parse_pax(rec),
            vec![
                ("path".into(), "a/b/c.json".into()),
                ("size".into(), "42".into())
            ]
        );
    }
}
