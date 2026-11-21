//! Text helpers: bounded logs, HTML sanitizing, base64, timestamps, message parsing.

use std::sync::OnceLock;

use fastproof_model::BoundedText;
use regex::Regex;

/// Keeps the head and the tail of long text, cut on char boundaries.
pub(crate) fn bound(text: &str, max: u64) -> BoundedText {
    let original = text.len() as u64;
    if original <= max {
        return BoundedText {
            text: text.to_string(),
            original_bytes: original,
            truncated: false,
        };
    }
    let half = (max / 2) as usize;
    let mut head_end = half.min(text.len());
    while !text.is_char_boundary(head_end) {
        head_end -= 1;
    }
    let mut tail_start = text.len().saturating_sub(half);
    while !text.is_char_boundary(tail_start) {
        tail_start += 1;
    }
    let skipped = tail_start - head_end;
    BoundedText {
        text: format!(
            "{}\n... [{skipped} bytes truncated] ...\n{}",
            &text[..head_end],
            &text[tail_start..]
        ),
        original_bytes: original,
        truncated: true,
    }
}

/// Bounded text, `None` for empty input.
pub(crate) fn bound_opt(text: &str, max: u64) -> Option<BoundedText> {
    if text.is_empty() {
        None
    } else {
        Some(bound(text, max))
    }
}

/// Truncates a message or trace to the log limit.
pub(crate) fn cap(text: String, max: u64) -> String {
    if text.len() as u64 <= max {
        text
    } else {
        bound(&text, max).text
    }
}

fn sanitizer() -> &'static ammonia::Builder<'static> {
    static B: OnceLock<ammonia::Builder<'static>> = OnceLock::new();
    B.get_or_init(|| {
        let mut b = ammonia::Builder::default();
        b.link_rel(Some("noopener noreferrer nofollow"));
        b
    })
}

/// Sanitizes untrusted HTML (descriptionHtml): no scripts, styles, SVG or event handlers.
pub fn sanitize_html(html: &str) -> String {
    sanitizer().clean(html).to_string()
}

