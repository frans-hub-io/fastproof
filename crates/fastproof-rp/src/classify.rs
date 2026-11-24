//! The place of a started item in the tree (design §4): its role, where its log lines go and
//! which row holds their sequence numbers and the error text.
//!
//! - `BEFORE_*`/`AFTER_*` under a container (or at the root) is a **fixture**; under a test
//!   it is a step of that test.
//! - `hasStats=false` is a **step** of the nearest result (or fixture, or container).
//! - `hasStats=true` is a **result** when its type is `STEP`, `SCENARIO` or `TEST`, else a
//!   **container**. A result that gets a `hasStats=true` child becomes a container (the
//!   leaf rule).
//! - An item whose parent did not arrive yet is **pending** until it does.

use crate::status::{is_fixture_type, is_test_type};
use crate::{MAX_STEP_PATH_BYTES, MAX_SUITE_NAME_BYTES, MAX_SUITE_PATH_BYTES, PATH_SEPARATOR, cut};

/// The role of an item.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Result,
    Container,
    Step,
    Fixture,
    Pending,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Result => "result",
            Role::Container => "container",
            Role::Step => "step",
            Role::Fixture => "fixture",
            Role::Pending => "pending",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "result" => Role::Result,
            "container" => Role::Container,
            "step" => Role::Step,
            "fixture" => Role::Fixture,
            "pending" => Role::Pending,
            _ => return None,
        })
    }
}

/// Log key of the lines of a launch without an item.
pub const LAUNCH_LOG_KEY: &str = "rp-launch";

/// Log key (result id of native log lines) of a result or failed fixture.
pub fn result_key(uuid: &str) -> String {
    format!("rp:{uuid}")
}

/// Log key of the lines of a container.
pub fn container_key(uuid: &str) -> String {
    format!("rp-container:{uuid}")
}

/// What classification needs to know of the parent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Parent {
    pub uuid: String,
    pub role: Role,
    /// Upper-case item type.
    pub item_type: String,
    pub name: String,
    pub depth: i32,
    pub owner_uuid: Option<String>,
    pub log_key: String,
    pub log_step: Option<String>,
    pub seq_uuid: Option<String>,
    pub error_uuid: Option<String>,
    pub suite_path: Vec<String>,
}

/// The place of a new item.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Placement {
    pub role: Role,
    pub depth: i32,
    /// The result, fixture or container whose steps include this step (steps only).
    pub owner_uuid: Option<String>,
    /// Result id of its log lines.
    pub log_key: String,
    /// Step name of its log lines.
    pub log_step: Option<String>,
    /// The item whose row counts the sequence numbers of `log_key` (`None`: the launch).
    pub seq_uuid: Option<String>,
    /// The result or fixture whose error text its ERROR lines set.
    pub error_uuid: Option<String>,
    /// Names of the containers above it, outermost first.
    pub suite_path: Vec<String>,
    /// The parent was a result and becomes a container (leaf rule).
    pub parent_becomes_container: bool,
}

/// Adds a container name to a suite path within [`MAX_SUITE_PATH_BYTES`] (the name cut to
/// [`MAX_SUITE_NAME_BYTES`]); a name that does not fit is left out.
fn push_suite(path: &mut Vec<String>, name: &str) {
    let name = cut(name, MAX_SUITE_NAME_BYTES);
    let used: usize = path.iter().map(String::len).sum();
    if used + name.len() <= MAX_SUITE_PATH_BYTES {
        path.push(name);
    }
}

fn join_step(parent: Option<&str>, name: &str) -> String {
    let s = match parent {
        Some(p) if !p.is_empty() => format!("{p}{PATH_SEPARATOR}{name}"),
        _ => name.to_string(),
    };
    cut(&s, MAX_STEP_PATH_BYTES)
}

