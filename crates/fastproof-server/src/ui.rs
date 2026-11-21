//! The embedded web UI (`ui/dist`, built by Vite). A binary built without it serves a short
//! page that says so.

use axum::Router;
use franshub_http::{Csp, SpaConfig, spa_router};

#[derive(rust_embed::RustEmbed)]
#[folder = "../../ui/dist"]
#[allow_missing = true]
struct Ui;

/// Text in `index.html` replaced with the per-response CSP nonce for `<style>` elements.
pub const NONCE_PLACEHOLDER: &str = "__CSP_NONCE__";

const NOT_BUILT: &str = "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><meta name=\"color-scheme\" content=\"dark light\"><title>FastProof</title></head><body><h1>FastProof</h1><p>This build of FastProof does not include the web UI. The API is available at <a href=\"/api/docs/\">/api/docs/</a>.</p></body></html>";

/// The UI policy: strict, plus `data:`/`blob:` images (Allure image diffs) and same-origin or
/// `blob:` media (videos).
pub fn ui_csp() -> Csp {
    let base = Csp::strict();
    base.clone()
        .allow("img-src", "blob:")
        .and_then(|c| c.allow("media-src", "'self'"))
        .and_then(|c| c.allow("media-src", "blob:"))
        .unwrap_or(base)
}

/// Whether the binary has the UI.
pub fn embedded() -> bool {
    <Ui as rust_embed::RustEmbed>::get("index.html").is_some()
}

/// The UI router.
pub fn router<S: Clone + Send + Sync + 'static>() -> Router<S> {
    spa_router::<Ui, S>(SpaConfig {
        base: "/".into(),
        csp: ui_csp(),
        immutable_prefix: "assets/".into(),
        nonce_placeholder: Some(NONCE_PLACEHOLDER.into()),
        reserved_prefixes: vec!["api/".into(), "auth/".into()],
        not_built_html: NOT_BUILT.into(),
    })
}
