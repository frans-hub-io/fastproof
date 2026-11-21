//! Lenient access to JSON values and a pre-parse scan that enforces depth and string limits.

use serde_json::Value;

use crate::error::IngestError;
use crate::limits::Limits;

/// Checks nesting depth and string lengths before the document is parsed.
pub(crate) fn scan(bytes: &[u8], limits: &Limits, file: &str) -> Result<(), IngestError> {
    let mut depth: u32 = 0;
    let mut in_string = false;
    let mut escape = false;
    let mut string_start = 0usize;
    for (i, &b) in bytes.iter().enumerate() {
        if in_string {
            if escape {
                escape = false;
            } else if b == b'\\' {
                escape = true;
            } else if b == b'"' {
                in_string = false;
                if (i - string_start) as u64 > limits.max_string_bytes {
                    return Err(IngestError::limit_at(
                        "max_string_bytes",
                        limits.max_string_bytes,
                        file.to_string(),
                    ));
                }
            }
            continue;
        }
        match b {
            b'"' => {
                in_string = true;
                string_start = i + 1;
            }
            b'{' | b'[' => {
                depth += 1;
                if depth > limits.max_json_depth {
                    return Err(IngestError::limit_at(
                        "max_json_depth",
                        limits.max_json_depth as u64,
                        file.to_string(),
                    ));
                }
            }
            b'}' | b']' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    if in_string && (bytes.len() - string_start) as u64 > limits.max_string_bytes {
        return Err(IngestError::limit_at(
            "max_string_bytes",
            limits.max_string_bytes,
            file.to_string(),
        ));
    }
    Ok(())
}

/// Parses a document after the limit scan. `Ok(None)` means malformed JSON (a warning).
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
}
