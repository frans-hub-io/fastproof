//! FPQL syntax tree (§8). The parser builds it with byte spans into the source; the resolver
//! replaces names (folders, fields, saved queries, `me()`) by what they mean, so the compiler
//! never needs the database. [`Query`] prints in the normalized form.

use std::fmt;

use chrono::NaiveDate;
use uuid::Uuid;

use crate::tm::{FieldType, QueryEntity};

/// A byte range of the source.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Span {
    pub start: usize,
    pub end: usize,
}

impl Span {
    pub fn new(start: usize, end: usize) -> Self {
        Self { start, end }
    }

    /// The smallest span covering both.
    pub fn to(self, other: Span) -> Span {
        Span::new(self.start.min(other.start), self.end.max(other.end))
    }
}

/// A parsed query. `root` is `None` for an empty query (it matches everything).
#[derive(Clone, Debug, PartialEq)]
pub struct Query {
    pub entity: QueryEntity,
    pub root: Option<Expr>,
    /// The source the spans point into.
    pub src: String,
}

/// A boolean expression.
#[derive(Clone, Debug, PartialEq)]
pub enum Expr {
    And(Vec<Expr>),
    Or(Vec<Expr>),
    Not(Box<Expr>),
    Cond(Cond),
}

/// A comparison operator.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Op {
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
}

impl Op {
    /// How the operator is written.
    pub fn as_str(self) -> &'static str {
        match self {
            Op::Eq => "=",
            Op::Ne => "!=",
            Op::Contains => "~",
            Op::NotContains => "!~",
            Op::Gt => ">",
            Op::Ge => ">=",
            Op::Lt => "<",
            Op::Le => "<=",
            Op::In => "in",
            Op::NotIn => "not in",
            Op::IsEmpty => "is empty",
            Op::IsNotEmpty => "is not empty",
        }
    }

    /// The operator that matches exactly the other cases (`=` ↔ `!=`, `in` ↔ `not in`, ...).
    pub fn positive(self) -> (Op, bool) {
        match self {
            Op::Ne => (Op::Eq, true),
            Op::NotContains => (Op::Contains, true),
            Op::NotIn => (Op::In, true),
            Op::IsNotEmpty => (Op::IsEmpty, true),
            o => (o, false),
        }
    }
}

/// An attribute of a condition: its canonical name and, for `field[...]`, the field.
#[derive(Clone, Debug, PartialEq)]
pub struct Attr {
    /// Canonical name from the entity's registry (`status`, `field`, ...).
    pub name: &'static str,
    /// `field[...]`: the key or name as written, then the resolved field.
    pub field: Option<FieldRef>,
    pub span: Span,
}

/// The field of `field[...]`.
#[derive(Clone, Debug, PartialEq)]
pub struct FieldRef {
    /// As written (a key or a name).
    pub written: String,
    /// The field key after resolution (`None` before).
    pub key: Option<String>,
    pub field_type: Option<FieldType>,
}

/// A condition: `attr op value`, `attr in [values]` or `attr is [not] empty`.
#[derive(Clone, Debug, PartialEq)]
pub struct Cond {
    pub attr: Attr,
    pub op: Op,
    pub values: Vec<Value>,
    pub span: Span,
}

/// A value with its span.
#[derive(Clone, Debug, PartialEq)]
pub struct Value {
    pub v: Val,
    pub span: Span,
}

/// What a value means. The parser produces the first group; the resolver turns `Me`, folder
/// paths and saved queries into the second.
#[derive(Clone, Debug, PartialEq)]
pub enum Val {
    /// A string or a bare word, with `quoted` for normalization.
    Text {
        s: String,
        quoted: bool,
    },
    Int(i64),
    Float(f64),
    Bool(bool),
    /// A whole UTC day.
    Date(NaiveDate),
    /// `now()` plus this many seconds (negative: in the past).
    Now(i64),
    Me,
    /// `subtree(path|id)`: the inner value is a `Text`.
    Subtree(Box<Value>),
    /// A resolved folder: exactly it, or it and its subfolders.
    Folder {
        id: Uuid,
        subtree: bool,
    },
    /// A resolved saved query (`filter = "Name"`).
    Saved(Box<Expr>),
}

impl Val {
    /// The text of a `Text` value.
    pub fn text(&self) -> Option<&str> {
        match self {
            Val::Text { s, .. } => Some(s),
            _ => None,
        }
    }
}

const KEYWORDS: [&str; 8] = ["and", "or", "not", "in", "is", "empty", "true", "false"];

/// Whether a word is a keyword (case-insensitive).
pub fn is_keyword(w: &str) -> bool {
    KEYWORDS.iter().any(|k| k.eq_ignore_ascii_case(w))
}

/// The keywords, for the schema.
pub fn keywords() -> Vec<String> {
    KEYWORDS.iter().map(|k| k.to_string()).collect()
}

/// Writes a string literal in double quotes (`\"` and `\\` escaped).
pub fn quote(s: &str, f: &mut impl fmt::Write) -> fmt::Result {
    f.write_char('"')?;
    for c in s.chars() {
        if c == '"' || c == '\\' {
            f.write_char('\\')?;
        }
        f.write_char(c)?;
    }
    f.write_char('"')
}

/// Whether a text value of an attribute prints bare (enum values and refs) or quoted.
fn bare(attr: &str) -> bool {
    matches!(
        attr,
        "status" | "state" | "origin" | "review" | "lastStatus" | "lastSource"
    )
}

