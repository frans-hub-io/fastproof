//! Lenient access to JSON values and a pre-parse scan that enforces depth, value-count and
//! string limits.

use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::error::IngestError;
use crate::limits::Limits;

/// Bounds checked by [`scan_json`] before a JSON document is parsed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JsonBounds {
    /// Nesting depth of arrays and objects.
    pub max_depth: u32,
    /// Values in the document (see [`Limits::max_json_values`]).
    pub max_values: u64,
    /// Length of one string (raw bytes between the quotes, escapes included).
    pub max_string_bytes: u64,
}

impl From<&Limits> for JsonBounds {
    fn from(l: &Limits) -> JsonBounds {
        JsonBounds {
            max_depth: l.max_json_depth,
            max_values: l.max_json_values,
            max_string_bytes: l.max_string_bytes,
        }
    }
}

/// Error of [`parse_json_bounded`].
#[derive(Debug, thiserror::Error)]
pub enum JsonError {
    /// A bound was exceeded; [`IngestError::limit_name`] names the [`Limits`] field
    /// (`max_json_depth`, `max_json_values` or `max_string_bytes`).
    #[error(transparent)]
    Limit(IngestError),
    /// The document is not valid JSON or does not match the target type.
    #[error("invalid JSON: {0}")]
    Invalid(String),
}

impl JsonError {
    /// Name of the limit that was hit, if this is a limit error.
    pub fn limit_name(&self) -> Option<&'static str> {
        match self {
            JsonError::Limit(e) => e.limit_name(),
            JsonError::Invalid(_) => None,
        }
    }
}

/// Checks nesting depth, the number of values and string lengths of a JSON document without
/// building it, in one pass and constant memory. Returns the number of values (arrays,
/// objects and scalars; an object member counts once), so callers can also bound the sum
/// over several documents, e.g. NDJSON lines. `context` is added to limit errors.
///
/// The scan is lenient: it does not validate the syntax, it only bounds what a parser could
/// build from the bytes.
pub fn scan_json(bytes: &[u8], bounds: &JsonBounds, context: &str) -> Result<u64, IngestError> {
    let limit = |name: &'static str, max: u64| IngestError::limit_at(name, max, context);
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    let mut depth: u32 = 0;
    let mut in_string = false;
    let mut escape = false;
    let mut string_start = 0usize;
    let mut values: u64 = 0;
    // Right after `[` or `{`: the next non-blank byte starts the first element unless it
    // closes the container. Every further element follows a comma. Top-level values (one,
    // or several in NDJSON) are counted where they start.
    let mut after_open = false;
    let mut in_scalar = false;
    for (i, &b) in bytes.iter().enumerate() {
        if in_string {
            if escape {
                escape = false;
            } else if b == b'\\' {
                escape = true;
            } else if b == b'"' {
                in_string = false;
                if (i - string_start) as u64 > bounds.max_string_bytes {
                    return Err(limit("max_string_bytes", bounds.max_string_bytes));
                }
            }
            continue;
        }
        if matches!(b, b' ' | b'\t' | b'\n' | b'\r') {
            in_scalar = false;
            continue;
        }
        if after_open {
            after_open = false;
            if b != b']' && b != b'}' {
                values += 1;
            }
        }
        match b {
            b'"' => {
                if depth == 0 {
                    values += 1;
                }
                in_string = true;
                string_start = i + 1;
                in_scalar = false;
            }
            b'{' | b'[' => {
                if depth == 0 {
                    values += 1;
                }
                depth += 1;
                if depth > bounds.max_depth {
                    return Err(limit("max_json_depth", bounds.max_depth as u64));
                }
                after_open = true;
                in_scalar = false;
            }
            b',' => {
                if depth > 0 {
                    values += 1;
                }
                in_scalar = false;
            }
            b'}' | b']' | b':' => {
                if b != b':' {
                    depth = depth.saturating_sub(1);
                }
                in_scalar = false;
            }
            _ => {
                if depth == 0 && !in_scalar {
                    values += 1;
                }
                in_scalar = true;
            }
        }
        if values > bounds.max_values {
            return Err(limit("max_json_values", bounds.max_values));
        }
    }
    if in_string && (bytes.len() - string_start) as u64 > bounds.max_string_bytes {
        return Err(limit("max_string_bytes", bounds.max_string_bytes));
    }
    Ok(values)
}