/// Decodes standard or URL-safe base64, ignoring whitespace. `None` on invalid input.
pub(crate) fn base64_decode(input: &str) -> Option<Vec<u8>> {
    fn val(c: u8) -> Option<u32> {
        match c {
            b'A'..=b'Z' => Some((c - b'A') as u32),
            b'a'..=b'z' => Some((c - b'a' + 26) as u32),
            b'0'..=b'9' => Some((c - b'0' + 52) as u32),
            b'+' | b'-' => Some(62),
            b'/' | b'_' => Some(63),
            _ => None,
        }
    }
    let mut out = Vec::with_capacity(input.len() / 4 * 3);
    let mut acc: u32 = 0;
    let mut bits = 0u32;
    for &c in input.as_bytes() {
        if c.is_ascii_whitespace() {
            continue;
        }
        if c == b'=' {
            break;
        }
        acc = (acc << 6) | val(c)?;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

/// Parses RFC 3339 or a timezone-less ISO timestamp (taken as UTC) to epoch milliseconds.
pub(crate) fn parse_timestamp_ms(value: &str) -> Option<i64> {
    use time::format_description::well_known::Rfc3339;
    use time::{OffsetDateTime, PrimitiveDateTime, macros::format_description};
    let v = value.trim();
    if v.is_empty() {
        return None;
    }
    if let Ok(t) = OffsetDateTime::parse(v, &Rfc3339) {
        return Some((t.unix_timestamp_nanos() / 1_000_000) as i64);
    }
    let base = v.split(['+']).next().unwrap_or(v);
    let (main, frac) = match base.split_once('.') {
        Some((m, f)) => (m, f),
        None => (base, ""),
    };
    let fmt = format_description!("[year]-[month]-[day]T[hour]:[minute]:[second]");
    let fmt_space = format_description!("[year]-[month]-[day] [hour]:[minute]:[second]");
    let dt = PrimitiveDateTime::parse(main, &fmt)
        .or_else(|_| PrimitiveDateTime::parse(main, &fmt_space))
        .ok()?;
    let mut ms = (dt.assume_utc().unix_timestamp_nanos() / 1_000_000) as i64;
    let digits: String = frac
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .take(3)
        .collect();
    if !digits.is_empty() {
        let scale = 10i64.pow(3 - digits.len() as u32);
        ms += digits.parse::<i64>().unwrap_or(0) * scale;
    }
    Some(ms)
}

/// Seconds (as written in JUnit `time`) to milliseconds.
pub(crate) fn seconds_to_ms(value: &str) -> Option<i64> {
    let v = value.trim().replace(',', "");
    let f: f64 = v.parse().ok()?;
    if f.is_finite() && f >= 0.0 {
        Some((f * 1000.0).round() as i64)
    } else {
        None
    }
}

/// Extracts expected/actual values from common assertion messages.
pub(crate) fn expected_actual(message: &str) -> Option<(String, String)> {
    static RES: OnceLock<Vec<(Regex, bool)>> = OnceLock::new();
    let res = RES.get_or_init(|| {
        vec![
            (
                Regex::new(r"(?s)expected:\s*<(.*?)>\s*but was:\s*<(.*?)>").expect("regex"),
                true,
            ),
            (
                Regex::new(r"(?m)^\s*Expected:\s*(.+?)\s*\n\s*Received:\s*(.+?)\s*$").expect("regex"),
                true,
            ),
            (
                Regex::new(r"^(?:AssertionError: )?expected (.+?) to (?:be|equal|deeply equal|strictly equal) (.+?)(?: // .*)?$")
                    .expect("regex"),
                false,
            ),
        ]
    });
    let first = message.lines().take(20).collect::<Vec<_>>().join("\n");
    for (re, expected_first) in res {
        if let Some(c) = re.captures(&first) {
            let a = c.get(1)?.as_str().to_string();
            let b = c.get(2)?.as_str().to_string();
            return Some(if *expected_first { (a, b) } else { (b, a) });
        }
    }
    None
}

/// Guesses a content type from a file name.
pub(crate) fn content_type_for(name: &str) -> Option<&'static str> {
    let lower = name.to_ascii_lowercase();
    let ext = lower.rsplit_once('.').map(|(_, e)| e)?;
    Some(match ext {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "webm" => "video/webm",
        "mp4" => "video/mp4",
        "txt" | "log" => "text/plain",
        "md" => "text/markdown",
        "json" => "application/json",
        "xml" => "application/xml",
        "html" | "htm" => "text/html",
        "csv" => "text/csv",
        "zip" => "application/zip",
        "httpexchange" => "application/vnd.allure.http+json",
        "imagediff" => "application/vnd.allure.image.diff",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounds_text() {
        let b = bound("abcdefghij", 4);
        assert!(b.truncated);
        assert!(b.text.starts_with("ab") && b.text.ends_with("ij"));
        assert_eq!(b.original_bytes, 10);
        let b = bound("ééééé", 3);
        assert!(b.truncated);
    }

    #[test]
    fn base64() {
        assert_eq!(base64_decode("aGVsbG8=").unwrap(), b"hello");
        assert_eq!(base64_decode("aGVs\nbG8").unwrap(), b"hello");
        assert!(base64_decode("@@").is_none());
    }

    #[test]
    fn timestamps() {
        assert_eq!(parse_timestamp_ms("1970-01-01T00:00:01Z"), Some(1000));
        assert_eq!(parse_timestamp_ms("1970-01-01T00:00:01.5Z"), Some(1500));
        assert_eq!(parse_timestamp_ms("1970-01-01T00:00:01.250"), Some(1250));
        assert_eq!(parse_timestamp_ms("1970-01-01T00:00:02"), Some(2000));
        assert_eq!(parse_timestamp_ms("bad"), None);
        assert_eq!(seconds_to_ms("0.061"), Some(61));
        assert_eq!(seconds_to_ms("1,234.5"), Some(1_234_500));
    }

    #[test]
    fn messages() {
        assert_eq!(
            expected_actual("expected: <OK> but was: <TIMEOUT>"),
            Some(("OK".into(), "TIMEOUT".into()))
        );
        assert_eq!(
            expected_actual("Error: expect(received).toBe(expected)\n\nExpected: 90\nReceived: 95"),
            Some(("90".into(), "95".into()))
        );
        assert_eq!(
            expected_actual("expected 95 to be 90 // Object.is equality"),
            Some(("90".into(), "95".into()))
        );
        assert_eq!(expected_actual("boom"), None);
    }

    #[test]
    fn html_is_sanitized() {
        let s = sanitize_html(
            "<p onclick=\"x()\">hi<script>alert(1)</script><svg><circle/></svg><a href=\"javascript:alert(1)\">l</a></p>",
        );
        assert!(!s.contains("script") && !s.contains("onclick") && !s.contains("svg"));
        assert!(!s.contains("javascript"));
        assert!(s.contains("<p>hi"));
    }
}
