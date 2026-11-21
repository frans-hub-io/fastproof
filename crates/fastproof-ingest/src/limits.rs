use std::io::Read;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

use crate::error::{IngestError, LimitIoError};

const KIB: u64 = 1024;
const MIB: u64 = 1024 * KIB;
const GIB: u64 = 1024 * MIB;

/// Security limits of one upload. Every limit error names the field that was hit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Limits {
    /// Size of the upload file as received (compressed archive or single file).
    pub max_upload_bytes: u64,
    /// Number of entries in an archive or files in a directory.
    pub max_archive_entries: u64,
    /// Byte budget charged for every expansion: every byte read from an entry after
    /// decompression (both passes), inline attachment bodies and fixture copies.
    pub max_total_uncompressed_bytes: u64,
    /// Size of a single entry after decompression.
    pub max_entry_bytes: u64,
    /// Size of a single report document (JSON or XML) that is parsed in memory.
    pub max_document_bytes: u64,
    /// Length of an entry path inside an archive.
    pub max_path_bytes: u64,
    pub max_json_depth: u32,
    pub max_xml_depth: u32,
    pub max_xml_attributes: u32,
    /// Length of a single JSON string or XML text/attribute value.
    pub max_string_bytes: u64,
    /// Kept size of stdout, stderr, message and trace per result (longer text is truncated).
    pub max_log_bytes: u64,
    /// Steps per result, counted across nesting, fixtures included.
    pub max_steps_per_result: u64,
    /// Attachment references resolved and stored per upload.
    pub max_attachments: u64,
    /// Results per upload (each attempt counts).
    pub max_results: u64,
    /// Attempts (retries included) of one test in one report; each attempt copies the
    /// shared parts of the test.
    pub max_attempts_per_result: u64,
    /// Labels (tags, properties, annotations) per result.
    pub max_labels_per_result: u64,
    /// Kept length of a test name and full name; longer names are truncated with a warning.
    pub max_name_bytes: u64,
    /// JSON values (scalars, arrays and objects, object members counted once) in one
    /// document, checked before the document is parsed.
    pub max_json_values: u64,
    /// Attachment references per result, steps included.
    pub max_attachments_per_result: u64,
    /// Size of a zip central directory, declared or read while the archive is opened.
    pub max_zip_directory_bytes: u64,
    /// Decoded inline attachment bodies (Playwright `body`) held in memory per upload
    /// until they are passed to the sink.
    pub max_inline_attachment_bytes: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            max_upload_bytes: GIB,
            max_archive_entries: 100_000,
            max_total_uncompressed_bytes: 4 * GIB,
            max_entry_bytes: 512 * MIB,
            max_document_bytes: 64 * MIB,
            max_path_bytes: 1024,
            max_json_depth: 128,
            max_xml_depth: 64,
            max_xml_attributes: 64,
            max_string_bytes: 8 * MIB,
            max_log_bytes: MIB,
            max_steps_per_result: 10_000,
            max_attachments: 200_000,
            max_results: 200_000,
            max_attempts_per_result: 100,
            max_labels_per_result: 500,
            max_name_bytes: 4 * KIB,
            max_json_values: 5_000_000,
            max_attachments_per_result: 1000,
            max_zip_directory_bytes: 64 * MIB,
            max_inline_attachment_bytes: 256 * MIB,
        }
    }
}

/// Shared byte budget of one upload.
#[derive(Debug, Clone)]
pub(crate) struct Budget {
    max: u64,
    used: Arc<AtomicU64>,
}

impl Budget {
    pub(crate) fn new(max: u64) -> Budget {
        Budget {
            max,
            used: Arc::new(AtomicU64::new(0)),
        }
    }

    pub(crate) fn used(&self) -> u64 {
        self.used.load(Ordering::Relaxed)
    }

    fn charge_raw(&self, n: u64) -> bool {
        let prev = self.used.fetch_add(n, Ordering::Relaxed);
        prev.saturating_add(n) <= self.max
    }

    pub(crate) fn charge(&self, n: u64, context: &str) -> Result<(), IngestError> {
        if self.charge_raw(n) {
            Ok(())
        } else {
            Err(IngestError::limit_at(
                "max_total_uncompressed_bytes",
                self.max,
                context.to_string(),
            ))
        }
    }
}

/// Reader that charges the budget for every byte and enforces the per-entry limit while
/// streaming, without trusting sizes declared in headers.
pub(crate) struct LimitedReader<R> {
    inner: R,
    budget: Budget,
    read: u64,
    max_entry: u64,
    entry: String,
}

impl<R: Read> LimitedReader<R> {
    pub(crate) fn new(inner: R, budget: Budget, max_entry: u64, entry: impl Into<String>) -> Self {
        LimitedReader {
            inner,
            budget,
            read: 0,
            max_entry,
            entry: entry.into(),
        }
    }
}

impl<R: Read> Read for LimitedReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.read += n as u64;
        if self.read > self.max_entry {
            return Err(LimitIoError::io(
                "max_entry_bytes",
                self.max_entry,
                self.entry.clone(),
            ));
        }
        if !self.budget.charge_raw(n as u64) {
            return Err(LimitIoError::io(
                "max_total_uncompressed_bytes",
                self.budget.max,
                self.entry.clone(),
            ));
        }
        Ok(n)
    }
}

/// Reader that only counts bytes against the budget (for the outer compressed stream).
pub(crate) struct BudgetReader<R> {
    pub(crate) inner: R,
    pub(crate) budget: Budget,
    pub(crate) context: &'static str,
}

impl<R: Read> Read for BudgetReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        if !self.budget.charge_raw(n as u64) {
            return Err(LimitIoError::io(
                "max_total_uncompressed_bytes",
                self.budget.max,
                self.context,
            ));
        }
        Ok(n)
    }
}

/// Reads at most `max` bytes; more is a limit error naming `limit`.
pub(crate) fn read_bounded(
    r: &mut dyn Read,
    max: u64,
    limit: &'static str,
    context: &str,
) -> Result<Vec<u8>, IngestError> {
    let mut buf = Vec::new();
    let n = r
        .take(max.saturating_add(1))
        .read_to_end(&mut buf)
        .map_err(IngestError::from_io)?;
    if n as u64 > max {
        return Err(IngestError::limit_at(limit, max, context.to_string()));
    }
    Ok(buf)
}
