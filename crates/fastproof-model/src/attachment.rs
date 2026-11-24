use serde::{Deserialize, Serialize};

/// Identifier returned by the attachment sink (typically a content hash or object key).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(transparent)]
pub struct AttachmentId(pub String);

impl std::fmt::Display for AttachmentId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// What a viewer should do with an attachment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum AttachmentKind {
    Image,
    Video,
    Trace,
    ImageDiff,
    HttpExchange,
    Text,
    Log,
    Json,
    #[default]
    Other,
}

impl AttachmentKind {
    /// Infers the kind from the content type and the file name.
    pub fn infer(content_type: Option<&str>, name: &str) -> AttachmentKind {
        let ct = content_type
            .map(|c| {
                c.split(';')
                    .next()
                    .unwrap_or("")
                    .trim()
                    .to_ascii_lowercase()
            })
            .unwrap_or_default();
        match ct.as_str() {
            "application/vnd.allure.image.diff" => return AttachmentKind::ImageDiff,
            "application/vnd.allure.playwright-trace" => return AttachmentKind::Trace,
            "application/vnd.allure.http+json" => return AttachmentKind::HttpExchange,
            "application/json" | "text/json" => return AttachmentKind::Json,
            "text/x-log" => return AttachmentKind::Log,
            _ => {}
        }
        if ct.starts_with("image/") {
            return AttachmentKind::Image;
        }
        if ct.starts_with("video/") {
            return AttachmentKind::Video;
        }
        if ct.ends_with("+json") {
            return AttachmentKind::Json;
        }
        let lower = name.to_ascii_lowercase();
        let ext = lower.rsplit_once('.').map(|(_, e)| e).unwrap_or("");
        if lower.ends_with("trace.zip") || (ext == "zip" && lower.contains("trace")) {
            return AttachmentKind::Trace;
        }
        // Playwright attaches its trace as `trace` (application/zip); the ReportPortal agent
        // sends it as `<test title>_trace`, without an extension.
        if ct == "application/zip" && (lower == "trace" || lower.ends_with("_trace")) {
            return AttachmentKind::Trace;
        }
        if ext == "httpexchange" || ext == "imagediff" {
            return if ext == "imagediff" {
                AttachmentKind::ImageDiff
            } else {
                AttachmentKind::HttpExchange
            };
        }
        if ct.starts_with("text/") {
            return if ext == "log" || lower == "stdout" || lower == "stderr" {
                AttachmentKind::Log
            } else {
                AttachmentKind::Text
            };
        }
        match ext {
            "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "svg" => AttachmentKind::Image,
            "webm" | "mp4" | "mov" => AttachmentKind::Video,
            "json" => AttachmentKind::Json,
            "log" => AttachmentKind::Log,
            "txt" | "csv" | "md" | "xml" | "html" | "htm" => AttachmentKind::Text,
            _ => AttachmentKind::Other,
        }
    }
}

/// Reference from a result, step or globals to an attachment file of the upload.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct AttachmentRef {
    pub name: String,
    /// Original reference as written (file name, relative or absolute path).
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    pub kind: AttachmentKind,
    /// Id assigned by the sink when the file was found in the upload and stored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<AttachmentId>,
    /// Upload-relative path the reference resolved to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_path: Option<String>,
    /// True when the referenced file was not present in the upload (it may come in a
    /// later upload of the same launch).
    #[serde(default, skip_serializing_if = "crate::result::is_false")]
    pub missing: bool,
}

impl AttachmentRef {
    pub fn new(
        name: impl Into<String>,
        source: impl Into<String>,
        content_type: Option<String>,
    ) -> Self {
        let name = name.into();
        let source = source.into();
        let kind = AttachmentKind::infer(content_type.as_deref(), &source);
        AttachmentRef {
            name,
            source,
            content_type,
            size: None,
            kind,
            id: None,
            resolved_path: None,
            missing: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn infer_kinds() {
        assert_eq!(
            AttachmentKind::infer(Some("image/png"), "a"),
            AttachmentKind::Image
        );
        assert_eq!(
            AttachmentKind::infer(Some("application/vnd.allure.image.diff"), "a"),
            AttachmentKind::ImageDiff
        );
        assert_eq!(
            AttachmentKind::infer(Some("application/zip"), "trace.zip"),
            AttachmentKind::Trace
        );
        assert_eq!(
            AttachmentKind::infer(Some("text/plain"), "x.txt"),
            AttachmentKind::Text
        );
        assert_eq!(
            AttachmentKind::infer(Some("text/plain"), "stdout"),
            AttachmentKind::Log
        );
        assert_eq!(
            AttachmentKind::infer(None, "video.webm"),
            AttachmentKind::Video
        );
        assert_eq!(
            AttachmentKind::infer(Some("application/vnd.allure.http+json"), "x"),
            AttachmentKind::HttpExchange
        );
        assert_eq!(AttachmentKind::infer(None, "bin"), AttachmentKind::Other);
    }

    #[test]
    fn playwright_traces_without_an_extension() {
        for name in [
            "checkout-passes-with-steps_trace",
            "Renders-Name_TRACE",
            "trace",
        ] {
            assert_eq!(
                AttachmentKind::infer(Some("application/zip"), name),
                AttachmentKind::Trace,
                "{name}"
            );
        }
        for (ct, name) in [
            (Some("application/zip"), "artifacts"),
            (Some("application/zip"), "backup.zip"),
            (Some("application/zip"), "stacktrace"),
            (Some("application/zip"), "trace_dump"),
            (Some("application/octet-stream"), "checkout_trace"),
            (Some("text/plain"), "checkout_trace"),
            (None, "checkout_trace"),
        ] {
            assert_ne!(
                AttachmentKind::infer(ct, name),
                AttachmentKind::Trace,
                "{ct:?} {name}"
            );
        }
    }
}
