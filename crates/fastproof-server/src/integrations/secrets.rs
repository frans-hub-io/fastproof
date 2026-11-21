//! Project secrets encrypted at rest with AES-256-GCM (ring). The key file holds one key per
//! line (64 hex characters or base64 of 32 bytes): the first line encrypts, every line
//! decrypts, so a key is rotated by putting the new key first and keeping the old one below it
//! until the server has re-encrypted every secret (it does so at start-up).
//!
//! Every secret is bound when it is written to a purpose and, optionally, to the one origin it
//! may be sent to (both are part of the authenticated data of the encryption). A secret is
//! refused for any other purpose or destination, so changing a URL in the settings cannot send
//! an existing token somewhere else: the secret has to be written again for the new origin.

use std::fmt;
use std::path::Path;

use anyhow::{Context, anyhow, bail};
use base64::Engine as _;
use fastproof_blob::Secret;
use fastproof_store::integrations::SealedSecret;
use ring::aead::{AES_256_GCM, Aad, LessSafeKey, NONCE_LEN, Nonce, UnboundKey};
use ring::rand::{SecureRandom, SystemRandom};
use sha2::{Digest, Sha256};
use uuid::Uuid;

/// Largest secret value in bytes.
pub const MAX_SECRET_BYTES: usize = 8192;

/// What a secret may be used for.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, utoipa::ToSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum SecretPurpose {
    /// GitLab or GitHub API token (`vcs.tokenSecretRef`); needs an origin.
    Vcs,
    /// FastMock admin token (`fastmock.tokenSecretRef`); needs an origin.
    Fastmock,
    /// Slack incoming webhook URL (`targetSecretRef` of a Slack notification).
    Slack,
    /// Telegram bot token (`secretRef` of a Telegram notification).
    Telegram,
    /// Webhook signing key (`secretRef`) or webhook URL (`targetSecretRef`).
    Webhook,
    /// SMTP credentials (reserved; the server-wide SMTP settings do not use project secrets).
    Smtp,
}

impl SecretPurpose {
    pub fn as_str(self) -> &'static str {
        match self {
            SecretPurpose::Vcs => "vcs",
            SecretPurpose::Fastmock => "fastmock",
            SecretPurpose::Slack => "slack",
            SecretPurpose::Telegram => "telegram",
            SecretPurpose::Webhook => "webhook",
            SecretPurpose::Smtp => "smtp",
        }
    }

    /// Whether a secret of this purpose must name the origin it is sent to.
    pub fn needs_origin(self) -> bool {
        matches!(self, SecretPurpose::Vcs | SecretPurpose::Fastmock)
    }
}

/// The origin (`scheme://host[:port]`, lowercase, default port left out) of an http(s) URL.
pub fn origin_of(u: &str) -> Option<String> {
    let p = url::Url::parse(u.trim()).ok()?;
    if !matches!(p.scheme(), "http" | "https") || p.host_str().is_none() {
        return None;
    }
    Some(p.origin().ascii_serialization())
}

/// Normalizes an origin given when a secret is written: an http(s) URL without user info,
/// path, query or fragment.
pub fn normalize_origin(o: &str) -> Result<String, String> {
    let err = || "origin must be an http(s) origin such as https://gitlab.example.com".to_string();
    if o.len() > 2048 {
        return Err(err());
    }
    let p = url::Url::parse(o.trim()).map_err(|_| err())?;
    if !matches!(p.scheme(), "http" | "https")
        || p.host_str().is_none()
        || !p.username().is_empty()
        || p.password().is_some()
        || !matches!(p.path(), "" | "/")
        || p.query().is_some()
        || p.fragment().is_some()
    {
        return Err(err());
    }
    Ok(p.origin().ascii_serialization())
}

struct Key {
    id: String,
    key: LessSafeKey,
}

/// The keys of `FASTPROOF_SECRETS_KEY_FILE`.
pub struct SecretKeys {
    keys: Vec<Key>,
    rng: SystemRandom,
}

impl fmt::Debug for SecretKeys {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SecretKeys")
            .field("active", &self.active_id())
            .field("count", &self.keys.len())
            .finish()
    }
}