/// Where an item with this uuid, name, upper-case type and `hasStats` goes under `parent`
/// (`None`: a root item). An item whose parent is pending is pending too.
pub fn place(
    uuid: &str,
    name: &str,
    item_type: &str,
    has_stats: bool,
    parent: Option<&Parent>,
) -> Placement {
    let depth = parent.map_or(0, |p| p.depth + 1);
    let suite_path = match parent {
        Some(p) if matches!(p.role, Role::Container | Role::Result) => {
            let mut s = p.suite_path.clone();
            push_suite(&mut s, &p.name);
            s
        }
        Some(p) => p.suite_path.clone(),
        None => Vec::new(),
    };
    let own = |role: Role| -> Placement {
        let (log_key, error_uuid) = match role {
            Role::Container | Role::Pending => (container_key(uuid), None),
            _ => (result_key(uuid), Some(uuid.to_string())),
        };
        Placement {
            role,
            depth,
            owner_uuid: None,
            log_key,
            log_step: None,
            seq_uuid: Some(uuid.to_string()),
            error_uuid,
            suite_path: suite_path.clone(),
            parent_becomes_container: false,
        }
    };
    let Some(p) = parent else {
        if is_fixture_type(item_type) {
            return Placement {
                role: Role::Fixture,
                depth,
                owner_uuid: None,
                log_key: LAUNCH_LOG_KEY.to_string(),
                log_step: Some(join_step(None, name)),
                seq_uuid: None,
                error_uuid: Some(uuid.to_string()),
                suite_path,
                parent_becomes_container: false,
            };
        }
        if !has_stats {
            return Placement {
                role: Role::Step,
                depth,
                owner_uuid: None,
                log_key: LAUNCH_LOG_KEY.to_string(),
                log_step: Some(join_step(None, name)),
                seq_uuid: None,
                error_uuid: None,
                suite_path,
                parent_becomes_container: false,
            };
        }
        return own(if is_test_type(item_type) {
            Role::Result
        } else {
            Role::Container
        });
    };
    if p.role == Role::Pending {
        return own(Role::Pending);
    }
    let step = |p: &Parent| -> Placement {
        let (owner, log_key, log_step, seq, err) = match p.role {
            Role::Result => (
                Some(p.uuid.clone()),
                result_key(&p.uuid),
                join_step(None, name),
                Some(p.uuid.clone()),
                Some(p.uuid.clone()),
            ),
            Role::Fixture => (
                Some(p.uuid.clone()),
                p.log_key.clone(),
                join_step(p.log_step.as_deref(), name),
                p.seq_uuid.clone(),
                Some(p.uuid.clone()),
            ),
            Role::Container => (
                Some(p.uuid.clone()),
                container_key(&p.uuid),
                join_step(None, name),
                Some(p.uuid.clone()),
                None,
            ),
            _ => (
                p.owner_uuid.clone(),
                p.log_key.clone(),
                join_step(p.log_step.as_deref(), name),
                p.seq_uuid.clone(),
                p.error_uuid.clone(),
            ),
        };
        Placement {
            role: Role::Step,
            depth,
            owner_uuid: owner,
            log_key,
            log_step: Some(log_step),
            seq_uuid: seq,
            error_uuid: err,
            suite_path: suite_path.clone(),
            parent_becomes_container: false,
        }
    };
    // A `TEST` result with a fixture under it is a TestNG class or `<test>`: a container.
    let tentative = p.role == Role::Result && p.item_type == "TEST";
    let under_test = matches!(p.role, Role::Result | Role::Step | Role::Fixture) && !tentative;
    if is_fixture_type(item_type) {
        if under_test {
            return step(p);
        }
        let mut suite_path = suite_path;
        if tentative && suite_path.last() != Some(&cut(&p.name, MAX_SUITE_NAME_BYTES)) {
            push_suite(&mut suite_path, &p.name);
        }
        return Placement {
            role: Role::Fixture,
            depth,
            owner_uuid: None,
            log_key: if tentative {
                container_key(&p.uuid)
            } else {
                p.log_key.clone()
            },
            log_step: Some(join_step(None, name)),
            seq_uuid: p.seq_uuid.clone(),
            error_uuid: Some(uuid.to_string()),
            suite_path,
            parent_becomes_container: tentative,
        };
    }
    if !has_stats || matches!(p.role, Role::Step | Role::Fixture) {
        return step(p);
    }
    let mut out = own(if is_test_type(item_type) {
        Role::Result
    } else {
        Role::Container
    });
    out.parent_becomes_container = p.role == Role::Result;
    out
}