/// [`scan_json`] followed by deserialization into `T` (`serde_json::Value` or a typed
/// structure). A UTF-8 byte order mark is accepted.
pub fn parse_json_bounded<T: DeserializeOwned>(
    bytes: &[u8],
    bounds: &JsonBounds,
    context: &str,
) -> Result<T, JsonError> {
    scan_json(bytes, bounds, context).map_err(JsonError::Limit)?;
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    serde_json::from_slice(bytes).map_err(|e| JsonError::Invalid(e.to_string()))
}

/// [`scan_json`] with the bounds of `limits`.
pub(crate) fn scan(bytes: &[u8], limits: &Limits, file: &str) -> Result<u64, IngestError> {
    scan_json(bytes, &JsonBounds::from(limits), file)
}

/// Parses a document after the limit scan. `Ok(Err(_))` means malformed JSON (a warning).
pub(crate) fn parse(
    bytes: &[u8],
    limits: &Limits,
    file: &str,
) -> Result<Result<Value, String>, IngestError> {
    scan(bytes, limits, file)?;
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    Ok(serde_json::from_slice(bytes).map_err(|e| e.to_string()))
}

pub(crate) fn clean(s: &str) -> String {
    if s.contains('\0') {
        s.replace('\0', "\u{FFFD}")
    } else {
        s.to_string()
    }
}

/// String, number or bool as a string; empty strings are kept.
pub(crate) fn as_string(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(clean(s)),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

pub(crate) fn str_field(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(as_string)
}

/// Non-empty string field.
pub(crate) fn text_field(v: &Value, key: &str) -> Option<String> {
    str_field(v, key).filter(|s| !s.is_empty())
}

pub(crate) fn i64_field(v: &Value, key: &str) -> Option<i64> {
    match v.get(key)? {
        Value::Number(n) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)),
        Value::String(s) => s.trim().parse::<f64>().ok().map(|f| f as i64),
        _ => None,
    }
}

pub(crate) fn f64_field(v: &Value, key: &str) -> Option<f64> {
    match v.get(key)? {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse::<f64>().ok(),
        _ => None,
    }
}

pub(crate) fn bool_field(v: &Value, key: &str) -> bool {
    match v.get(key) {
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => s.eq_ignore_ascii_case("true"),
        _ => false,
    }
}

pub(crate) fn arr<'a>(v: &'a Value, key: &str) -> &'a [Value] {
    match v.get(key) {
        Some(Value::Array(a)) => a.as_slice(),
        _ => &[],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scan_limits() {
        let limits = Limits {
            max_json_depth: 3,
            max_string_bytes: 5,
            ..Limits::default()
        };
        assert!(scan(br#"{"a":[{"b":"12345"}]}"#, &limits, "f").is_ok());
        let e = scan(br#"{"a":[{"b":[1]}]}"#, &limits, "f").unwrap_err();
        assert_eq!(e.limit_name(), Some("max_json_depth"));
        let e = scan(br#"{"a":"123456"}"#, &limits, "f").unwrap_err();
        assert_eq!(e.limit_name(), Some("max_string_bytes"));
        assert!(scan(br#"{"a":"[[\"["}"#, &limits, "f").is_ok());
    }

    #[test]
    fn counts_values() {
        let b = JsonBounds {
            max_depth: 16,
            max_values: 1000,
            max_string_bytes: 1000,
        };
        let n = |s: &str| scan_json(s.as_bytes(), &b, "f").unwrap();
        assert_eq!(n(""), 0);
        assert_eq!(n("  "), 0);
        assert_eq!(n("5"), 1);
        assert_eq!(n("\"a,b\""), 1);
        assert_eq!(n("[]"), 1);
        assert_eq!(n("{}"), 1);
        assert_eq!(n("[1, 2, 3]"), 4);
        assert_eq!(n(r#"{"a": 1, "b": [true, null], "c": {}}"#), 6);
        assert_eq!(n("[[1],[ ],{\"k\":\"v,]\"}]"), 6);
        assert_eq!(n("{\"a\":1}\n{\"b\":2}\n7 8"), 6);
        let small = JsonBounds { max_values: 3, ..b };
        let e = scan_json(b"[1,2,3]", &small, "f").unwrap_err();
        assert_eq!(e.limit_name(), Some("max_json_values"));
        let e = parse_json_bounded::<Value>(b"[0,0,0,0]", &small, "f").unwrap_err();
        assert_eq!(e.limit_name(), Some("max_json_values"));
        assert!(matches!(
            parse_json_bounded::<Value>(b"[0,", &b, "f"),
            Err(JsonError::Invalid(_))
        ));
        let v: Vec<u8> = parse_json_bounded(b"\xEF\xBB\xBF[1,2]", &small, "f").unwrap();
        assert_eq!(v, vec![1, 2]);
    }
}
