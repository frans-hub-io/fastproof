//! ReportPortal agent compatibility (`72-rp-compat-design.md`): the reporting subset of the
//! ReportPortal API (v1 and v2) translated into the native FastProof model.
//!
//! This crate holds the parts without I/O, shared by the live `/rp` endpoint of the server
//! and by importers: request models ([`model`]), time ([`time`]), statuses and log levels
//! ([`status`]), ids and defect locators ([`ids`]), the place of an item in the tree
//! ([`classify`]), the assembly of a finished test item into a
//! [`fastproof_model::ReportResult`] ([`assemble`]), project settings ([`settings`]) and a
//! streaming multipart reader with hard limits ([`multipart`]).

pub mod assemble;
pub mod classify;
pub mod ids;
pub mod model;
pub mod multipart;
pub mod settings;
pub mod status;
pub mod time;

/// Bytes of a stored item or launch name, at most (longer names are cut).
pub const MAX_NAME_BYTES: usize = 4096;
/// Bytes of a code reference or test case id, at most.
pub const MAX_REF_BYTES: usize = 2048;
/// Bytes of a description, at most.
pub const MAX_DESCRIPTION_BYTES: usize = 64 * 1024;
/// Attributes of one item or launch, at most (the rest is dropped).
pub const MAX_ATTRIBUTES: usize = 100;
/// Parameters of one item, at most.
pub const MAX_PARAMETERS: usize = 100;
/// Bytes of an attribute key, at most.
pub const MAX_ATTRIBUTE_KEY_BYTES: usize = 256;
/// Bytes of an attribute or parameter value, at most.
pub const MAX_ATTRIBUTE_VALUE_BYTES: usize = 1024;
/// Depth of the item tree, at most.
pub const MAX_DEPTH: i32 = 64;
/// Bytes of a step path in a log line (the native logs API takes at most 1024).
pub const MAX_STEP_PATH_BYTES: usize = 1024;
/// Bytes of one container name in a suite path (the native label value limit).
pub const MAX_SUITE_NAME_BYTES: usize = 1024;
/// Bytes of a whole suite path, at most: deeper containers are left out of it, so a stored
/// item (and its cached route) stays small however deep and long-named its containers are.
pub const MAX_SUITE_PATH_BYTES: usize = 4096;
/// Separator of names in paths (`a › b`).
pub const PATH_SEPARATOR: &str = " \u{203a} ";

/// Cuts `s` to at most `max` bytes at a character boundary.
pub fn cut(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

/// Removes NUL and other control characters except tab, newline and carriage return.
pub fn clean_text(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_control() || matches!(c, '\t' | '\n' | '\r'))
        .collect()
}

/// A name: control characters removed, trimmed, cut to [`MAX_NAME_BYTES`].
pub fn clean_name(s: &str) -> String {
    let t: String = s.chars().filter(|c| !c.is_control()).collect();
    cut(t.trim(), MAX_NAME_BYTES)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cut_keeps_char_boundaries() {
        assert_eq!(cut("héllo", 2), "h");
        assert_eq!(cut("héllo", 3), "hé");
        assert_eq!(cut("abc", 10), "abc");
    }

    #[test]
    fn names_lose_control_characters() {
        assert_eq!(clean_name("  a\u{0}b\nc "), "abc");
        assert_eq!(clean_text("a\u{0}b\nc"), "ab\nc");
    }
}
