//! Outbound calls of integrations go through one HTTP client that only reaches hosts allowed
//! by `FASTPROOF_EGRESS_ALLOW` (a comma list of `host`, `*.domain`, `*`, IP addresses, each
//! optionally with `:port`; empty denies everything).
//!
//! DNS rebinding is handled by resolving in the client's own resolver: every address a name
//! resolves to is checked, and the connection goes to the checked addresses. Names that match
//! only a wildcard must resolve to public addresses; a host listed exactly (or an IP address
//! listed exactly for the port of the URL) may be internal. Redirects are checked hop by hop
//! (credential headers are dropped when the origin changes), proxies from the environment are
//! ignored, and errors never carry the URL (which may hold a token).

use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use reqwest::header::{CONTENT_LENGTH, CONTENT_TYPE, LOCATION, TRANSFER_ENCODING};
use reqwest::{Method, RequestBuilder, StatusCode};

/// Largest response body read from an integration endpoint.
pub const MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
enum HostPattern {
    Exact(String),
    Suffix(String),
    Any,
    Ip(IpAddr),
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Rule {
    host: HostPattern,
    port: Option<u16>,
}

/// Why an outbound call was refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EgressError {
    Scheme,
    NotAllowed(String),
    Internal(String),
    Resolve(String),
}

impl fmt::Display for EgressError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EgressError::Scheme => f.write_str("egress denied: only http and https URLs"),
            EgressError::NotAllowed(h) => {
                write!(f, "egress denied: {h} is not in FASTPROOF_EGRESS_ALLOW")
            }
            EgressError::Internal(h) => write!(
                f,
                "egress denied: {h} resolves to an internal address (list the host exactly in FASTPROOF_EGRESS_ALLOW to allow it)"
            ),
            EgressError::Resolve(h) => write!(f, "cannot resolve {h}"),
        }
    }
}

impl std::error::Error for EgressError {}

/// The parsed allowlist.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EgressPolicy {
    rules: Vec<Rule>,
}

/// Loopback, private, link-local, shared, multicast, unspecified and reserved addresses,
/// including IPv4 addresses embedded in IPv6.
pub fn is_internal(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v) => internal_v4(v),
        IpAddr::V6(v) => internal_v6(v),
    }
}

fn internal_v4(v: Ipv4Addr) -> bool {
    let o = v.octets();
    v.is_loopback()
        || v.is_private()
        || v.is_link_local()
        || v.is_unspecified()
        || v.is_broadcast()
        || v.is_multicast()
        || o[0] == 0
        || (o[0] == 100 && (64..=127).contains(&o[1]))
        || (o[0] == 192 && o[1] == 0 && o[2] == 0)
        || (o[0] == 198 && (18..=19).contains(&o[1]))
        || o[0] >= 240
}

fn internal_v6(v: Ipv6Addr) -> bool {
    if let Some(m) = v.to_ipv4_mapped() {
        return internal_v4(m);
    }
    let s = v.segments();
    if s[0] == 0x64 && s[1] == 0xff9b {
        let e = Ipv4Addr::new((s[6] >> 8) as u8, s[6] as u8, (s[7] >> 8) as u8, s[7] as u8);
        return internal_v4(e);
    }
    if s[0] == 0x2002 {
        let e = Ipv4Addr::new((s[1] >> 8) as u8, s[1] as u8, (s[2] >> 8) as u8, s[2] as u8);
        return internal_v4(e);
    }
    v.is_loopback()
        || v.is_unspecified()
        || v.is_multicast()
        || (s[0] & 0xfe00) == 0xfc00
        || (s[0] & 0xffc0) == 0xfe80
        || (s[0] & 0xffc0) == 0xfec0
        || s[..6] == [0, 0, 0, 0, 0, 0]
}

fn split_port(s: &str) -> Result<(&str, Option<u16>), String> {
    if let Some(rest) = s.strip_prefix('[') {
        let (h, tail) = rest
            .split_once(']')
            .ok_or_else(|| format!("{s:?}: unclosed ["))?;
        return match tail.strip_prefix(':') {
            Some(p) => Ok((h, Some(p.parse().map_err(|_| format!("{s:?}: bad port"))?))),
            None if tail.is_empty() => Ok((h, None)),
            None => Err(format!("{s:?}: unexpected text after ]")),
        };
    }
    if s.matches(':').count() == 1 {
        let (h, p) = s.split_once(':').unwrap_or((s, ""));
        return Ok((h, Some(p.parse().map_err(|_| format!("{s:?}: bad port"))?)));
    }
    Ok((s, None))
}

