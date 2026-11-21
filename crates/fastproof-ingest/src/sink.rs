use std::io::Read;

use fastproof_model::AttachmentId;

use crate::error::IngestError;

/// Receives attachment bodies while the upload is streamed, so the caller can hash and
/// store them without buffering whole files.
///
/// `source` is the upload-relative path (or `inline/<n>/<name>` for bodies that were inline
/// in a report). `size` is the size declared by the archive or file system: do not trust it
/// for allocation; the reader itself is bounded by the ingest limits.
pub trait AttachmentSink {
    fn put(
        &mut self,
        source: &str,
        content_type: Option<&str>,
        size: u64,
        body: &mut dyn Read,
    ) -> Result<AttachmentId, IngestError>;
}

/// Sink that reads and discards bodies (useful for parsing without storage).
#[derive(Debug, Default)]
pub struct NullSink {
    pub count: u64,
    pub bytes: u64,
}

impl AttachmentSink for NullSink {
    fn put(
        &mut self,
        source: &str,
        _content_type: Option<&str>,
        _size: u64,
        body: &mut dyn Read,
    ) -> Result<AttachmentId, IngestError> {
        self.bytes += std::io::copy(body, &mut std::io::sink()).map_err(IngestError::from_io)?;
        self.count += 1;
        Ok(AttachmentId(format!("null:{}:{source}", self.count)))
    }
}

/// An attachment kept by [`MemorySink`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryAttachment {
    pub source: String,
    pub content_type: Option<String>,
    pub bytes: Vec<u8>,
}

/// Sink that keeps bodies in memory (tests and small tools).
#[derive(Debug, Default)]
pub struct MemorySink {
    pub items: Vec<MemoryAttachment>,
}

impl MemorySink {
    pub fn get(&self, id: &AttachmentId) -> Option<&MemoryAttachment> {
        id.0.strip_prefix("mem:")
            .and_then(|n| n.parse::<usize>().ok())
            .and_then(|n| self.items.get(n))
    }
}

impl AttachmentSink for MemorySink {
    fn put(
        &mut self,
        source: &str,
        content_type: Option<&str>,
        _size: u64,
        body: &mut dyn Read,
    ) -> Result<AttachmentId, IngestError> {
        let mut bytes = Vec::new();
        body.read_to_end(&mut bytes).map_err(IngestError::from_io)?;
        self.items.push(MemoryAttachment {
            source: source.to_string(),
            content_type: content_type.map(str::to_string),
            bytes,
        });
        Ok(AttachmentId(format!("mem:{}", self.items.len() - 1)))
    }
}
