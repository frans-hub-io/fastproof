//! Item and launch ids, the `uniqueId` of v1 answers, and defect type locators.

use md5::{Digest, Md5};
use sha2::Sha256;

/// Longest uuid accepted from a client.
pub const MAX_UUID_BYTES: usize = 64;

/// A client uuid: 1 to 64 bytes of `[A-Za-z0-9._-]`.
pub fn valid_uuid(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= MAX_UUID_BYTES
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// The `uniqueId` of a v1 item answer: `auto:` and an MD5 of the code reference (or the
/// launch-relative path) and the parameters, like ReportPortal's own.
pub fn unique_id(path: &str, params: &[(Option<String>, Option<String>)]) -> String {
    let mut h = Md5::new();
    h.update(path.as_bytes());
    for (k, v) in params {
        h.update([0u8]);
        h.update(k.as_deref().unwrap_or_default().as_bytes());
        h.update([1u8]);
        h.update(v.as_deref().unwrap_or_default().as_bytes());
    }
    format!("auto:{}", hex::encode(h.finalize()))
}

/// A built-in defect group: native group, ReportPortal type, locator, names and color.
#[derive(Clone, Copy, Debug)]
pub struct Group {
    pub group: &'static str,
    pub type_ref: &'static str,
    pub locator: &'static str,
    pub prefix: &'static str,
    pub long_name: &'static str,
    pub short_name: &'static str,
    pub color: &'static str,
}

/// The five defect groups in ReportPortal order.
pub const GROUPS: [Group; 5] = [
    Group {
        group: "productBug",
        type_ref: "PRODUCT_BUG",
        locator: "pb001",
        prefix: "pb",
        long_name: "Product Bug",
        short_name: "PB",
        color: "#ec3900",
    },
    Group {
        group: "automationBug",
        type_ref: "AUTOMATION_BUG",
        locator: "ab001",
        prefix: "ab",
        long_name: "Automation Bug",
        short_name: "AB",
        color: "#f7d63e",
    },
    Group {
        group: "systemIssue",
        type_ref: "SYSTEM_ISSUE",
        locator: "si001",
        prefix: "si",
        long_name: "System Issue",
        short_name: "SI",
        color: "#0274d1",
    },
    Group {
        group: "noDefect",
        type_ref: "NO_DEFECT",
        locator: "nd001",
        prefix: "nd",
        long_name: "No Defect",
        short_name: "ND",
        color: "#777777",
    },
    Group {
        group: "toInvestigate",
        type_ref: "TO_INVESTIGATE",
        locator: "ti001",
        prefix: "ti",
        long_name: "To Investigate",
        short_name: "TI",
        color: "#ffb743",
    },
];

/// The group of a native group name.
pub fn group(name: &str) -> Option<&'static Group> {
    GROUPS.iter().find(|g| g.group == name)
}

/// The stable locator of a custom defect type: `<prefix>_<first 8 hex of sha256(id)>`.
pub fn custom_locator(group: &Group, id: &str) -> String {
    let h = Sha256::digest(id.as_bytes());
    format!("{}_{}", group.prefix, &hex::encode(h)[..8])
}

/// What an `issueType` of a finished item names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IssueType {
    /// `NOT_ISSUE`: a skip without a defect; ignored.
    NotIssue,
    /// A group's built-in type, or a custom type (its id) of the group.
    Defect {
        group: &'static str,
        custom: Option<String>,
    },
}

/// Resolves an `issueType` (a locator such as `pb001` or `ab_1a2b3c4d`, or a type such as
/// `PRODUCT_BUG`) against the custom types of the project (`(id, group)`).
pub fn issue_type(v: &str, custom: &[(String, String)]) -> Option<IssueType> {
    let t = v.trim();
    if t.eq_ignore_ascii_case("NOT_ISSUE") || t.eq_ignore_ascii_case("not_issue_flag") {
        return Some(IssueType::NotIssue);
    }
    let lower = t.to_ascii_lowercase();
    for g in &GROUPS {
        if lower == g.locator || t.eq_ignore_ascii_case(g.type_ref) {
            return Some(IssueType::Defect {
                group: g.group,
                custom: None,
            });
        }
    }
    for (id, grp) in custom {
        if let Some(g) = group(grp)
            && custom_locator(g, id) == lower
        {
            return Some(IssueType::Defect {
                group: g.group,
                custom: Some(id.clone()),
            });
        }
    }
    let prefix = lower
        .split(['_', '0', '1', '2', '3', '4', '5', '6', '7', '8', '9'])
        .next()?;
    GROUPS
        .iter()
        .find(|g| g.prefix == prefix)
        .map(|g| IssueType::Defect {
            group: g.group,
            custom: None,
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uuids() {
        assert!(valid_uuid("4b3c2e1a-0f5d-4d7a-9b1e-123456789abc"));
        assert!(valid_uuid("a.b_c-1"));
        assert!(!valid_uuid(""));
        assert!(!valid_uuid("a/b"));
        assert!(!valid_uuid("../x"));
        assert!(!valid_uuid("a b"));
        assert!(!valid_uuid(&"a".repeat(65)));
        assert!(!valid_uuid("a\u{0}b"));
    }

    #[test]
    fn unique_ids_depend_on_parameters() {
        let a = unique_id("t.py:test", &[]);
        let b = unique_id("t.py:test", &[(Some("n".into()), Some("1".into()))]);
        assert!(a.starts_with("auto:") && a.len() == 37);
        assert_ne!(a, b);
    }

    #[test]
    fn issue_types() {
        let custom = vec![("0193-x".to_string(), "productBug".to_string())];
        let loc = custom_locator(group("productBug").unwrap(), "0193-x");
        assert!(loc.starts_with("pb_") && loc.len() == 11);
        assert_eq!(
            issue_type(&loc, &custom),
            Some(IssueType::Defect {
                group: "productBug",
                custom: Some("0193-x".into())
            })
        );
        assert_eq!(
            issue_type("AB001", &custom),
            Some(IssueType::Defect {
                group: "automationBug",
                custom: None
            })
        );
        assert_eq!(
            issue_type("SYSTEM_ISSUE", &custom),
            Some(IssueType::Defect {
                group: "systemIssue",
                custom: None
            })
        );
        assert_eq!(issue_type("NOT_ISSUE", &custom), Some(IssueType::NotIssue));
        assert_eq!(
            issue_type("si_deadbeef", &custom),
            Some(IssueType::Defect {
                group: "systemIssue",
                custom: None
            })
        );
        assert_eq!(issue_type("zz123", &custom), None);
    }
}