impl EgressPolicy {
    /// Parses `FASTPROOF_EGRESS_ALLOW`.
    pub fn parse(s: &str) -> Result<Self, String> {
        let mut rules = Vec::new();
        for raw in s.split(',') {
            let item = raw.trim().to_ascii_lowercase();
            if item.is_empty() {
                continue;
            }
            let (h, port) = split_port(&item)?;
            let host = if h == "*" {
                HostPattern::Any
            } else if let Ok(ip) = h.parse::<IpAddr>() {
                HostPattern::Ip(ip)
            } else if let Some(suffix) = h.strip_prefix("*.") {
                if suffix.is_empty() || suffix.contains('*') {
                    return Err(format!("{item:?}: only a leading *. is supported"));
                }
                HostPattern::Suffix(suffix.trim_end_matches('.').to_string())
            } else {
                if h.contains('*')
                    || h.contains('/')
                    || !h
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_'))
                {
                    return Err(format!("{item:?}: not a host name"));
                }
                HostPattern::Exact(h.trim_end_matches('.').to_string())
            };
            rules.push(Rule { host, port });
        }
        if rules.len() > 500 {
            return Err("at most 500 entries".into());
        }
        Ok(Self { rules })
    }

    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    /// `Some(true)` when the rule lists the host exactly, `Some(false)` when it matches by a
    /// wildcard, `None` when it does not match (ports are not compared here).
    fn host_match(r: &Rule, h: &str, ip: Option<IpAddr>) -> Option<bool> {
        match (&r.host, ip) {
            (HostPattern::Ip(a), Some(b)) => (*a == b).then_some(true),
            (HostPattern::Ip(_), None) => None,
            (HostPattern::Exact(e), None) => (e == h).then_some(true),
            (HostPattern::Suffix(s), None) => (h.len() > s.len()
                && h.ends_with(s.as_str())
                && h.as_bytes()[h.len() - s.len() - 1] == b'.')
                .then_some(false),
            (HostPattern::Any, _) => Some(false),
            (_, Some(_)) => None,
        }
    }

    fn normalize(host: &str) -> (String, Option<IpAddr>) {
        let h = host
            .trim_start_matches('[')
            .trim_end_matches(']')
            .trim_end_matches('.')
            .to_ascii_lowercase();
        let ip = h.parse::<IpAddr>().ok();
        (h, ip)
    }

    /// Whether `host` (a name or an IP address) may be called on `port`: `Some(true)` when a
    /// rule lists it exactly for that port (internal addresses allowed), `Some(false)` when
    /// only a wildcard matches, `None` when it is not allowed.
    pub fn allows(&self, host: &str, port: u16) -> Option<bool> {
        let (h, ip) = Self::normalize(host);
        let mut found = None;
        for r in &self.rules {
            if r.port.is_some_and(|rp| rp != port) {
                continue;
            }
            match Self::host_match(r, &h, ip) {
                Some(true) => return Some(true),
                Some(false) => found = Some(false),
                None => {}
            }
        }
        found
    }

    /// Whether a rule (on any port) matches the name; with `exact_only`, only exact rules.
    /// For the resolvers, which do not see the port: the port was checked with the URL.
    fn names(&self, host: &str, exact_only: bool) -> bool {
        let (h, ip) = Self::normalize(host);
        self.rules
            .iter()
            .filter_map(|r| Self::host_match(r, &h, ip))
            .any(|exact| exact || !exact_only)
    }

    /// Checks a URL before a request: scheme, host and port, and the address of an IP host.
    /// Returns whether the host is listed exactly for that port (its names may then resolve to
    /// internal addresses).
    pub fn check_url(&self, u: &url::Url) -> Result<bool, EgressError> {
        if !matches!(u.scheme(), "http" | "https") {
            return Err(EgressError::Scheme);
        }
        let host = match u.host() {
            Some(url::Host::Domain(d)) => d.to_string(),
            Some(url::Host::Ipv4(v)) => v.to_string(),
            Some(url::Host::Ipv6(v)) => v.to_string(),
            None => return Err(EgressError::NotAllowed("(no host)".into())),
        };
        let port = u
            .port_or_known_default()
            .ok_or_else(|| EgressError::NotAllowed(host.clone()))?;
        let explicit = self
            .allows(&host, port)
            .ok_or_else(|| EgressError::NotAllowed(host.clone()))?;
        if let Ok(ip) = host.parse::<IpAddr>()
            && !explicit
            && is_internal(ip)
        {
            return Err(EgressError::Internal(host));
        }
        Ok(explicit)
    }

