//! Failure fingerprints: the error message with volatile parts removed (numbers, hex ids, UUIDs,
//! timestamps, temporary paths, line numbers) plus the top non-framework stack frames, hashed
//! with SHA-1 (first 16 hex digits). Equal fingerprints group the same failure across tests,
//! retries and launches.

use std::sync::LazyLock;

use regex::{Captures, Regex};
use sha1::{Digest, Sha1};

/// Stack frames that take part in the fingerprint.
pub const FINGERPRINT_FRAMES: usize = 3;
/// Characters of the message that are normalized (the rest is ignored).
pub const MESSAGE_SCAN_CHARS: usize = 2000;
/// Characters of the normalized message that are kept.
pub const NORMALIZED_CHARS: usize = 1000;
/// Lines of a trace that are scanned for frames.
pub const TRACE_SCAN_LINES: usize = 400;
/// Bytes of a trace that are scanned for frames.
pub const TRACE_SCAN_BYTES: usize = 64 * 1024;

fn re(p: &str) -> Regex {
    Regex::new(p).expect("static regex")
}

static UUID: LazyLock<Regex> = LazyLock::new(|| {
    re(r"[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}")
});
static TIMESTAMP: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r"\d{4}-\d{2}-\d{2}(?:[T ]\d{1,2}:\d{2}(?::\d{2})?(?:[.,]\d+)?(?:Z|[+-]\d{2}:?\d{2})?)?|\b\d{1,2}:\d{2}:\d{2}(?:[.,]\d+)?\b",
    )
});
static TEMP_PATH: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r#"(?:/private)?(?:/tmp|/var/tmp|/var/folders|/dev/shm)(?:/[^\s'"<>:;,)\]]*)?|[A-Za-z]:\\(?:Users\\[^\\\s]+\\AppData\\Local\\Temp|Windows\\Temp|Temp)(?:\\[^\s'"<>:;,)\]]*)?"#,
    )
});
static HEX_PREFIXED: LazyLock<Regex> =
    LazyLock::new(|| re(r"\b0[xX][0-9a-fA-F]+\b|@[0-9a-fA-F]{4,}\b"));
static HEX_WORD: LazyLock<Regex> = LazyLock::new(|| re(r"\b[0-9a-fA-F]{8,}\b"));
static LINE_NUMBER: LazyLock<Regex> =
    LazyLock::new(|| re(r"(\.[A-Za-z]{1,6}):\d+(?::\d+)?|\b(line) \d+"));
static NUMBER: LazyLock<Regex> = LazyLock::new(|| re(r"\d+(?:\.\d+)?"));
static SPACE: LazyLock<Regex> = LazyLock::new(|| re(r"\s+"));

static JAVA_FRAME: LazyLock<Regex> =
    LazyLock::new(|| re(r"^\s*at\s+([\w$.<>/]+)\(([^()]*)\)(?:\s+in\s+.*)?\s*$"));
static JS_FRAME: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^\s*at\s+(?:async\s+)?(?:new\s+)?(?:(.+?)\s+\()?([^()\s]+?)(?::\d+)?(?::\d+)?\)?\s*$")
});
static PY_FRAME: LazyLock<Regex> =
    LazyLock::new(|| re(r#"^\s*File "([^"]+)", line \d+, in (\S+)"#));
static PYTEST_LOCATION: LazyLock<Regex> =
    LazyLock::new(|| re(r"^([\w./\\-]+\.py):\d+: [\w.]*(?:Error|Exception|Failed|Exit)\w*"));
static GO_FUNC: LazyLock<Regex> =
    LazyLock::new(|| re(r"^((?:[\w.~-]+/)*[\w.~-]+\.(?:\(\*?[\w]+\)\.)?[\w.]+)\((?:[^()]*)\)$"));
static JAVA_LAMBDA: LazyLock<Regex> = LazyLock::new(|| re(r"\$\$Lambda[^.]*"));
static JAVA_SYNTHETIC: LazyLock<Regex> = LazyLock::new(|| re(r"\$\d+"));
static GO_TEST_LOG: LazyLock<Regex> = LazyLock::new(|| re(r"^\s+([\w-]+_test\.go):\d+: (.*)$"));

const JAVA_FRAMEWORK: &[&str] = &[
    "java.",
    "javax.",
    "jdk.",
    "sun.",
    "com.sun.",
    "org.junit.",
    "junit.",
    "org.opentest4j.",
    "org.testng.",
    "org.apache.maven.",
    "org.gradle.",
    "worker.org.gradle.",
    "kotlin.",
    "kotlinx.coroutines.",
    "scala.",
    "org.scalatest.",
    "io.qameta.allure.",
    "org.assertj.",
    "org.hamcrest.",
    "com.google.common.truth.",
    "org.mockito.",
    "net.bytebuddy.",
    "org.springframework.test.",
    "com.intellij.",
    "org.spockframework.",
    "groovy.",
    "org.codehaus.groovy.",
    "io.kotest.",
    "System.",
    "Microsoft.",
    "NUnit.",
    "Xunit.",
    "xunit.",
];

const PY_FRAMEWORK: &[&str] = &[
    "site-packages",
    "dist-packages",
    "/lib/python",
    "_pytest",
    "pluggy",
    "<frozen",
    "unittest/",
];

const GO_FRAMEWORK: &[&str] = &[
    "testing.",
    "runtime.",
    "reflect.",
    "github.com/stretchr/testify/",
];

const GENERIC_MESSAGES: &[&str] = &[
    "", "failed", "failure", "fail", "error", "errored", "broken",
];

/// The message with volatile parts replaced by placeholders and whitespace collapsed.
pub fn normalize_message(message: &str) -> String {
    let cut = match message.char_indices().nth(MESSAGE_SCAN_CHARS) {
        Some((i, _)) => &message[..i],
        None => message,
    };
    let s = UUID.replace_all(cut, "<uuid>");
    let s = TIMESTAMP.replace_all(&s, "<ts>");
    let s = TEMP_PATH.replace_all(&s, "<tmp>");
    let s = HEX_PREFIXED.replace_all(&s, |c: &Captures<'_>| {
        if c[0].starts_with('@') {
            "@<hex>".to_string()
        } else {
            "<hex>".to_string()
        }
    });
    let s = HEX_WORD.replace_all(&s, |c: &Captures<'_>| {
        if c[0].bytes().any(|b| b.is_ascii_digit()) {
            "<hex>".to_string()
        } else {
            c[0].to_string()
        }
    });
    let s = LINE_NUMBER.replace_all(&s, |c: &Captures<'_>| {
        c.get(1)
            .or_else(|| c.get(2))
            .map(|m| m.as_str().to_string())
            .unwrap_or_default()
    });
    let s = NUMBER.replace_all(&s, "<n>");
    let s = SPACE.replace_all(&s, " ");
    let s = s.trim();
    match s.char_indices().nth(NORMALIZED_CHARS) {
        Some((i, _)) => s[..i].to_string(),
        None => s.to_string(),
    }
}

