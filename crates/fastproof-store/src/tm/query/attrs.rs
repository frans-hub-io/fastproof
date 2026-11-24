//! The attribute registry of FPQL: per entity, each attribute's name, type, operators, how its
//! values parse and its help text. Case attributes are here; requirement attributes live in
//! [`super::requirements`] (night 2).

use super::ast::Op;
use crate::tm::{QueryAttributeType, QueryEntity};

/// How the values of an attribute parse.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// A positive integer, also written `<prefix>-<n>` (`TC-12`, `REQ-3`, `SS-5`).
    Int {
        prefix: &'static str,
    },
    /// A whole number of days, `0..=36500`.
    Days,
    /// A string (quoted or a word).
    Str,
    /// Words for full-text search (`~` and `!~` only).
    Text,
    /// One of the attribute's `values` (case-insensitive).
    Enum,
    /// A key `^[a-z][a-z0-9_]{0,31}$` (workflow state).
    Key,
    Bool,
    /// `YYYY-MM-DD` (a whole UTC day) or `now() [+|-] N(d|h|m)`.
    Date,
    /// A user name or `me()`.
    User,
    /// A folder path (`"Payments/Cards"`) or id, or `subtree(path|id)`.
    Folder,
    /// A value of `field[key]` (checked against the field's type when resolved).
    Field,
    /// A plan id or name.
    Plan,
    /// The name of a saved query.
    Filter,
}

/// One attribute of an entity.
#[derive(Debug)]
pub struct AttrSpec {
    pub name: &'static str,
    pub ty: QueryAttributeType,
    pub kind: Kind,
    pub ops: &'static [Op],
    /// Enum values (statuses, origins, ...); dynamic ones come from the schema.
    pub values: &'static [&'static str],
    pub description: &'static str,
}

use Op::*;

const NUM_OPS: &[Op] = &[Eq, Ne, Gt, Ge, Lt, Le, In, NotIn];
const STR_OPS: &[Op] = &[Eq, Ne, Contains, NotContains, In, NotIn];
const STR_EMPTY_OPS: &[Op] = &[
    Eq,
    Ne,
    Contains,
    NotContains,
    In,
    NotIn,
    IsEmpty,
    IsNotEmpty,
];
const TEXT_OPS: &[Op] = &[Contains, NotContains];
const ENUM_OPS: &[Op] = &[Eq, Ne, In, NotIn];
const ENUM_EMPTY_OPS: &[Op] = &[Eq, Ne, In, NotIn, IsEmpty, IsNotEmpty];
const BOOL_OPS: &[Op] = &[Eq, Ne];
const DATE_OPS: &[Op] = &[Eq, Ne, Gt, Ge, Lt, Le];
const DATE_EMPTY_OPS: &[Op] = &[Eq, Ne, Gt, Ge, Lt, Le, IsEmpty, IsNotEmpty];
const FIELD_OPS: &[Op] = &[
    Eq,
    Ne,
    Contains,
    NotContains,
    Gt,
    Ge,
    Lt,
    Le,
    In,
    NotIn,
    IsEmpty,
    IsNotEmpty,
];
const DAYS_OPS: &[Op] = &[Gt, Ge, Lt, Le];

/// Case statuses.
pub const CASE_STATUSES: &[&str] = &["draft", "active", "outdated"];
/// Origins of a case.
pub const ORIGINS: &[&str] = &["ui", "code", "import", "ai"];
/// Review states.
pub const REVIEW_STATES: &[&str] = &["pending", "changes_requested", "approved"];
/// Final result statuses.
pub const RESULT_STATUSES: &[&str] = &["passed", "failed", "broken", "skipped", "unknown"];
/// Sources of a result.
pub const RESULT_SOURCES: &[&str] = &["manual", "automated"];

const fn attr(
    name: &'static str,
    ty: QueryAttributeType,
    kind: Kind,
    ops: &'static [Op],
    values: &'static [&'static str],
    description: &'static str,
) -> AttrSpec {
    AttrSpec {
        name,
        ty,
        kind,
        ops,
        values,
        description,
    }
}

use QueryAttributeType as T;