    /// Resolves an allowed host for a connection to `port` and returns only addresses it may
    /// connect to.
    pub async fn resolve(&self, host: &str, port: u16) -> Result<Vec<SocketAddr>, EgressError> {
        let explicit = self
            .allows(host, port)
            .ok_or_else(|| EgressError::NotAllowed(host.to_string()))?;
        lookup(host, port, explicit).await
    }
}

async fn lookup(
    host: &str,
    port: u16,
    allow_internal: bool,
) -> Result<Vec<SocketAddr>, EgressError> {
    let addrs: Vec<SocketAddr> = match host.parse::<IpAddr>() {
        Ok(ip) => vec![SocketAddr::new(ip, port)],
        Err(_) => tokio::net::lookup_host((host, port))
            .await
            .map_err(|_| EgressError::Resolve(host.to_string()))?
            .collect(),
    };
    if addrs.is_empty() {
        return Err(EgressError::Resolve(host.to_string()));
    }
    if !allow_internal && addrs.iter().any(|a| is_internal(a.ip())) {
        return Err(EgressError::Internal(host.to_string()));
    }
    Ok(addrs)
}

/// The resolver of one of the two clients. reqwest hands a resolver the name without the
/// port, so the decision that needs the port (exact listing or wildcard) is made per request
/// from the URL, and the request goes to the client of that decision: the public client never
/// connects to internal addresses, the explicit one is used only for hosts listed exactly for
/// the URL's port.
struct PolicyResolver {
    policy: Arc<EgressPolicy>,
    explicit: bool,
}

impl Resolve for PolicyResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let policy = self.policy.clone();
        let explicit = self.explicit;
        let host = name.as_str().to_string();
        Box::pin(async move {
            if !policy.names(&host, explicit) {
                return Err(EgressError::NotAllowed(host).into());
            }
            let addrs = lookup(&host, 0, explicit).await?;
            let it: Addrs = Box::new(addrs.into_iter());
            Ok(it)
        })
    }
}

/// An outbound call that failed.
#[derive(Debug)]
pub enum OutboundError {
    /// Refused by the egress policy (not retried).
    Denied(EgressError),
    /// Network or protocol error (retried); the text has no URL.
    Network(String),
    /// The response was larger than allowed.
    TooLarge,
}

impl fmt::Display for OutboundError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            OutboundError::Denied(e) => e.fmt(f),
            OutboundError::Network(e) => write!(f, "request failed: {e}"),
            OutboundError::TooLarge => f.write_str("response too large"),
        }
    }
}

impl std::error::Error for OutboundError {}

fn classify(e: reqwest::Error) -> OutboundError {
    let mut src: Option<&(dyn std::error::Error + 'static)> = Some(&e);
    while let Some(s) = src {
        if let Some(d) = s.downcast_ref::<EgressError>() {
            return OutboundError::Denied(d.clone());
        }
        src = s.source();
    }
    let kind = if e.is_timeout() {
        "timed out"
    } else if e.is_connect() {
        "connection failed"
    } else if e.is_redirect() {
        "too many redirects"
    } else if e.is_body() || e.is_decode() {
        "reading the response failed"
    } else {
        "error"
    };
    let detail = e.without_url().to_string();
    OutboundError::Network(format!("{kind} ({detail})"))
}

/// A response read in full (bounded).
#[derive(Debug)]
pub struct OutResponse {
    pub status: u16,
    pub headers: reqwest::header::HeaderMap,
    pub body: Bytes,
}

impl OutResponse {
    pub fn json<T: serde::de::DeserializeOwned>(&self) -> Result<T, OutboundError> {
        serde_json::from_slice(&self.body)
            .map_err(|_| OutboundError::Network("the response is not the expected JSON".into()))
    }
}

/// Redirects followed by [`Egress::send`].
pub const MAX_REDIRECTS: usize = 3;

/// Headers dropped when a redirect leaves the origin of the request.
const CREDENTIAL_HEADERS: [&str; 6] = [
    "authorization",
    "proxy-authorization",
    "cookie",
    "private-token",
    "job-token",
    "x-api-key",
];

/// The egress-checked HTTP client. It never follows redirects by itself: [`Egress::send`]
/// follows them hop by hop, checking every URL and dropping credential headers when the
/// origin changes, and [`Egress::send_no_redirect`] does not follow them at all.
#[derive(Clone)]
pub struct Egress {
    policy: Arc<EgressPolicy>,
    public: reqwest::Client,
    explicit: reqwest::Client,
}

impl fmt::Debug for Egress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Egress")
            .field("policy", &self.policy)
            .finish_non_exhaustive()
    }
}

