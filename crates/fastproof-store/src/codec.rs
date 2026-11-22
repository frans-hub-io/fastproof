//! Step trees, logs and fixtures are stored as zstd-compressed JSON (`bytea`).

use std::io::Read;

use serde::Serialize;
use serde::de::DeserializeOwned;

/// Compression level: fast, still several times smaller than JSON.
const LEVEL: i32 = 3;
/// Largest decompressed document accepted when reading back.
pub const MAX_DECODED: u64 = 512 * 1024 * 1024;

thread_local! {
    /// A compression context per thread: creating one for every document costs more than
    /// compressing a small one.
    static COMPRESSOR: std::cell::RefCell<Option<zstd::bulk::Compressor<'static>>> =
        const { std::cell::RefCell::new(None) };
}

/// JSON then zstd.
pub fn encode<T: Serialize>(v: &T) -> anyhow::Result<Vec<u8>> {
    let json = serde_json::to_vec(v)?;
    COMPRESSOR.with(|c| {
        let mut c = c.borrow_mut();
        if c.is_none() {
            *c = Some(zstd::bulk::Compressor::new(LEVEL)?);
        }
        let z = c.as_mut().expect("set above").compress(&json)?;
        Ok(z)
    })
}

/// zstd then JSON, bounded by [`MAX_DECODED`].
pub fn decode<T: DeserializeOwned>(bytes: &[u8]) -> anyhow::Result<T> {
    let mut out = Vec::new();
    let dec = zstd::stream::read::Decoder::new(bytes)?;
    let n = dec.take(MAX_DECODED + 1).read_to_end(&mut out)?;
    if n as u64 > MAX_DECODED {
        anyhow::bail!("stored document exceeds {MAX_DECODED} bytes");
    }
    Ok(serde_json::from_slice(&out)?)
}

#[cfg(test)]
mod tests {
    #[test]
    fn round_trip() {
        let v = serde_json::json!({"steps": vec![serde_json::json!({"name": "a"}); 50]});
        let z = super::encode(&v).unwrap();
        assert!(z.len() < serde_json::to_vec(&v).unwrap().len());
        let back: serde_json::Value = super::decode(&z).unwrap();
        assert_eq!(back, v);
        assert!(super::decode::<serde_json::Value>(b"not zstd").is_err());
    }
}
