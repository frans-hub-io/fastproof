//! Outbound calls of integrations go through one HTTP client that only reaches hosts allowed
//! by `FASTPROOF_EGRESS_ALLOW` (a comma list of `host`, `*.domain`, `*`, IP addresses, each
//! optionally with `:port`; empty denies everything).
//!
//! DNS rebinding is handled by resolving in the client's own resolver: every address a name
//! resolves to is checked, and the connection goes to the checked addresses. Names that match
//! only a wildcard must resolve to public addresses; a host listed exactly (or an IP address
//! listed exactly) may be internal. Redirects are checked hop by hop, proxies from the
//! environment are ignored, and errors never carry the URL (which may hold a token).

use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use reqwest::{Method, RequestBuilder};

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

    /// Whether `host` (a name or an IP address) may be called on `port` (any port when
    /// `None`): `Some(true)` when it is listed exactly (internal addresses allowed),
    /// `Some(false)` when only a wildcard matches, `None` when it is not allowed.
    pub fn allows(&self, host: &str, port: Option<u16>) -> Option<bool> {
        let h = host
            .trim_start_matches('[')
            .trim_end_matches(']')
            .trim_end_matches('.')
            .to_ascii_lowercase();
        let ip = h.parse::<IpAddr>().ok();
        let mut found = None;
        for r in &self.rules {
            if let (Some(rp), Some(p)) = (r.port, port)
                && rp != p
            {
                continue;
            }
            let m = match (&r.host, ip) {
                (HostPattern::Ip(a), Some(b)) => (*a == b).then_some(true),
                (HostPattern::Ip(_), None) => None,
                (HostPattern::Exact(e), None) => (*e == h).then_some(true),
                (HostPattern::Suffix(s), None) => (h.len() > s.len()
                    && h.ends_with(s.as_str())
                    && h.as_bytes()[h.len() - s.len() - 1] == b'.')
                    .then_some(false),
                (HostPattern::Any, _) => Some(false),
                (_, Some(_)) => None,
            };
            match m {
                Some(true) => return Some(true),
                Some(false) => found = Some(false),
                None => {}
            }
        }
        found
    }

    /// Checks a URL before a request: scheme, host and port, and the address of an IP host.
    pub fn check_url(&self, u: &url::Url) -> Result<(), EgressError> {
        if !matches!(u.scheme(), "http" | "https") {
            return Err(EgressError::Scheme);
        }
        let host = match u.host() {
            Some(url::Host::Domain(d)) => d.to_string(),
            Some(url::Host::Ipv4(v)) => v.to_string(),
            Some(url::Host::Ipv6(v)) => v.to_string(),
            None => return Err(EgressError::NotAllowed("(no host)".into())),
        };
        let explicit = self
            .allows(&host, u.port_or_known_default())
            .ok_or_else(|| EgressError::NotAllowed(host.clone()))?;
        if let Ok(ip) = host.parse::<IpAddr>()
            && !explicit
            && is_internal(ip)
        {
            return Err(EgressError::Internal(host));
        }
        Ok(())
    }

    /// Resolves an allowed host and returns only addresses it may connect to (`port` is
    /// checked when given and used in the addresses).
    pub async fn resolve(
        &self,
        host: &str,
        port: Option<u16>,
    ) -> Result<Vec<SocketAddr>, EgressError> {
        let explicit = self
            .allows(host, port)
            .ok_or_else(|| EgressError::NotAllowed(host.to_string()))?;
        let port = port.unwrap_or(0);
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
        if !explicit && addrs.iter().any(|a| is_internal(a.ip())) {
            return Err(EgressError::Internal(host.to_string()));
        }
        Ok(addrs)
    }
}

struct PolicyResolver(Arc<EgressPolicy>);