fn client(policy: &Arc<EgressPolicy>, explicit: bool) -> anyhow::Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .user_agent(concat!("FastProof/", env!("CARGO_PKG_VERSION")))
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .dns_resolver(Arc::new(PolicyResolver {
            policy: policy.clone(),
            explicit,
        }))
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(20))
        .build()?)
}

/// The request for the next hop of a redirect.
fn redirected(mut req: reqwest::Request, status: StatusCode, to: url::Url) -> reqwest::Request {
    let method = req.method().clone();
    let to_get = (status == StatusCode::SEE_OTHER && method != Method::HEAD)
        || (matches!(status, StatusCode::MOVED_PERMANENTLY | StatusCode::FOUND)
            && method == Method::POST);
    let h = req.headers_mut();
    if to_get {
        for name in [CONTENT_TYPE, CONTENT_LENGTH, TRANSFER_ENCODING] {
            h.remove(name);
        }
    }
    if req.url().origin() != to.origin() {
        for name in CREDENTIAL_HEADERS {
            req.headers_mut().remove(name);
        }
    }
    if to_get {
        *req.method_mut() = Method::GET;
        *req.body_mut() = None;
    }
    *req.url_mut() = to;
    req
}

impl Egress {
    pub fn new(policy: Arc<EgressPolicy>) -> anyhow::Result<Self> {
        fastproof_blob::install_crypto_provider();
        Ok(Self {
            public: client(&policy, false)?,
            explicit: client(&policy, true)?,
            policy,
        })
    }

    pub fn policy(&self) -> &EgressPolicy {
        &self.policy
    }

    /// A request to `url` after checking it against the policy.
    pub fn request(&self, method: Method, url: &str) -> Result<RequestBuilder, OutboundError> {
        let u = url::Url::parse(url).map_err(|_| OutboundError::Network("invalid URL".into()))?;
        self.policy.check_url(&u).map_err(OutboundError::Denied)?;
        Ok(self.public.request(method, u))
    }

    /// Sends, following up to [`MAX_REDIRECTS`] redirects, and reads the response body up to
    /// `max` bytes.
    pub async fn send(&self, rb: RequestBuilder, max: usize) -> Result<OutResponse, OutboundError> {
        self.send_with(rb, max, true).await
    }

    /// Sends without following redirects (a 3xx answer is returned as it is), for requests
    /// that carry credentials.
    pub async fn send_no_redirect(
        &self,
        rb: RequestBuilder,
        max: usize,
    ) -> Result<OutResponse, OutboundError> {
        self.send_with(rb, max, false).await
    }

