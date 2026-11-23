//! Issue links from issue keys through the project's tracker URL templates (X4).

use super::settings::{IssueTracker, key_regex};

/// A link built from a key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IssueLink {
    pub tracker: String,
    pub key: String,
    pub url: String,
}

fn encode(key: &str) -> String {
    key.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

/// The link of the first tracker whose key regex matches the whole key.
pub fn issue_link(trackers: &[IssueTracker], key: &str) -> Option<IssueLink> {
    let key = key.trim();
    if key.is_empty() || key.len() > 128 {
        return None;
    }
    for t in trackers {
        let Ok(re) = key_regex(&t.key_regex) else {
            continue;
        };
        if re.is_match(key) {
            return Some(IssueLink {
                tracker: t.tracker.clone(),
                key: key.to_string(),
                url: t
                    .url_template
                    .replace("{key}", &encode(key.trim_start_matches('#'))),
            });
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trackers() -> Vec<IssueTracker> {
        vec![
            IssueTracker {
                tracker: "jira".into(),
                url_template: "https://jira.example.com/browse/{key}".into(),
                key_regex: "[A-Z][A-Z0-9]+-[0-9]+".into(),
                project_id: None,
                issue_type_id: None,
                id: None,
                api: None,
            },
            IssueTracker {
                tracker: "github".into(),
                url_template: "https://github.com/org/app/issues/{key}".into(),
                key_regex: "#?[0-9]+".into(),
                project_id: None,
                issue_type_id: None,
                id: None,
                api: None,
            },
        ]
    }

    #[test]
    fn links() {
        let t = trackers();
        assert_eq!(
            issue_link(&t, "PAY-123").unwrap().url,
            "https://jira.example.com/browse/PAY-123"
        );
        let gh = issue_link(&t, "#42").unwrap();
        assert_eq!(gh.tracker, "github");
        assert_eq!(gh.url, "https://github.com/org/app/issues/42");
        assert!(issue_link(&t, "PAY-123 extra").is_none());
        assert!(issue_link(&t, "x/../y").is_none());
        assert!(issue_link(&[], "PAY-1").is_none());
    }
}