/// The last two components of a path, without a `file://` scheme.
fn short_path(p: &str) -> String {
    let p = p.trim_start_matches("file://");
    let parts: Vec<&str> = p.split(['/', '\\']).filter(|s| !s.is_empty()).collect();
    let n = parts.len();
    parts[n.saturating_sub(2)..].join("/")
}

fn scanned(trace: &str) -> &str {
    if trace.len() <= TRACE_SCAN_BYTES {
        return trace;
    }
    let mut end = TRACE_SCAN_BYTES;
    while !trace.is_char_boundary(end) {
        end -= 1;
    }
    &trace[..end]
}

fn frame_of(line: &str) -> Option<Option<String>> {
    if let Some(c) = JAVA_FRAME.captures(line) {
        let f = JAVA_LAMBDA.replace_all(&c[1], "$$$$Lambda");
        let f = match f.rfind('/') {
            Some(i) => &f[i + 1..],
            None => &f[..],
        };
        if JAVA_FRAMEWORK.iter().any(|p| f.starts_with(p)) {
            return Some(None);
        }
        return Some(Some(JAVA_SYNTHETIC.replace_all(f, "$$n").into_owned()));
    }
    if let Some(c) = PY_FRAME.captures(line) {
        let (path, func) = (&c[1], &c[2]);
        if PY_FRAMEWORK.iter().any(|p| path.contains(p)) {
            return Some(None);
        }
        return Some(Some(format!("{}:{func}", short_path(path))));
    }
    if let Some(c) = PYTEST_LOCATION.captures(line) {
        return Some(Some(short_path(&c[1])));
    }
    if line.trim_start().starts_with("at ")
        && let Some(c) = JS_FRAME.captures(line)
    {
        let func = c.get(1).map(|m| m.as_str());
        let loc = &c[2];
        if loc.contains("node_modules/")
            || loc.starts_with("node:")
            || loc.starts_with("internal/")
            || loc == "<anonymous>"
            || loc == "native"
        {
            return Some(None);
        }
        let path = short_path(loc);
        return Some(Some(match func {
            Some(f) if !f.is_empty() => format!("{f}@{path}"),
            _ => path,
        }));
    }
    if line.starts_with("created by ") {
        return Some(None);
    }
    if let Some(c) = GO_FUNC.captures(line.trim_end()) {
        let f = &c[1];
        if GO_FRAMEWORK.iter().any(|p| f.starts_with(p)) || !f.contains('.') {
            return Some(None);
        }
        return Some(Some(f.to_string()));
    }
    if let Some(c) = GO_TEST_LOG.captures(line) {
        return Some(Some(c[1].to_string()));
    }
    None
}

/// Non-framework stack frames of a trace (Java/Kotlin/C#, Python, JavaScript, Go), top first,
/// without line numbers and with paths cut to their last two components. Repeated frames are
/// kept once.
pub fn stack_frames(trace: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in scanned(trace).lines().take(TRACE_SCAN_LINES) {
        if let Some(Some(f)) = frame_of(line)
            && !out.contains(&f)
        {
            out.push(f);
        }
    }
    out
}