    async fn send_with(
        &self,
        rb: RequestBuilder,
        max: usize,
        follow: bool,
    ) -> Result<OutResponse, OutboundError> {
        let mut req = rb.build().map_err(classify)?;
        let mut hops = 0;
        let mut resp = loop {
            let explicit = self
                .policy
                .check_url(req.url())
                .map_err(OutboundError::Denied)?;
            let http = if explicit {
                &self.explicit
            } else {
                &self.public
            };
            let again = if follow { req.try_clone() } else { None };
            let resp = http.execute(req).await.map_err(classify)?;
            let location = resp
                .headers()
                .get(LOCATION)
                .and_then(|l| l.to_str().ok())
                .map(str::to_string);
            match (again, location) {
                (Some(next), Some(loc)) if resp.status().is_redirection() => {
                    if hops >= MAX_REDIRECTS {
                        return Err(OutboundError::Denied(EgressError::NotAllowed(
                            "(too many redirects)".into(),
                        )));
                    }
                    let to = next.url().join(&loc).map_err(|_| {
                        OutboundError::Network("the redirect location is not a URL".into())
                    })?;
                    req = redirected(next, resp.status(), to);
                    hops += 1;
                }
                _ => break resp,
            }
        };
        let status = resp.status().as_u16();
        let headers = resp.headers().clone();
        let mut body = Vec::new();
        while let Some(chunk) = resp.chunk().await.map_err(classify)? {
            if body.len() + chunk.len() > max {
                return Err(OutboundError::TooLarge);
            }
            body.extend_from_slice(&chunk);
        }
        Ok(OutResponse {
            status,
            headers,
            body: Bytes::from(body),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_internal_addresses() {
        for a in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.169.254",
            "100.64.0.1",
            "0.0.0.0",
            "::1",
            "::",
            "fd00::1",
            "fe80::1",
            "::ffff:10.0.0.1",
            "64:ff9b::a00:1",
            "2002:a00:1::",
            "::127.0.0.1",
            "224.0.0.1",
        ] {
            assert!(is_internal(a.parse().unwrap()), "{a}");
        }
        for a in [
            "198.51.100.7",
            "203.0.113.9",
            "8.8.8.8",
            "2001:db8::1",
            "::ffff:198.51.100.1",
        ] {
            assert!(!is_internal(a.parse().unwrap()), "{a}");
        }
    }

    #[test]
    fn allowlist_matching() {
        let p = EgressPolicy::parse(
            "hooks.example.com, *.example.org, 127.0.0.1:8080, gitlab.example.net:443",
        )
        .unwrap();
        assert_eq!(p.allows("hooks.example.com", 443), Some(true));
        assert_eq!(p.allows("HOOKS.example.com.", 8443), Some(true));
        assert_eq!(p.allows("a.example.org", 443), Some(false));
        assert_eq!(p.allows("example.org", 443), None);
        assert_eq!(p.allows("evil-example.org", 443), None);
        assert_eq!(p.allows("127.0.0.1", 8080), Some(true));
        assert_eq!(p.allows("127.0.0.1", 8081), None);
        assert_eq!(p.allows("gitlab.example.net", 80), None);
        assert_eq!(p.allows("other.example.com", 443), None);
        assert!(EgressPolicy::parse("").unwrap().is_empty());
        assert_eq!(
            EgressPolicy::parse("").unwrap().allows("example.com", 443),
            None
        );
        assert!(EgressPolicy::parse("a*b.example.com").is_err());
        assert!(EgressPolicy::parse("host:notaport").is_err());
        let any = EgressPolicy::parse("*").unwrap();
        assert_eq!(any.allows("example.com", 443), Some(false));
        let u = |s: &str| url::Url::parse(s).unwrap();
        assert_eq!(any.check_url(&u("https://198.51.100.4/x")), Ok(false));
        assert_eq!(
            any.check_url(&u("http://10.0.0.1/")),
            Err(EgressError::Internal("10.0.0.1".into()))
        );
        assert_eq!(
            any.check_url(&u("http://[::ffff:127.0.0.1]/")),
            Err(EgressError::Internal("::ffff:127.0.0.1".into()))
        );
        assert_eq!(
            any.check_url(&u("ftp://example.com/")),
            Err(EgressError::Scheme)
        );
        assert_eq!(p.check_url(&u("http://127.0.0.1:8080/hook")), Ok(true));
        let mixed = EgressPolicy::parse("gitlab.example.net:443, *").unwrap();
        assert_eq!(mixed.check_url(&u("https://gitlab.example.net/")), Ok(true));
        assert_eq!(
            mixed.check_url(&u("http://gitlab.example.net:22/")),
            Ok(false),
            "another port of an exactly listed host is only a wildcard match"
        );
    }

    #[tokio::test]
    async fn wildcard_names_must_resolve_publicly() {
        let p = EgressPolicy::parse("*.localhost, localhost").unwrap();
        assert!(p.resolve("localhost", 80).await.is_ok());
        assert!(matches!(
            p.resolve("foo.localhost", 80).await,
            Err(EgressError::Internal(_)) | Err(EgressError::Resolve(_))
        ));
        assert!(matches!(
            p.resolve("example.com", 80).await,
            Err(EgressError::NotAllowed(_))
        ));
        let ported = EgressPolicy::parse("localhost:8080, *").unwrap();
        assert!(ported.resolve("localhost", 8080).await.is_ok());
        assert!(matches!(
            ported.resolve("localhost", 22).await,
            Err(EgressError::Internal(_))
        ));
    }

    type Seen = Arc<std::sync::Mutex<Vec<String>>>;

    /// A listener answering every connection with `answer`, keeping the request heads.
    async fn listener(answer: String) -> (u16, Seen) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = l.local_addr().unwrap().port();
        let seen: Seen = Arc::default();
        let s = seen.clone();
        tokio::spawn(async move {
            while let Ok((mut sock, _)) = l.accept().await {
                let mut buf = vec![0u8; 8192];
                let n = sock.read(&mut buf).await.unwrap_or(0);
                s.lock()
                    .unwrap()
                    .push(String::from_utf8_lossy(&buf[..n]).to_ascii_lowercase());
                let _ = sock.write_all(answer.as_bytes()).await;
            }
        });
        (port, seen)
    }

    const NO_CONTENT: &str =
        "HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";

    #[tokio::test]
    async fn the_port_survives_name_resolution() {
        let (port, seen) = listener(NO_CONTENT.into()).await;
        let other = if port == 1 { 2 } else { 1 };
        let e = Egress::new(Arc::new(
            EgressPolicy::parse(&format!("localhost:{other}, *")).unwrap(),
        ))
        .unwrap();
        let rb = e
            .request(Method::GET, &format!("http://localhost:{port}/"))
            .unwrap();
        let err = e.send(rb, 1024).await.unwrap_err();
        assert!(
            matches!(err, OutboundError::Denied(EgressError::Internal(_))),
            "localhost:{other} must not open localhost:{port}: {err}"
        );
        assert!(seen.lock().unwrap().is_empty());
        let ok = Egress::new(Arc::new(
            EgressPolicy::parse(&format!("localhost:{port}, *")).unwrap(),
        ))
        .unwrap();
        let rb = ok
            .request(Method::GET, &format!("http://localhost:{port}/"))
            .unwrap();
        assert_eq!(ok.send(rb, 1024).await.unwrap().status, 204);
    }

    #[tokio::test]
    async fn redirects_drop_credentials_across_origins() {
        let (to, landed) = listener(NO_CONTENT.into()).await;
        let (from, _) = listener(format!(
            "HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:{to}/landed\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        ))
        .await;
        let e = Egress::new(Arc::new(EgressPolicy::parse("127.0.0.1").unwrap())).unwrap();
        let url = format!("http://127.0.0.1:{from}/start");
        let with_token = || {
            e.request(Method::POST, &url)
                .unwrap()
                .header("PRIVATE-TOKEN", "glpat-secret")
                .bearer_auth("also-secret")
                .body("{}")
        };
        let r = e.send_no_redirect(with_token(), 1024).await.unwrap();
        assert_eq!(r.status, 302);
        assert!(landed.lock().unwrap().is_empty());
        let r = e.send(with_token(), 1024).await.unwrap();
        assert_eq!(r.status, 204);
        let heads = landed.lock().unwrap().clone();
        assert_eq!(heads.len(), 1);
        assert!(heads[0].starts_with("get /landed"), "{}", heads[0]);
        for secret in [
            "private-token",
            "authorization",
            "glpat-secret",
            "also-secret",
        ] {
            assert!(!heads[0].contains(secret), "{secret} leaked: {}", heads[0]);
        }
        let same = redirected(
            with_token().build().unwrap(),
            StatusCode::TEMPORARY_REDIRECT,
            url::Url::parse(&format!("http://127.0.0.1:{from}/again")).unwrap(),
        );
        assert_eq!(same.method(), Method::POST);
        assert!(
            same.headers().contains_key("private-token"),
            "same origin keeps them"
        );
    }

    #[tokio::test]
    async fn errors_never_carry_the_url() {
        let e = Egress::new(Arc::new(EgressPolicy::parse("127.0.0.1").unwrap())).unwrap();
        let rb = e
            .request(
                Method::POST,
                "http://127.0.0.1:1/botSECRET-TOKEN-123/sendMessage",
            )
            .unwrap();
        let err = e.send(rb, 1024).await.unwrap_err();
        let text = format!("{err} {err:?}");
        assert!(matches!(err, OutboundError::Network(_)), "{text}");
        assert!(!text.contains("SECRET-TOKEN-123"), "{text}");
        let denied = e.request(Method::GET, "http://hooks.example.com/botSECRET-TOKEN-123");
        assert!(!format!("{:?}", denied.unwrap_err()).contains("SECRET"));
    }
}
