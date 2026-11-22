//! HTTP byte ranges (RFC 9110 section 14) for the attachment content routes.

/// What a `Range` header asks of an object of known size.
#[derive(Debug, PartialEq, Eq)]
pub enum RangeOutcome {
    /// Serve the whole body with 200: no usable header, another unit, several ranges or bad syntax.
    Full,
    /// Serve `start..end` (end exclusive) with 206.
    Partial { start: u64, end: u64 },
    /// 416.
    Unsatisfiable,
}

/// Evaluates a `Range` header value against an object of `size` bytes. Only one range is
/// served; a list of ranges is answered with the whole body, which the RFC allows.
pub fn parse(value: &str, size: u64) -> RangeOutcome {
    let Some(spec) = value.trim().strip_prefix("bytes=") else {
        return RangeOutcome::Full;
    };
    if spec.contains(',') {
        return RangeOutcome::Full;
    }
    let Some((a, b)) = spec.trim().split_once('-') else {
        return RangeOutcome::Full;
    };
    let (a, b) = (a.trim(), b.trim());
    let num = |s: &str| -> Option<u64> {
        (!s.is_empty() && s.len() <= 19 && s.bytes().all(|c| c.is_ascii_digit()))
            .then(|| s.parse().ok())
            .flatten()
    };
    match (a.is_empty(), b.is_empty()) {
        (true, true) => RangeOutcome::Full,
        (true, false) => match num(b) {
            None => RangeOutcome::Full,
            Some(0) => RangeOutcome::Unsatisfiable,
            Some(_) if size == 0 => RangeOutcome::Unsatisfiable,
            Some(n) => RangeOutcome::Partial {
                start: size.saturating_sub(n),
                end: size,
            },
        },
        (false, true) => match num(a) {
            None => RangeOutcome::Full,
            Some(s) if s >= size => RangeOutcome::Unsatisfiable,
            Some(s) => RangeOutcome::Partial {
                start: s,
                end: size,
            },
        },
        (false, false) => match (num(a), num(b)) {
            (Some(s), Some(e)) if s <= e => {
                if s >= size {
                    RangeOutcome::Unsatisfiable
                } else {
                    RangeOutcome::Partial {
                        start: s,
                        end: e.saturating_add(1).min(size),
                    }
                }
            }
            _ => RangeOutcome::Full,
        },
    }
}

/// Whether an `If-None-Match` value matches the strong `etag` (weak comparison, `*` matches).
pub fn none_match(value: &str, etag: &str) -> bool {
    value
        .split(',')
        .map(str::trim)
        .any(|t| t == "*" || t.strip_prefix("W/").unwrap_or(t) == etag)
}

/// Whether an `If-Range` value allows the range: only the exact strong entity tag does (the
/// content has no modification date, so a date never matches).
pub fn if_range_ok(value: &str, etag: &str) -> bool {
    value.trim() == etag
}

#[cfg(test)]
mod tests {
    use super::*;
    use RangeOutcome::*;

    #[test]
    fn forms() {
        assert_eq!(parse("bytes=0-9", 100), Partial { start: 0, end: 10 });
        assert_eq!(
            parse("bytes=90-", 100),
            Partial {
                start: 90,
                end: 100
            }
        );
        assert_eq!(
            parse("bytes=-10", 100),
            Partial {
                start: 90,
                end: 100
            }
        );
        assert_eq!(parse("bytes=-500", 100), Partial { start: 0, end: 100 });
        assert_eq!(
            parse("bytes=50-5000", 100),
            Partial {
                start: 50,
                end: 100
            }
        );
        assert_eq!(
            parse("bytes=99-99", 100),
            Partial {
                start: 99,
                end: 100
            }
        );
    }

    #[test]
    fn unsatisfiable() {
        assert_eq!(parse("bytes=100-", 100), Unsatisfiable);
        assert_eq!(parse("bytes=100-200", 100), Unsatisfiable);
        assert_eq!(parse("bytes=-0", 100), Unsatisfiable);
        assert_eq!(parse("bytes=0-0", 0), Unsatisfiable);
        assert_eq!(parse("bytes=-5", 0), Unsatisfiable);
    }

    #[test]
    fn ignored() {
        assert_eq!(parse("bytes=0-1,5-9", 100), Full);
        assert_eq!(parse("items=0-1", 100), Full);
        assert_eq!(parse("bytes=9-3", 100), Full);
        assert_eq!(parse("bytes=a-3", 100), Full);
        assert_eq!(parse("bytes=-", 100), Full);
        assert_eq!(parse("bytes=1", 100), Full);
        assert_eq!(parse("bytes=0-99999999999999999999999", 100), Full);
        assert_eq!(parse("", 100), Full);
    }

    #[test]
    fn validators() {
        let e = "\"abc\"";
        assert!(none_match("\"abc\"", e));
        assert!(none_match("\"x\", W/\"abc\"", e));
        assert!(none_match("*", e));
        assert!(!none_match("\"abd\"", e));
        assert!(if_range_ok("\"abc\"", e));
        assert!(!if_range_ok("W/\"abc\"", e));
        assert!(!if_range_ok("Wed, 21 Oct 2015 07:28:00 GMT", e));
    }
}
