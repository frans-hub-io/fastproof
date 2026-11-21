//! Normalization of names and paths used for test identity.

use unicode_normalization::UnicodeNormalization;

/// Trims and converts to Unicode NFC.
pub fn normalize_text(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_ascii() {
        return trimmed.to_string();
    }
    trimmed.nfc().collect()
}

/// Normalizes a source path: posix separators, NFC, no `file://`, no `./` segments, no
/// duplicate slashes, and the workspace prefix stripped (the given prefixes first, then
/// the well-known CI workspace layouts).
pub fn normalize_path(path: &str, prefixes: &[&str]) -> String {
    let mut p = normalize_text(path).replace('\\', "/");
    if let Some(rest) = p.strip_prefix("file://") {
        p = rest.to_string();
    }
    let mut p = collapse(&p);
    for prefix in prefixes {
        let prefix = collapse(&normalize_text(prefix).replace('\\', "/"));
        let prefix = prefix.trim_end_matches('/');
        if prefix.is_empty() {
            continue;
        }
        if let Some(rest) = p.strip_prefix(prefix)
            && let Some(rest) = rest.strip_prefix('/')
        {
            return rest.to_string();
        }
    }
    if let Some(rest) = strip_ci_workspace(&p) {
        p = rest;
    }
    p
}

fn collapse(p: &str) -> String {
    let absolute = p.starts_with('/');
    let parts: Vec<&str> = p
        .split('/')
        .filter(|s| !s.is_empty() && *s != ".")
        .collect();
    let joined = parts.join("/");
    if absolute {
        format!("/{joined}")
    } else {
        joined
    }
}

fn skip_segments(p: &str, n: usize) -> Option<String> {
    let mut rest = p;
    for _ in 0..n {
        let (_, r) = rest.split_once('/')?;
        rest = r;
    }
    if rest.is_empty() {
        None
    } else {
        Some(rest.to_string())
    }
}

/// Strips well-known CI workspace and temp prefixes from an absolute path.
pub fn strip_ci_workspace(p: &str) -> Option<String> {
    if let Some(rest) = p.strip_prefix("/builds/") {
        return skip_segments(rest, 2);
    }
    if let Some(rest) = p.strip_prefix("/home/runner/work/") {
        return skip_segments(rest, 2);
    }
    if let Some(rest) = p.strip_prefix("/github/workspace/") {
        return Some(rest.to_string());
    }
    if let Some(idx) = p.find("/workspace/") {
        return skip_segments(&p[idx + "/workspace/".len()..], 1);
    }
    let bytes = p.as_bytes();
    if bytes.len() > 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && bytes[2] == b'/' {
        let rest = &p[3..];
        if let Some(r) = rest.strip_prefix("a/") {
            return skip_segments(r, 2);
        }
        return Some(rest.to_string());
    }
    for tmp in [
        "/tmp/",
        "/var/tmp/",
        "/private/tmp/",
        "/private/var/folders/",
    ] {
        if let Some(rest) = p.strip_prefix(tmp) {
            return skip_segments(rest, 1);
        }
    }
    None
}

/// Splits parameters out of a JUnit test name.
///
/// `name[params]` gives `("name", [("params", "params")])` and `name(sig)[n]` gives
/// `("name", [("signature", "sig"), ("index", "n")])`. Names without these shapes are
/// returned unchanged.
pub fn split_junit_name(name: &str) -> (String, Vec<(String, String)>) {
    let name = name.trim();
    if let Some(stripped) = name.strip_suffix(']')
        && let Some(open) = find_matching_open(stripped, '[', ']')
    {
        let inner = &stripped[open + 1..];
        let base = &stripped[..open];
        if !base.is_empty() {
            if let Some(base_stripped) = base.strip_suffix(')')
                && let Some(popen) = find_matching_open(base_stripped, '(', ')')
                && popen > 0
            {
                let sig = &base_stripped[popen + 1..];
                return (
                    base_stripped[..popen].to_string(),
                    vec![
                        ("signature".to_string(), sig.to_string()),
                        ("index".to_string(), inner.to_string()),
                    ],
                );
            }
            return (
                base.to_string(),
                vec![("params".to_string(), inner.to_string())],
            );
        }
    }
    (name.to_string(), Vec::new())
}

fn find_matching_open(s: &str, open: char, close: char) -> Option<usize> {
    let mut depth = 0usize;
    for (i, c) in s.char_indices().rev() {
        if c == close {
            depth += 1;
        } else if c == open {
            if depth == 0 {
                return Some(i);
            }
            depth -= 1;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_is_nfc_and_trimmed() {
        assert_eq!(normalize_text("  cafe\u{301} "), "caf\u{e9}");
    }

    #[test]
    fn paths() {
        assert_eq!(
            normalize_path("tests\\a\\b.spec.ts", &[]),
            "tests/a/b.spec.ts"
        );
        assert_eq!(normalize_path("./tests//a.ts", &[]), "tests/a.ts");
        assert_eq!(
            normalize_path("/builds/example/project/tests/a.py", &[]),
            "tests/a.py"
        );
        assert_eq!(
            normalize_path("/home/runner/work/repo/repo/src/x.ts", &[]),
            "src/x.ts"
        );
        assert_eq!(
            normalize_path("/var/lib/jenkins/workspace/job/src/x.ts", &[]),
            "src/x.ts"
        );
        assert_eq!(normalize_path("D:\\a\\repo\\repo\\x.ts", &[]), "x.ts");
        assert_eq!(normalize_path("/tmp/abc123/x/y.ts", &[]), "x/y.ts");
        assert_eq!(normalize_path("/srv/app/x/y.ts", &["/srv/app/"]), "x/y.ts");
        assert_eq!(
            normalize_path("file:///srv/app/x.ts", &["/srv/app"]),
            "x.ts"
        );
        assert_eq!(normalize_path("/opt/x.ts", &[]), "/opt/x.ts");
    }

    #[test]
    fn junit_names() {
        assert_eq!(
            split_junit_name("test_add[1-2-3]"),
            ("test_add".into(), vec![("params".into(), "1-2-3".into())])
        );
        assert_eq!(
            split_junit_name("adds(int, int)[2]"),
            (
                "adds".into(),
                vec![
                    ("signature".into(), "int, int".into()),
                    ("index".into(), "2".into())
                ]
            )
        );
        assert_eq!(
            split_junit_name("t[a[b]c]"),
            ("t".into(), vec![("params".into(), "a[b]c".into())])
        );
        assert_eq!(
            split_junit_name("plain name"),
            ("plain name".into(), vec![])
        );
        assert_eq!(split_junit_name("[1] only"), ("[1] only".into(), vec![]));
        assert_eq!(split_junit_name("[2]"), ("[2]".into(), vec![]));
    }
}