/// Attributes of cases.
pub static CASE_ATTRIBUTES: &[AttrSpec] = &[
    attr(
        "number",
        T::Number,
        Kind::Int { prefix: "TC" },
        NUM_OPS,
        &[],
        "Case number: 12 or TC-12.",
    ),
    attr(
        "name",
        T::String,
        Kind::Str,
        STR_OPS,
        &[],
        "Case name; ~ matches a part (at least 3 characters).",
    ),
    attr(
        "text",
        T::Text,
        Kind::Text,
        TEXT_OPS,
        &[],
        "Words of the name, description, preconditions and steps (full-text search).",
    ),
    attr(
        "status",
        T::Enum,
        Kind::Enum,
        ENUM_OPS,
        CASE_STATUSES,
        "Status category: draft, active or outdated.",
    ),
    attr(
        "state",
        T::Enum,
        Kind::Key,
        ENUM_EMPTY_OPS,
        &[],
        "Workflow state key; empty without a workflow.",
    ),
    attr(
        "automated",
        T::Bool,
        Kind::Bool,
        BOOL_OPS,
        &[],
        "Whether the case is automated.",
    ),
    attr(
        "folder",
        T::Folder,
        Kind::Folder,
        ENUM_EMPTY_OPS,
        &[],
        "Folder path (\"Payments/Cards\") or id; subtree(...) includes subfolders.",
    ),
    attr(
        "tag",
        T::String,
        Kind::Str,
        STR_EMPTY_OPS,
        &[],
        "A tag of the case.",
    ),
    attr(
        "field",
        T::Field,
        Kind::Field,
        FIELD_OPS,
        &[],
        "A custom field by key or name: field[\"Priority\"] = high.",
    ),
    attr(
        "createdBy",
        T::User,
        Kind::User,
        STR_EMPTY_OPS,
        &[],
        "Who created the case; me() is you.",
    ),
    attr(
        "updatedBy",
        T::User,
        Kind::User,
        STR_EMPTY_OPS,
        &[],
        "Who changed the case last; me() is you.",
    ),
    attr(
        "createdAt",
        T::Date,
        Kind::Date,
        DATE_OPS,
        &[],
        "When the case was created: 2026-01-31 or now() - 7d.",
    ),
    attr(
        "updatedAt",
        T::Date,
        Kind::Date,
        DATE_OPS,
        &[],
        "When the case was changed last.",
    ),
    attr(
        "origin",
        T::Enum,
        Kind::Enum,
        ENUM_OPS,
        ORIGINS,
        "Where the case came from: ui, code, import or ai.",
    ),
    attr(
        "aiUnreviewed",
        T::Bool,
        Kind::Bool,
        BOOL_OPS,
        &[],
        "An AI draft nobody has acknowledged yet.",
    ),
    attr(
        "review",
        T::Enum,
        Kind::Enum,
        ENUM_EMPTY_OPS,
        REVIEW_STATES,
        "Review state of the head version; empty when review does not apply.",
    ),
    attr(
        "lastStatus",
        T::Enum,
        Kind::Enum,
        ENUM_EMPTY_OPS,
        RESULT_STATUSES,
        "Status of the newest result; empty when never run.",
    ),
    attr(
        "lastRunAt",
        T::Date,
        Kind::Date,
        DATE_EMPTY_OPS,
        &[],
        "Time of the newest result.",
    ),
    attr(
        "lastSource",
        T::Enum,
        Kind::Enum,
        ENUM_EMPTY_OPS,
        RESULT_SOURCES,
        "Whether the newest result was manual or automated.",
    ),
    attr(
        "requirement",
        T::Ref,
        Kind::Int { prefix: "REQ" },
        ENUM_EMPTY_OPS,
        &[],
        "A requirement the case covers: REQ-12.",
    ),
    attr(
        "sharedStep",
        T::Ref,
        Kind::Int { prefix: "SS" },
        ENUM_EMPTY_OPS,
        &[],
        "A shared step the case uses: SS-3.",
    ),
    attr(
        "plan",
        T::Ref,
        Kind::Plan,
        ENUM_OPS,
        &[],
        "A test plan (id or name) the case is in.",
    ),
    attr(
        "hasParameters",
        T::Bool,
        Kind::Bool,
        BOOL_OPS,
        &[],
        "Whether the case has parameters.",
    ),
    attr(
        "staleDays",
        T::Number,
        Kind::Days,
        DAYS_OPS,
        &[],
        "Days an automated case has had no trusted result: staleDays > 30.",
    ),
    attr(
        "archived",
        T::Bool,
        Kind::Bool,
        BOOL_OPS,
        &[],
        "Archived cases (the trash); lists leave them out unless the query names this attribute.",
    ),
    attr(
        "filter",
        T::Ref,
        Kind::Filter,
        BOOL_OPS,
        &[],
        "A saved query by name: filter = \"Regression core\".",
    ),
];

/// The attributes of an entity.
pub fn registry(entity: QueryEntity) -> &'static [AttrSpec] {
    match entity {
        QueryEntity::Cases => CASE_ATTRIBUTES,
        QueryEntity::Requirements => super::requirements::ATTRIBUTES,
    }
}

/// An attribute by name (case-insensitive).
pub fn find(entity: QueryEntity, name: &str) -> Option<&'static AttrSpec> {
    registry(entity)
        .iter()
        .find(|a| a.name.eq_ignore_ascii_case(name))
}

/// Whether FPQL over the entity is available in this build.
pub fn available(entity: QueryEntity) -> bool {
    match entity {
        QueryEntity::Cases => true,
        QueryEntity::Requirements => super::requirements::AVAILABLE,
    }
}
