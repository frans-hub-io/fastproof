//! The HTTP client of the ClickHouse and OpenSearch backends: rustls (ring) with the Mozilla
//! roots plus an optional private CA, no redirects (requests go only to the configured host),
//! timeouts, and basic credentials.

use std::time::Duration;

use anyhow::Context;

use crate::config::HttpEndpoint;

/// A client bound to one endpoint.
#[derive(Clone, Debug)]
pub struct HttpBackend {
    pub client: reqwest::Client,
    pub endpoint: HttpEndpoint,
}

impl HttpBackend {
    pub fn new(endpoint: HttpEndpoint, what: &str) -> anyhow::Result<Self> {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let mut b = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(120))
            .user_agent(concat!("fastproof/", env!("CARGO_PKG_VERSION")));
        if endpoint.url.scheme() == "https" {
            let tls = crate::kafka::tls_config(endpoint.ca_file.as_deref())
                .with_context(|| format!("{what}_CA_FILE"))?;
            b = b.use_preconfigured_tls((*tls).clone());
        }
        Ok(Self {
            client: b
                .build()
                .with_context(|| format!("building the {what} client"))?,
            endpoint,
        })
    }

    /// A request to `path` (relative to the endpoint URL) with credentials.
    pub fn request(
        &self,
        method: reqwest::Method,
        path: &str,
    ) -> anyhow::Result<reqwest::RequestBuilder> {
        let base = self.endpoint.url.as_str().trim_end_matches('/');
        let url = format!("{base}/{}", path.trim_start_matches('/'));
        let mut rb = self.client.request(method, url);
        if let Some(u) = &self.endpoint.user {
            rb = rb.basic_auth(u, self.endpoint.password.as_ref().map(|p| p.expose()));
        }
        Ok(rb)
    }
}

/// The body of a failed response, bounded, for error messages.
pub async fn error_text(r: reqwest::Response) -> String {
    let status = r.status();
    let body = r.text().await.unwrap_or_default();
    let body: String = body.chars().take(500).collect();
    format!("HTTP {status}: {body}")
}