/// The parent view of an item that was placed.
pub fn as_parent(uuid: &str, name: &str, item_type: &str, p: &Placement) -> Parent {
    Parent {
        uuid: uuid.to_string(),
        role: p.role,
        item_type: item_type.to_string(),
        name: name.to_string(),
        depth: p.depth,
        owner_uuid: p.owner_uuid.clone(),
        log_key: p.log_key.clone(),
        log_step: p.log_step.clone(),
        seq_uuid: p.seq_uuid.clone(),
        error_uuid: p.error_uuid.clone(),
        suite_path: p.suite_path.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn placed(uuid: &str, name: &str, t: &str, stats: bool, parent: Option<&Parent>) -> Parent {
        let p = place(uuid, name, t, stats, parent);
        as_parent(uuid, name, t, &p)
    }

    #[test]
    fn playwright_tree() {
        let file = placed("f", "example.spec.ts", "SUITE", true, None);
        assert_eq!(file.role, Role::Container);
        let describe = placed("d", "Checkout", "SUITE", true, Some(&file));
        let test = place("t", "pays", "TEST", true, Some(&describe));
        assert_eq!(test.role, Role::Result);
        assert_eq!(test.suite_path, vec!["example.spec.ts", "Checkout"]);
        assert_eq!(test.log_key, "rp:t");
        let test = as_parent("t", "pays", "TEST", &test);
        let s1 = placed("s1", "open page", "STEP", false, Some(&test));
        assert_eq!(s1.role, Role::Step);
        assert_eq!(s1.owner_uuid.as_deref(), Some("t"));
        assert_eq!(s1.log_key, "rp:t");
        assert_eq!(s1.log_step.as_deref(), Some("open page"));
        let s2 = place("s2", "click", "STEP", false, Some(&s1));
        assert_eq!(s2.owner_uuid.as_deref(), Some("t"));
        assert_eq!(s2.log_step.as_deref(), Some("open page \u{203a} click"));
        assert_eq!(s2.seq_uuid.as_deref(), Some("t"));
        assert_eq!(s2.error_uuid.as_deref(), Some("t"));
        assert_eq!(s2.depth, 4);
    }

    #[test]
    fn leaf_rule_turns_a_test_with_tests_into_a_container() {
        let suite = placed("s", "suite", "SUITE", true, None);
        let xml_test = placed("x", "regression", "TEST", true, Some(&suite));
        assert_eq!(xml_test.role, Role::Result);
        let method = place("m", "pays", "STEP", true, Some(&xml_test));
        assert_eq!(method.role, Role::Result);
        assert!(method.parent_becomes_container);
        assert_eq!(method.suite_path, vec!["suite", "regression"]);
    }

    #[test]
    fn fixtures() {
        let class = placed("c", "OrderTest", "SUITE", true, None);
        let before = place("b", "setUp", "BEFORE_CLASS", true, Some(&class));
        assert_eq!(before.role, Role::Fixture);
        assert_eq!(before.log_key, "rp-container:c");
        assert_eq!(before.log_step.as_deref(), Some("setUp"));
        assert_eq!(before.seq_uuid.as_deref(), Some("c"));
        assert_eq!(before.error_uuid.as_deref(), Some("b"));
        let before = as_parent("b", "setUp", "BEFORE_CLASS", &before);
        let inner = place("bi", "connect", "STEP", false, Some(&before));
        assert_eq!(inner.role, Role::Step);
        assert_eq!(inner.owner_uuid.as_deref(), Some("b"));
        assert_eq!(inner.log_key, "rp-container:c");
        assert_eq!(inner.log_step.as_deref(), Some("setUp \u{203a} connect"));
        assert_eq!(inner.error_uuid.as_deref(), Some("b"));
        let test = placed("t", "pays", "STEP", true, Some(&class));
        let method_fixture = place("mf", "init", "BEFORE_METHOD", true, Some(&test));
        assert_eq!(method_fixture.role, Role::Step);
        assert_eq!(method_fixture.owner_uuid.as_deref(), Some("t"));
        let class = placed("k", "CartTest", "TEST", true, Some(&class));
        let setup = place("ks", "setUpClass", "BEFORE_CLASS", true, Some(&class));
        assert_eq!(setup.role, Role::Fixture);
        assert!(setup.parent_becomes_container);
        assert_eq!(setup.log_key, "rp-container:k");
        assert_eq!(
            setup.suite_path.last().map(String::as_str),
            Some("CartTest")
        );
        let root = place("r", "global", "BEFORE_SUITE", true, None);
        assert_eq!(root.role, Role::Fixture);
        assert_eq!(root.log_key, LAUNCH_LOG_KEY);
        assert_eq!(root.seq_uuid, None);
    }

    #[test]
    fn steps_of_containers_and_the_root() {
        let suite = placed("s", "suite", "SUITE", true, None);
        let st = place("x", "prepare", "STEP", false, Some(&suite));
        assert_eq!(st.role, Role::Step);
        assert_eq!(st.log_key, "rp-container:s");
        assert_eq!(st.error_uuid, None);
        let root = place("y", "log", "STEP", false, None);
        assert_eq!(root.log_key, LAUNCH_LOG_KEY);
        assert_eq!(root.owner_uuid, None);
    }

    #[test]
    fn pending_parents_make_pending_children() {
        let pending = Parent {
            uuid: "p".into(),
            role: Role::Pending,
            item_type: "SUITE".into(),
            name: "p".into(),
            depth: 0,
            owner_uuid: None,
            log_key: container_key("p"),
            log_step: None,
            seq_uuid: Some("p".into()),
            error_uuid: None,
            suite_path: vec![],
        };
        let c = place("c", "child", "STEP", true, Some(&pending));
        assert_eq!(c.role, Role::Pending);
        assert_eq!(c.log_key, "rp-container:c");
    }

    #[test]
    fn long_step_paths_are_cut() {
        let t = placed("t", "t", "STEP", true, None);
        let mut p = placed("s0", &"x".repeat(600), "STEP", false, Some(&t));
        for i in 1..4 {
            p = placed(&format!("s{i}"), &"y".repeat(600), "STEP", false, Some(&p));
        }
        assert!(p.log_step.unwrap().len() <= MAX_STEP_PATH_BYTES);
        assert!(Role::parse("result") == Some(Role::Result) && Role::parse("x").is_none());
    }

    #[test]
    fn deep_long_named_containers_keep_suite_paths_small() {
        let mut p = placed("c0", &"n".repeat(4096), "SUITE", true, None);
        for i in 1..64 {
            p = placed(&format!("c{i}"), &"m".repeat(4096), "SUITE", true, Some(&p));
        }
        let t = place("t", "t", "STEP", true, Some(&p));
        let bytes: usize = t.suite_path.iter().map(String::len).sum();
        assert!(bytes <= MAX_SUITE_PATH_BYTES, "{bytes}");
        assert!(t.suite_path.iter().all(|n| n.len() <= MAX_SUITE_NAME_BYTES));
        assert_eq!(t.suite_path.len(), 4);
        assert_eq!(t.depth, 64);
    }
}
