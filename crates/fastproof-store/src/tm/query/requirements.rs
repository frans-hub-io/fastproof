//! FPQL attributes of requirements (§8): `number, title, type, status, parent, issue,
//! externalState, coverage`. Conditions compile on requirements aliased `r` with `push_bind`
//! only. `coverage` is the default scope (each case's last result) of the requirement with
//! everything under it, as `GET /coverage` without a scope counts it.

use sqlx::{Postgres, QueryBuilder};
use uuid::Uuid;

use super::ast::{Cond, Op, Val};
use super::attrs::{AttrSpec, Kind};
use crate::launches::like_escape;
use crate::no_nul;
use crate::tm::QueryAttributeType as T;

/// Whether requirement queries are available in this build.
pub const AVAILABLE: bool = true;

/// Requirement statuses.
pub const REQUIREMENT_STATUSES: &[&str] = &["draft", "active", "done", "obsolete"];
/// Coverage statuses.
pub const COVERAGE_STATUSES: &[&str] = &["uncovered", "failed", "notRun", "partial", "passed"];

const NUM_OPS: &[Op] = &[
    Op::Eq,
    Op::Ne,
    Op::Gt,
    Op::Ge,
    Op::Lt,
    Op::Le,
    Op::In,
    Op::NotIn,
];
const STR_OPS: &[Op] = &[
    Op::Eq,
    Op::Ne,
    Op::Contains,
    Op::NotContains,
    Op::In,
    Op::NotIn,
];
const STR_EMPTY_OPS: &[Op] = &[
    Op::Eq,
    Op::Ne,
    Op::Contains,
    Op::NotContains,
    Op::In,
    Op::NotIn,
    Op::IsEmpty,
    Op::IsNotEmpty,
];
const ENUM_OPS: &[Op] = &[Op::Eq, Op::Ne, Op::In, Op::NotIn];
const REF_OPS: &[Op] = &[
    Op::Eq,
    Op::Ne,
    Op::In,
    Op::NotIn,
    Op::IsEmpty,
    Op::IsNotEmpty,
];

/// Attributes of requirements.
pub static ATTRIBUTES: &[AttrSpec] = &[
    AttrSpec {
        name: "number",
        ty: T::Number,
        kind: Kind::Int { prefix: "REQ" },
        ops: NUM_OPS,
        values: &[],
        description: "Requirement number: 12 or REQ-12.",
    },
    AttrSpec {
        name: "title",
        ty: T::String,
        kind: Kind::Str,
        ops: STR_OPS,
        values: &[],
        description: "Title; ~ matches a part (at least 3 characters).",
    },
    AttrSpec {
        name: "type",
        ty: T::String,
        kind: Kind::Str,
        ops: STR_OPS,
        values: &[],
        description: "Type: requirement, epic, story, risk or your own.",
    },
    AttrSpec {
        name: "status",
        ty: T::Enum,
        kind: Kind::Enum,
        ops: ENUM_OPS,
        values: REQUIREMENT_STATUSES,
        description: "Status: draft, active, done or obsolete.",
    },
    AttrSpec {
        name: "parent",
        ty: T::Ref,
        kind: Kind::Int { prefix: "REQ" },
        ops: REF_OPS,
        values: &[],
        description: "The requirement directly above: REQ-3; empty for top-level ones.",
    },
    AttrSpec {
        name: "issue",
        ty: T::String,
        kind: Kind::Str,
        ops: STR_EMPTY_OPS,
        values: &[],
        description: "Key of the tracker issue: SHOP-12; empty when none.",
    },
    AttrSpec {
        name: "externalState",
        ty: T::String,
        kind: Kind::Str,
        ops: STR_EMPTY_OPS,
        values: &[],
        description: "State of the tracker issue as the tracker names it.",
    },
    AttrSpec {
        name: "coverage",
        ty: T::Enum,
        kind: Kind::Enum,
        ops: ENUM_OPS,
        values: COVERAGE_STATUSES,
        description: "Coverage with everything below, by each case's last result: uncovered, failed, \
            notRun, partial or passed.",
    },
];