fn decode_key(line: &str) -> anyhow::Result<Vec<u8>> {
    let l = line.trim();
    if l.len() == 64 && l.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Ok(hex::decode(l)?);
    }
    let b = base64::engine::general_purpose::STANDARD
        .decode(l)
        .map_err(|_| anyhow!("a key is neither 64 hex characters nor base64"))?;
    Ok(b)
}

fn aad(project_id: Uuid, name: &str, purpose: Option<&str>, origin: Option<&str>) -> Vec<u8> {
    let mut a = b"fastproof-secret\0".to_vec();
    a.extend_from_slice(project_id.as_bytes());
    a.extend_from_slice(name.as_bytes());
    if let Some(p) = purpose {
        a.extend_from_slice(b"\0purpose=");
        a.extend_from_slice(p.as_bytes());
        a.extend_from_slice(b"\0origin=");
        a.extend_from_slice(origin.unwrap_or_default().as_bytes());
    }
    a
}

impl SecretKeys {
    /// Reads the key file.
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        Self::parse(&text)
    }

    /// Parses keys, one per line (blank lines and `#` lines are skipped).
    pub fn parse(text: &str) -> anyhow::Result<Self> {
        let mut keys = Vec::new();
        for line in text.lines() {
            let l = line.trim();
            if l.is_empty() || l.starts_with('#') {
                continue;
            }
            let raw = decode_key(l)?;
            if raw.len() != 32 {
                bail!("a key must have 32 bytes, got {}", raw.len());
            }
            let id = hex::encode(&Sha256::digest(&raw)[..8]);
            let key = LessSafeKey::new(
                UnboundKey::new(&AES_256_GCM, &raw).map_err(|_| anyhow!("invalid key"))?,
            );
            if keys.iter().any(|k: &Key| k.id == id) {
                continue;
            }
            keys.push(Key { id, key });
        }
        if keys.is_empty() {
            bail!("no key in the file");
        }
        Ok(Self {
            keys,
            rng: SystemRandom::new(),
        })
    }

    /// Id of the key that encrypts (a hash prefix, not the key).
    pub fn active_id(&self) -> &str {
        &self.keys[0].id
    }

    /// Encrypts a value with the active key, bound to the project, the name, the purpose and
    /// the origin.
    pub fn seal(
        &self,
        project_id: Uuid,
        name: &str,
        purpose: Option<&str>,
        origin: Option<&str>,
        value: &str,
    ) -> anyhow::Result<SealedSecret> {
        let k = &self.keys[0];
        let mut nonce = [0u8; NONCE_LEN];
        self.rng
            .fill(&mut nonce)
            .map_err(|_| anyhow!("no randomness"))?;
        let mut buf = value.as_bytes().to_vec();
        k.key
            .seal_in_place_append_tag(
                Nonce::assume_unique_for_key(nonce),
                Aad::from(aad(project_id, name, purpose, origin)),
                &mut buf,
            )
            .map_err(|_| anyhow!("encryption failed"))?;
        Ok(SealedSecret {
            key_id: k.id.clone(),
            nonce: nonce.to_vec(),
            ciphertext: buf,
            purpose: purpose.map(str::to_string),
            origin: origin.map(str::to_string),
        })
    }

    /// Decrypts a value; fails when its key is not in the file or the data was altered.
    pub fn open(
        &self,
        project_id: Uuid,
        name: &str,
        sealed: &SealedSecret,
    ) -> anyhow::Result<Secret> {
        let k = self
            .keys
            .iter()
            .find(|k| k.id == sealed.key_id)
            .ok_or_else(|| anyhow!("the secret {name:?} was encrypted with a key that is not in FASTPROOF_SECRETS_KEY_FILE"))?;
        let nonce: [u8; NONCE_LEN] = sealed
            .nonce
            .as_slice()
            .try_into()
            .map_err(|_| anyhow!("the secret {name:?} is damaged"))?;
        let mut buf = sealed.ciphertext.clone();
        let plain = k
            .key
            .open_in_place(
                Nonce::assume_unique_for_key(nonce),
                Aad::from(aad(
                    project_id,
                    name,
                    sealed.purpose.as_deref(),
                    sealed.origin.as_deref(),
                )),
                &mut buf,
            )
            .map_err(|_| anyhow!("the secret {name:?} cannot be decrypted"))?;
        let s = String::from_utf8(plain.to_vec())
            .map_err(|_| anyhow!("the secret {name:?} is damaged"))?;
        Ok(Secret::new(s))
    }
}