/// What `go test` printed with `t.Error*`/`t.Fatal*` (the message of `gotestsum` reports is
/// just "Failed").
fn go_test_messages(trace: &str) -> Option<String> {
    let lines: Vec<&str> = scanned(trace)
        .lines()
        .take(TRACE_SCAN_LINES)
        .filter_map(|l| GO_TEST_LOG.captures(l).and_then(|c| c.get(2)))
        .map(|m| m.as_str())
        .take(5)
        .collect();
    (!lines.is_empty()).then(|| lines.join("\n"))
}

/// The message that identifies the failure: the given one, or for generic messages ("Failed")
/// what the trace says.
pub fn effective_message(message: Option<&str>, trace: Option<&str>) -> String {
    let m = message.unwrap_or("").trim();
    let generic = GENERIC_MESSAGES.iter().any(|g| m.eq_ignore_ascii_case(g));
    if !generic {
        return m.to_string();
    }
    let Some(t) = trace else {
        return m.to_string();
    };
    if let Some(g) = go_test_messages(t) {
        return g;
    }
    t.lines()
        .map(str::trim)
        .find(|l| {
            !l.is_empty()
                && !l.starts_with("=== ")
                && !l.starts_with("--- ")
                && frame_of(l).is_none()
        })
        .map(str::to_string)
        .unwrap_or_else(|| m.to_string())
}

/// A fingerprint and the normalized message it was made of.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fingerprint {
    /// 16 lowercase hex digits.
    pub hash: String,
    pub normalized_message: String,
    pub frames: Vec<String>,
}

/// The fingerprint of a failure; `None` without a message and a trace.
pub fn fingerprint(message: Option<&str>, trace: Option<&str>) -> Option<Fingerprint> {
    let message = message.filter(|m| !m.trim().is_empty());
    let trace = trace.filter(|t| !t.trim().is_empty());
    if message.is_none() && trace.is_none() {
        return None;
    }
    let normalized_message = normalize_message(&effective_message(message, trace));
    let mut frames = trace.map(stack_frames).unwrap_or_default();
    frames.truncate(FINGERPRINT_FRAMES);
    let mut h = Sha1::new();
    h.update(normalized_message.as_bytes());
    for f in &frames {
        h.update(b"\n");
        h.update(f.as_bytes());
    }
    let digest = h.finalize();
    let hash: String = digest.iter().take(8).map(|b| format!("{b:02x}")).collect();
    Some(Fingerprint {
        hash,
        normalized_message,
        frames,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn volatile_parts_are_removed() {
        let a = normalize_message(
            "Order 4711 failed at 2026-11-21T08:19:07.123Z for user 3f1b2c4d-1111-4222-8333-444455556666 in /tmp/pytest-of-ci/pytest-12/test_x0/data.json (ptr 0x7ffd5e8a, hash 9f86d081884c) Foo@5e9f23b4 at file.ts:56:18 line 42",
        );
        assert_eq!(
            a,
            "Order <n> failed at <ts> for user <uuid> in <tmp> (ptr <hex>, hash <hex>) Foo@<hex> at file.ts line"
        );
        assert_eq!(normalize_message("expected deadbeef"), "expected deadbeef");
        assert_eq!(
            normalize_message("C:\\Users\\ci\\AppData\\Local\\Temp\\x12\\f.txt missing"),
            "<tmp> missing"
        );
    }

    #[test]
    fn long_messages_are_bounded() {
        let m = "x ".repeat(100_000);
        let n = normalize_message(&m);
        assert!(n.chars().count() <= NORMALIZED_CHARS);
    }

    #[test]
    fn same_failure_with_other_numbers_has_one_fingerprint() {
        let t = "java.lang.IllegalStateException: boom\n\tat com.example.A.run(A.java:10)\n";
        let a = fingerprint(Some("Card vault connection refused (attempt 1)"), Some(t)).unwrap();
        let b = fingerprint(
            Some("Card vault connection refused (attempt 2)"),
            Some(&t.replace("10", "99")),
        )
        .unwrap();
        assert_eq!(a.hash, b.hash);
        assert_eq!(a.hash.len(), 16);
        let c = fingerprint(
            Some("Card vault connection refused (attempt 1)"),
            Some("\tat com.example.B.run(B.java:10)"),
        )
        .unwrap();
        assert_ne!(a.hash, c.hash);
        assert!(fingerprint(None, Some("  ")).is_none());
    }

    #[test]
    fn go_panic_frames() {
        let t = "panic: runtime error: index out of range [3] with length 3\n\ngoroutine 7 [running]:\nexample.com/shop/cart.(*Cart).Total(0xc000014090)\n\t/home/ci/shop/cart/cart.go:42 +0x1d\nexample.com/shop/cart.TestTotal(0xc0000a2340)\n\t/home/ci/shop/cart/cart_test.go:12 +0x25\ntesting.tRunner(0xc0000a2340, 0x5a1b30)\n\t/usr/local/go/src/testing/testing.go:1792 +0xf4\ncreated by testing.(*T).Run in goroutine 1\n";
        assert_eq!(
            stack_frames(t),
            vec![
                "example.com/shop/cart.(*Cart).Total",
                "example.com/shop/cart.TestTotal"
            ]
        );
    }
}