fn duration(secs: i64) -> String {
    let (sign, a) = if secs < 0 { ("-", -secs) } else { ("+", secs) };
    if a == 0 {
        return "now()".into();
    }
    let unit = if a % 86_400 == 0 {
        (a / 86_400, "d")
    } else if a % 3600 == 0 {
        (a / 3600, "h")
    } else {
        (a / 60, "m")
    };
    format!("now() {sign} {}{}", unit.0, unit.1)
}

fn float(n: f64) -> String {
    if n.fract() == 0.0 && n.abs() < 1e15 {
        format!("{}", n as i64)
    } else {
        format!("{n}")
    }
}

impl Value {
    fn write(&self, attr: &str, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.v {
            Val::Text { s, .. } => {
                if bare(attr) && crate::tm::query::lexer::is_word(s) && !is_keyword(s) {
                    f.write_str(s)
                } else {
                    quote(s, f)
                }
            }
            Val::Int(n) => match attr {
                "requirement" => write!(f, "REQ-{n}"),
                "sharedStep" => write!(f, "SS-{n}"),
                _ => write!(f, "{n}"),
            },
            Val::Float(n) => f.write_str(&float(*n)),
            Val::Bool(b) => write!(f, "{b}"),
            Val::Date(d) => write!(f, "{}", d.format("%Y-%m-%d")),
            Val::Now(s) => f.write_str(&duration(*s)),
            Val::Me => f.write_str("me()"),
            Val::Subtree(v) => {
                f.write_str("subtree(")?;
                v.write("", f)?;
                f.write_str(")")
            }
            Val::Folder { id, subtree } => {
                if *subtree {
                    write!(f, "subtree(\"{id}\")")
                } else {
                    write!(f, "\"{id}\"")
                }
            }
            Val::Saved(_) => f.write_str("\"\""),
        }
    }
}

impl fmt::Display for Cond {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.attr.name)?;
        if let Some(fr) = &self.attr.field {
            f.write_str("[")?;
            quote(&fr.written, f)?;
            f.write_str("]")?;
        }
        write!(f, " {}", self.op.as_str())?;
        match self.op {
            Op::IsEmpty | Op::IsNotEmpty => Ok(()),
            Op::In | Op::NotIn
                if self.values.len() == 1 && matches!(self.values[0].v, Val::Subtree(_)) =>
            {
                f.write_str(" ")?;
                self.values[0].write(self.attr.name, f)
            }
            Op::In | Op::NotIn => {
                f.write_str(" [")?;
                for (i, v) in self.values.iter().enumerate() {
                    if i > 0 {
                        f.write_str(", ")?;
                    }
                    v.write(self.attr.name, f)?;
                }
                f.write_str("]")
            }
            _ => {
                f.write_str(" ")?;
                match self.values.first() {
                    Some(v) => v.write(self.attr.name, f),
                    None => Ok(()),
                }
            }
        }
    }
}

impl Expr {
    fn prec(&self) -> u8 {
        match self {
            Expr::Or(_) => 1,
            Expr::And(_) => 2,
            Expr::Not(_) => 3,
            Expr::Cond(_) => 4,
        }
    }

    fn write(&self, f: &mut fmt::Formatter<'_>, parent: u8) -> fmt::Result {
        let paren = self.prec() < parent;
        if paren {
            f.write_str("(")?;
        }
        match self {
            Expr::Or(xs) | Expr::And(xs) => {
                let (sep, p) = if matches!(self, Expr::Or(_)) {
                    (" or ", 1)
                } else {
                    (" and ", 2)
                };
                for (i, x) in xs.iter().enumerate() {
                    if i > 0 {
                        f.write_str(sep)?;
                    }
                    x.write(f, p + 1)?;
                }
            }
            Expr::Not(x) => {
                f.write_str("not ")?;
                x.write(f, 3)?;
            }
            Expr::Cond(c) => write!(f, "{c}")?,
        }
        if paren {
            f.write_str(")")?;
        }
        Ok(())
    }

    /// Visits every condition.
    pub fn each_cond<'a>(&'a self, out: &mut impl FnMut(&'a Cond)) {
        match self {
            Expr::And(xs) | Expr::Or(xs) => xs.iter().for_each(|x| x.each_cond(out)),
            Expr::Not(x) => x.each_cond(out),
            Expr::Cond(c) => {
                out(c);
                for v in &c.values {
                    if let Val::Saved(e) = &v.v {
                        e.each_cond(out);
                    }
                }
            }
        }
    }
}

impl fmt::Display for Expr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.write(f, 0)
    }
}

impl Query {
    /// The normalized text (empty for an empty query).
    pub fn normalized(&self) -> String {
        self.root
            .as_ref()
            .map(|r| r.to_string())
            .unwrap_or_default()
    }

    /// Whether any condition (also inside saved queries) uses the attribute.
    pub fn mentions(&self, attr: &str) -> bool {
        let mut found = false;
        if let Some(r) = &self.root {
            r.each_cond(&mut |c| found |= c.attr.name == attr);
        }
        found
    }

    /// Conditions, counting those of inlined saved queries.
    pub fn conditions(&self) -> usize {
        let mut n = 0;
        if let Some(r) = &self.root {
            r.each_cond(&mut |_| n += 1);
        }
        n
    }
}
