//! The Playwright trace viewer (playwright-core 1.65.0 `lib/vite/traceViewer`, Apache-2.0),
//! vendored under `assets/trace-viewer` (see its `VERSION`), served at `/trace/`. Open
//! `/trace/?trace=/api/v1/projects/<key>/attachments/<id>/content`: the viewer's service worker
//! fetches the trace from the same origin with the session cookie.

use std::borrow::Cow;

use axum::Router;
use axum::extract::Path;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;
use franshub_http::{IMMUTABLE, NO_CACHE, content_type};
use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "assets/trace-viewer"]
struct TraceViewer;

/// Policy of the viewer pages: its own scripts, inline styles, a service worker, same-origin
/// snapshot frames, and `data:`/`blob:` images and media from traces.
pub const CSP: &str = "default-src 'none'; script-src 'self'; style-src 'self' 'unsafe-inline'; \
    img-src 'self' data: blob:; media-src 'self' data: blob:; font-src 'self' data:; \
    connect-src 'self' data: blob:; worker-src 'self'; frame-src 'self' blob:; manifest-src 'self'; \
    frame-ancestors 'self'; base-uri 'none'; form-action 'none'; object-src 'none'";

fn serve(path: &str) -> Response {
    let path = if path.is_empty() { "index.html" } else { path };
    let clean = !path
        .split('/')
        .any(|seg| seg == ".." || seg == "." || seg.is_empty())
        && !path.contains('\\');
    let file = if clean { TraceViewer::get(path) } else { None };
    let Some(f) = file else {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    };
    let body: Cow<'static, [u8]> = f.data;
    let mut r = (StatusCode::OK, body).into_response();
    let h = r.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(content_type(path)),
    );
    h.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(if path.starts_with("assets/") {
            IMMUTABLE
        } else {
            NO_CACHE
        }),
    );
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(CSP),
    );
    h.insert(
        header::X_FRAME_OPTIONS,
        HeaderValue::from_static("SAMEORIGIN"),
    );
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    h.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    if path == "sw.bundle.js" {
        h.insert(
            "service-worker-allowed",
            HeaderValue::from_static("/trace/"),
        );
    }
    r
}

/// The viewer router.
pub fn router<S: Clone + Send + Sync + 'static>() -> Router<S> {
    Router::new()
        .route("/trace", get(|| async { Redirect::permanent("/trace/") }))
        .route("/trace/", get(|| async { serve("index.html") }))
        .route(
            "/trace/{*path}",
            get(|Path(p): Path<String>| async move { serve(&p) }),
        )
}

#[cfg(test)]
mod tests {
    #[test]
    fn vendored_viewer_is_complete() {
        for f in [
            "index.html",
            "sw.bundle.js",
            "snapshot.html",
            "LICENSE",
            "NOTICE",
            "VERSION",
        ] {
            assert!(super::TraceViewer::get(f).is_some(), "{f}");
        }
        let r = super::serve("../Cargo.toml");
        assert_eq!(r.status(), axum::http::StatusCode::NOT_FOUND);
    }
}