/// `^[A-Za-z0-9][A-Za-z0-9_.-]{0,63}$`
pub fn valid_name(n: &str) -> bool {
    !n.is_empty()
        && n.len() <= 64
        && n.as_bytes()[0].is_ascii_alphanumeric()
        && n.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
}

#[cfg(test)]
mod tests {
    use super::*;

    const K1: &str = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";

    #[test]
    fn seal_open_bound_to_project_and_name() {
        let keys = SecretKeys::parse(K1).unwrap();
        let p = Uuid::now_v7();
        let s = keys
            .seal(
                p,
                "slack",
                Some("slack"),
                None,
                "https://hooks.example.com/x",
            )
            .unwrap();
        assert!(!s.ciphertext.windows(5).any(|w| w == b"hooks"));
        assert_eq!(
            keys.open(p, "slack", &s).unwrap().expose(),
            "https://hooks.example.com/x"
        );
        assert!(keys.open(p, "other", &s).is_err());
        assert!(keys.open(Uuid::now_v7(), "slack", &s).is_err());
        let mut bad = s.clone();
        bad.ciphertext[0] ^= 1;
        assert!(keys.open(p, "slack", &bad).is_err());
        let mut rebound = s.clone();
        rebound.purpose = Some("vcs".into());
        assert!(
            keys.open(p, "slack", &rebound).is_err(),
            "the purpose is authenticated"
        );
        let vcs = keys
            .seal(
                p,
                "gl",
                Some("vcs"),
                Some("https://gitlab.example.com"),
                "t",
            )
            .unwrap();
        let mut moved = vcs.clone();
        moved.origin = Some("https://evil.example.net".into());
        assert!(
            keys.open(p, "gl", &moved).is_err(),
            "the origin is authenticated"
        );
        assert_eq!(keys.open(p, "gl", &vcs).unwrap().expose(), "t");
        assert!(!format!("{keys:?}").contains("0001020304"));
    }

    #[test]
    fn rotation_keeps_old_keys_readable() {
        let old = SecretKeys::parse(K1).unwrap();
        let p = Uuid::now_v7();
        let s = old.seal(p, "t", Some("webhook"), None, "v").unwrap();
        let k2 = base64::engine::general_purpose::STANDARD.encode([7u8; 32]);
        let both = SecretKeys::parse(&format!("{k2}\n{K1}\n")).unwrap();
        assert_ne!(both.active_id(), old.active_id());
        assert_eq!(both.open(p, "t", &s).unwrap().expose(), "v");
        let resealed = both.seal(p, "t", Some("webhook"), None, "v").unwrap();
        assert_eq!(resealed.key_id, both.active_id());
        let only_new = SecretKeys::parse(&k2).unwrap();
        assert!(only_new.open(p, "t", &s).is_err());
        assert!(SecretKeys::parse("short").is_err());
        assert!(SecretKeys::parse("\n# nothing\n").is_err());
    }

    #[test]
    fn names() {
        assert!(valid_name("slack-webhook_1.prod"));
        assert!(!valid_name(""));
        assert!(!valid_name("-x"));
        assert!(!valid_name("a/b"));
        assert!(!valid_name(&"a".repeat(65)));
    }

    #[test]
    fn origins() {
        assert_eq!(
            normalize_origin("https://GitLab.example.com/").unwrap(),
            "https://gitlab.example.com"
        );
        assert_eq!(
            normalize_origin("https://gitlab.example.com:443").unwrap(),
            "https://gitlab.example.com"
        );
        assert_eq!(
            normalize_origin("http://fastmock.example.com:9090").unwrap(),
            "http://fastmock.example.com:9090"
        );
        for bad in [
            "gitlab.example.com",
            "ftp://example.com",
            "https://example.com/api",
            "https://u:p@example.com",
            "https://example.com/?x=1",
        ] {
            assert!(normalize_origin(bad).is_err(), "{bad}");
        }
        assert_eq!(
            origin_of("https://hooks.example.com/services/x").as_deref(),
            Some("https://hooks.example.com")
        );
        assert_eq!(origin_of("not a url"), None);
    }
}