/// The default-scope coverage status of `r` with its subtree (the rules of `coverage.rs`).
const COVERAGE_SQL: &str = "(select case when count(*) = 0 then 'uncovered' \
      when count(*) filter (where c.last_status in ('failed', 'broken')) > 0 then 'failed' \
      when count(*) filter (where c.last_status = 'passed') = count(*) then 'passed' \
      when count(*) filter (where c.last_status = 'passed') = 0 then 'notRun' \
      else 'partial' end \
    from (select distinct l.case_id from requirements d \
      join requirement_links l on l.requirement_id = d.id and l.status = 'active' \
      where d.project_id = r.project_id and d.archived_at is null and d.path_ids @> array[r.id]) x \
    join cases c on c.id = x.case_id and c.archived_at is null)";

fn texts(c: &Cond) -> Vec<String> {
    c.values
        .iter()
        .filter_map(|v| v.v.text().map(no_nul))
        .collect()
}

fn ints(c: &Cond) -> Vec<i64> {
    c.values
        .iter()
        .filter_map(|v| match v.v {
            Val::Int(n) => Some(n),
            _ => None,
        })
        .collect()
}

fn cmp(op: Op) -> &'static str {
    match op {
        Op::Gt => " > ",
        Op::Ge => " >= ",
        Op::Lt => " < ",
        Op::Le => " <= ",
        _ => " = ",
    }
}

fn contains_pattern(c: &Cond) -> String {
    format!(
        "%{}%",
        like_escape(c.values.first().and_then(|v| v.v.text()).unwrap_or(""))
    )
}

/// `col <op> values` for a string column; nullable ones are guarded so a negation matches
/// exactly the other requirements.
fn push_str(qb: &mut QueryBuilder<Postgres>, col: &str, nullable: bool, op: Op, c: &Cond) {
    match op {
        Op::IsEmpty => {
            qb.push(format!("{col} is null"));
        }
        Op::Contains => {
            qb.push(format!("({col} is not null and {col} ilike "))
                .push_bind(contains_pattern(c))
                .push(")");
        }
        _ if nullable => {
            qb.push(format!("({col} is not null and {col} = any("))
                .push_bind(texts(c))
                .push("))");
        }
        _ => {
            qb.push(format!("{col} = any("))
                .push_bind(texts(c))
                .push(")");
        }
    }
}

/// Appends one condition on requirements aliased `r` (no leading ` and `).
pub fn push_condition(qb: &mut QueryBuilder<Postgres>, project_id: Uuid, c: &Cond) {
    let (op, negate) = c.op.positive();
    if negate {
        qb.push("not (");
    }
    match c.attr.name {
        "number" => {
            if matches!(op, Op::Eq | Op::In) {
                qb.push("r.number = any(").push_bind(ints(c)).push(")");
            } else {
                qb.push("r.number")
                    .push(cmp(op))
                    .push_bind(ints(c).first().copied().unwrap_or(0));
            }
        }
        "title" => push_str(qb, "r.title", false, op, c),
        "type" => push_str(qb, "r.type", false, op, c),
        "status" => push_str(qb, "r.status", false, op, c),
        "issue" => push_str(qb, "r.issue_key", true, op, c),
        "externalState" => push_str(qb, "r.external_state", true, op, c),
        "parent" => {
            if op == Op::IsEmpty {
                qb.push("r.parent_id is null");
            } else {
                qb.push("(r.parent_id is not null and r.parent_id in (select p.id from requirements p where p.project_id = ")
                    .push_bind(project_id)
                    .push(" and p.number = any(")
                    .push_bind(ints(c))
                    .push(")))");
            }
        }
        "coverage" => {
            qb.push(COVERAGE_SQL)
                .push(" = any(")
                .push_bind(texts(c))
                .push(")");
        }
        _ => {
            qb.push("false");
        }
    }
    if negate {
        qb.push(")");
    }
}
