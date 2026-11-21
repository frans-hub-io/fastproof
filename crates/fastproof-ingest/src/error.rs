use thiserror::Error;

/// Fatal ingest errors. Malformed individual files are not fatal; they become warnings.
#[derive(Debug, Error)]
pub enum IngestError {
    /// A configured limit was exceeded; `limit` is the field name in [`crate::Limits`].
    #[error("limit {limit} exceeded (max {max}){}", context.as_ref().map(|c| format!(": {c}")).unwrap_or_default())]
    LimitExceeded {
        limit: &'static str,
        max: u64,
        context: Option<String>,
    },
    #[error("unsafe archive entry {entry:?}: {reason}")]
    UnsafeEntry { entry: String, reason: &'static str },
    #[error("unsupported upload: {0}")]
    Unsupported(String),
    #[error("invalid archive: {0}")]
    Archive(String),
    #[error("attachment sink failed: {0}")]
    Sink(String),
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
}

impl IngestError {
    pub(crate) fn limit(limit: &'static str, max: u64) -> IngestError {
        IngestError::LimitExceeded {
            limit,
            max,
            context: None,
        }
    }

    pub(crate) fn limit_at(
        limit: &'static str,
        max: u64,
        context: impl Into<String>,
    ) -> IngestError {
        IngestError::LimitExceeded {
            limit,
            max,
            context: Some(context.into()),
        }
    }

    /// Name of the limit that was hit, if this is a limit error.
    pub fn limit_name(&self) -> Option<&'static str> {
        match self {
            IngestError::LimitExceeded { limit, .. } => Some(limit),
            IngestError::Io(e) => e
                .get_ref()
                .and_then(|inner| inner.downcast_ref::<LimitIoError>())
                .map(|l| l.limit),
            _ => None,
        }
    }

    /// Converts an i/o error that wraps a limit violation back into a limit error.
    pub(crate) fn from_io(e: std::io::Error) -> IngestError {
        if let Some(l) = e.get_ref().and_then(|i| i.downcast_ref::<LimitIoError>()) {
            return IngestError::limit_at(l.limit, l.max, l.context.clone());
        }
        IngestError::Io(e)
    }
}

/// Limit violation raised inside a `Read` implementation.
#[derive(Debug, Error)]
#[error("limit {limit} exceeded (max {max}): {context}")]
pub(crate) struct LimitIoError {
    pub limit: &'static str,
    pub max: u64,
    pub context: String,
}

impl LimitIoError {
    pub(crate) fn io(limit: &'static str, max: u64, context: impl Into<String>) -> std::io::Error {
        std::io::Error::other(LimitIoError {
            limit,
            max,
            context: context.into(),
        })
    }
}