impl Resolve for PolicyResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let policy = self.0.clone();
        let host = name.as_str().to_string();
        Box::pin(async move {
            let addrs = policy.resolve(&host, None).await?;
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

/// The egress-checked HTTP client.
#[derive(Clone)]
pub struct Egress {
    policy: Arc<EgressPolicy>,
    http: reqwest::Client,
}

impl fmt::Debug for Egress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Egress")
            .field("policy", &self.policy)
            .finish_non_exhaustive()
    }
}

impl Egress {
    pub fn new(policy: Arc<EgressPolicy>) -> anyhow::Result<Self> {
        fastproof_blob::install_crypto_provider();
        let p = policy.clone();
        let redirect = reqwest::redirect::Policy::custom(move |attempt| {
            if attempt.previous().len() >= 3 {
                return attempt.error(EgressError::NotAllowed("(too many redirects)".into()));
            }
            match p.check_url(attempt.url()) {
                Ok(()) => attempt.follow(),
                Err(e) => attempt.error(e),
            }
        });
        let http = reqwest::Client::builder()
            .user_agent(concat!("FastProof/", env!("CARGO_PKG_VERSION")))
            .no_proxy()
            .redirect(redirect)
            .dns_resolver(Arc::new(PolicyResolver(policy.clone())))
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(20))
            .build()?;
        Ok(Self { policy, http })
    }

    pub fn policy(&self) -> &EgressPolicy {
        &self.policy
    }

    /// A request to `url` after checking it against the policy.
    pub fn request(&self, method: Method, url: &str) -> Result<RequestBuilder, OutboundError> {
        let u = url::Url::parse(url).map_err(|_| OutboundError::Network("invalid URL".into()))?;
        self.policy.check_url(&u).map_err(OutboundError::Denied)?;
        Ok(self.http.request(method, u))
    }

    /// Sends and reads the response body up to `max` bytes.
    pub async fn send(&self, rb: RequestBuilder, max: usize) -> Result<OutResponse, OutboundError> {
        let mut resp = rb.send().await.map_err(classify)?;
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
        assert_eq!(p.allows("hooks.example.com", Some(443)), Some(true));
        assert_eq!(p.allows("HOOKS.example.com.", None), Some(true));
        assert_eq!(p.allows("a.example.org", Some(443)), Some(false));
        assert_eq!(p.allows("example.org", Some(443)), None);
        assert_eq!(p.allows("evil-example.org", Some(443)), None);
        assert_eq!(p.allows("127.0.0.1", Some(8080)), Some(true));
        assert_eq!(p.allows("127.0.0.1", Some(8081)), None);
        assert_eq!(p.allows("gitlab.example.net", Some(80)), None);
        assert_eq!(p.allows("other.example.com", Some(443)), None);
        assert!(EgressPolicy::parse("").unwrap().is_empty());
        assert_eq!(
            EgressPolicy::parse("").unwrap().allows("example.com", None),
            None
        );
        assert!(EgressPolicy::parse("a*b.example.com").is_err());
        assert!(EgressPolicy::parse("host:notaport").is_err());
        let any = EgressPolicy::parse("*").unwrap();
        assert_eq!(any.allows("example.com", Some(443)), Some(false));
        let u = |s: &str| url::Url::parse(s).unwrap();
        assert!(any.check_url(&u("https://198.51.100.4/x")).is_ok());
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
        assert!(p.check_url(&u("http://127.0.0.1:8080/hook")).is_ok());
    }

    #[tokio::test]
    async fn wildcard_names_must_resolve_publicly() {
        let p = EgressPolicy::parse("*.localhost, localhost").unwrap();
        assert!(p.resolve("localhost", Some(80)).await.is_ok());
        assert!(matches!(
            p.resolve("foo.localhost", Some(80)).await,
            Err(EgressError::Internal(_)) | Err(EgressError::Resolve(_))
        ));
        assert!(matches!(
            p.resolve("example.com", Some(80)).await,
            Err(EgressError::NotAllowed(_))
        ));
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
