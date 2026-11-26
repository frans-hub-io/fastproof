//! FPQL parser (§8): recursive descent over the tokens of [`super::lexer`] into the
//! [`super::ast`], with the static checks (attribute names, operators per attribute, value
//! types, the size limits). Names that need the database are checked by the resolver.
//!
//! ```text
//! query   := or? EOF
//! or      := and ("or" and)*
//! and     := unary ("and" unary)*
//! unary   := "not" unary | "(" or ")" | cond
//! cond    := attr ( cmp value | "in" list | "not" "in" list | "is" ["not"] "empty" )
//! attr    := word | "field" "[" (string | word) "]"
//! cmp     := "=" | "!=" | "~" | "!~" | ">" | ">=" | "<" | "<="
//! list    := "[" value ("," value)* "]"
//! value   := string | ["-"] word | "now" "(" ")" [("-" | "+") word] | "me" "(" ")"
//!          | "subtree" "(" (string | word) ")"
//! ```

use chrono::NaiveDate;

use super::ast::{Attr, Cond, Expr, FieldRef, Op, Query, Span, Val, Value};
use super::attrs::{AttrSpec, Kind, find, registry};
use super::lexer::{LexError, Tok, Token, lex};
use super::{
    MAX_LIST_VALUES, MAX_QUERY_BYTES, MAX_QUERY_CONDITIONS, MAX_QUERY_DEPTH, MIN_CONTAINS_CHARS,
};
use crate::tm::{QueryEntity, QueryError};

/// A parse error before positions are converted to UTF-16.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PErr {
    pub span: Span,
    pub message: String,
    pub expected: Vec<String>,
}

impl PErr {
    pub fn new(span: Span, message: impl Into<String>) -> Self {
        Self {
            span,
            message: message.into(),
            expected: Vec::new(),
        }
    }

    fn expected(mut self, e: &[&str]) -> Self {
        self.expected = e.iter().map(|s| s.to_string()).collect();
        self
    }

    /// The API error: UTF-16 positions, the text near the problem.
    pub fn into_query_error(self, src: &str) -> QueryError {
        let clamp = |i: usize| {
            let mut i = i.min(src.len());
            while !src.is_char_boundary(i) {
                i -= 1;
            }
            i
        };
        let (s, e) = (
            clamp(self.span.start),
            clamp(self.span.end.max(self.span.start)),
        );
        let utf16 = |t: &str| t.encode_utf16().count() as u32;
        let near: String = if s < e {
            src[s..e].chars().take(40).collect()
        } else {
            let tail: Vec<char> = src[..s].chars().collect();
            tail[tail.len().saturating_sub(20)..].iter().collect()
        };
        QueryError {
            position: utf16(&src[..s]),
            length: utf16(&src[s..e]),
            expected: self.expected,
            near,
            message: self.message,
        }
    }
}

impl From<LexError> for PErr {
    fn from(e: LexError) -> Self {
        Self {
            span: e.span,
            message: e.message,
            expected: e.expected,
        }
    }
}

struct Parser {
    entity: QueryEntity,
    toks: Vec<Token>,
    pos: usize,
    conditions: usize,
}

fn word_is(t: &Tok, w: &str) -> bool {
    matches!(t, Tok::Word(x) if x.eq_ignore_ascii_case(w))
}

const CMP: [&str; 8] = ["=", "!=", "~", "!~", ">", ">=", "<", "<="];

impl Parser {
    fn peek(&self) -> &Token {
        &self.toks[self.pos.min(self.toks.len() - 1)]
    }

    fn next(&mut self) -> Token {
        let t = self.peek().clone();
        if self.pos < self.toks.len() - 1 {
            self.pos += 1;
        }
        t
    }

    /// Whether the next tokens are a function call (`word (`).
    fn peek_is_call(&self) -> bool {
        self.toks
            .get(self.pos + 1)
            .is_some_and(|t| t.tok == Tok::LParen)
    }

    /// Steps back over a token `next` returned (the end stays where it is).
    fn back(&mut self, t: &Token) {
        if t.tok != Tok::Eof {
            self.pos -= 1;
        }
    }

    fn unexpected(&self, expected: &[&str]) -> PErr {
        let t = self.peek();
        let what = if t.tok == Tok::Eof {
            "the query ends too early".to_string()
        } else {
            format!("unexpected {}", t.tok.describe())
        };
        PErr::new(t.span, what).expected(expected)
    }

    fn expect(&mut self, tok: Tok, label: &str) -> Result<Token, PErr> {
        if self.peek().tok == tok {
            Ok(self.next())
        } else {
            Err(self.unexpected(&[label]))
        }
    }

    fn deeper(&self, depth: usize, at: Span) -> Result<usize, PErr> {
        if depth + 1 > MAX_QUERY_DEPTH {
            return Err(PErr::new(
                at,
                format!("queries nest at most {MAX_QUERY_DEPTH} levels deep"),
            ));
        }
        Ok(depth + 1)
    }

    fn or(&mut self, depth: usize) -> Result<Expr, PErr> {
        let mut xs = vec![self.and(depth)?];
        while word_is(&self.peek().tok, "or") {
            self.next();
            xs.push(self.and(depth)?);
        }
        Ok(flatten(xs, true))
    }

    fn and(&mut self, depth: usize) -> Result<Expr, PErr> {
        let mut xs = vec![self.unary(depth)?];
        while word_is(&self.peek().tok, "and") {
            self.next();
            xs.push(self.unary(depth)?);
        }
        Ok(flatten(xs, false))
    }

    fn unary(&mut self, depth: usize) -> Result<Expr, PErr> {
        let t = self.peek().clone();
        if word_is(&t.tok, "not") {
            let d = self.deeper(depth, t.span)?;
            self.next();
            let inner = self.unary(d)?;
            return Ok(match inner {
                Expr::Not(x) => *x,
                x => Expr::Not(Box::new(x)),
            });
        }
        if t.tok == Tok::LParen {
            let d = self.deeper(depth, t.span)?;
            self.next();
            let e = self.or(d)?;
            self.expect(Tok::RParen, ")")?;
            return Ok(e);
        }
        if matches!(t.tok, Tok::Word(_)) && !keyword_here(&t.tok) {
            return self.cond().map(Expr::Cond);
        }
        Err(self.unexpected(&["an attribute", "(", "not"]))
    }

    fn cond(&mut self) -> Result<Cond, PErr> {
        let t = self.next();
        let Tok::Word(name) = &t.tok else {
            return Err(PErr::new(t.span, "expected an attribute"));
        };
        let Some(spec) = find(self.entity, name) else {
            let mut e = PErr::new(t.span, format!("unknown attribute {name:?}"));
            e.expected = suggestions(self.entity, name);
            return Err(e);
        };
        let mut attr = Attr {
            name: spec.name,
            field: None,
            span: t.span,
        };
        if spec.kind == Kind::Field {
            self.expect(Tok::LBracket, "[")?;
            let k = self.next();
            let written = match k.tok {
                Tok::Str(s) | Tok::Word(s) if !s.trim().is_empty() && s.len() <= 200 => s,
                _ => {
                    return Err(PErr::new(k.span, "expected a field key or name")
                        .expected(&["a field key or name"]));
                }
            };
            let close = self.expect(Tok::RBracket, "]")?;
            attr.span = t.span.to(close.span);
            attr.field = Some(FieldRef {
                written,
                key: None,
                field_type: None,
            });
        }
        let (op, op_span) = self.op()?;
        if !spec.ops.contains(&op) {
            let ops: Vec<&str> = spec.ops.iter().map(|o| o.as_str()).collect();
            return Err(PErr::new(
                op_span,
                format!("{} cannot be used with {}", op.as_str(), spec.name),
            )
            .expected(&ops));
        }
        let mut values = Vec::new();
        let mut end = op_span;
        match op {
            Op::IsEmpty | Op::IsNotEmpty => {}
            Op::In | Op::NotIn
                if matches!(self.peek().tok, Tok::Word(_)) && self.peek_is_call() =>
            {
                let v = self.value(spec, op)?;
                end = v.span;
                values.push(v);
            }
            Op::In | Op::NotIn => {
                self.expect(Tok::LBracket, "[")?;
                loop {
                    let v = self.value(spec, op)?;
                    values.push(v);
                    if values.len() > MAX_LIST_VALUES {
                        return Err(PErr::new(
                            values[values.len() - 1].span,
                            format!("a list has at most {MAX_LIST_VALUES} values"),
                        ));
                    }
                    if self.peek().tok == Tok::Comma {
                        self.next();
                        continue;
                    }
                    break;
                }
                end = self.expect(Tok::RBracket, "]")?.span;
            }
            _ => {
                let v = self.value(spec, op)?;
                end = v.span;
                values.push(v);
            }
        }
        let span = attr.span.to(end);
        self.conditions += 1;
        if self.conditions > MAX_QUERY_CONDITIONS {
            return Err(PErr::new(
                span,
                format!("a query has at most {MAX_QUERY_CONDITIONS} conditions"),
            ));
        }
        Ok(Cond {
            attr,
            op,
            values,
            span,
        })
    }

    fn op(&mut self) -> Result<(Op, Span), PErr> {
        let t = self.next();
        let op = match &t.tok {
            Tok::Eq => Op::Eq,
            Tok::Ne => Op::Ne,
            Tok::Tilde => Op::Contains,
            Tok::NotTilde => Op::NotContains,
            Tok::Gt => Op::Gt,
            Tok::Ge => Op::Ge,
            Tok::Lt => Op::Lt,
            Tok::Le => Op::Le,
            w if word_is(w, "in") => Op::In,
            w if word_is(w, "not") => {
                let n = self.peek().clone();
                if !word_is(&n.tok, "in") {
                    return Err(self.unexpected(&["in"]));
                }
                self.next();
                return Ok((Op::NotIn, t.span.to(n.span)));
            }
            w if word_is(w, "is") => {
                let n = self.next();
                if word_is(&n.tok, "empty") {
                    return Ok((Op::IsEmpty, t.span.to(n.span)));
                }
                if word_is(&n.tok, "not") {
                    let m = self.next();
                    if word_is(&m.tok, "empty") {
                        return Ok((Op::IsNotEmpty, t.span.to(m.span)));
                    }
                    return Err(PErr::new(m.span, "expected \"empty\"").expected(&["empty"]));
                }
                return Err(PErr::new(n.span, "expected \"empty\" or \"not empty\"")
                    .expected(&["empty", "not empty"]));
            }
            _ => {
                self.back(&t);
                let mut e: Vec<&str> = CMP.to_vec();
                e.extend(["in", "not in", "is empty", "is not empty"]);
                return Err(self.unexpected(&e));
            }
        };
        Ok((op, t.span))
    }

    /// A raw value: a string, a (possibly negative) word or a function call.
    fn raw(&mut self) -> Result<Raw, PErr> {
        let t = self.next();
        match t.tok {
            Tok::Str(s) => Ok(Raw::Text {
                s,
                quoted: true,
                span: t.span,
            }),
            Tok::Minus => {
                let w = self.next();
                match w.tok {
                    Tok::Word(s) => Ok(Raw::Text {
                        s: format!("-{s}"),
                        quoted: false,
                        span: t.span.to(w.span),
                    }),
                    _ => Err(PErr::new(w.span, "expected a number").expected(&["a number"])),
                }
            }
            Tok::Word(w) if self.peek().tok == Tok::LParen => {
                let lower = w.to_ascii_lowercase();
                if !matches!(lower.as_str(), "now" | "me" | "subtree") {
                    return Err(
                        PErr::new(t.span, format!("unknown function {w:?}")).expected(&[
                            "now()",
                            "me()",
                            "subtree()",
                        ]),
                    );
                }
                self.next();
                let mut arg = None;
                if lower == "subtree" {
                    let a = self.next();
                    arg = Some(match a.tok {
                        Tok::Str(s) | Tok::Word(s) => (s, a.span),
                        _ => {
                            return Err(PErr::new(a.span, "expected a folder path or id")
                                .expected(&["a folder path or id"]));
                        }
                    });
                }
                let close = self.expect(Tok::RParen, ")")?;
                let mut span = t.span.to(close.span);
                let mut offset = 0i64;
                if lower == "now" && matches!(self.peek().tok, Tok::Minus | Tok::Plus) {
                    let sign = if self.next().tok == Tok::Minus { -1 } else { 1 };
                    let d = self.next();
                    let Tok::Word(dw) = &d.tok else {
                        return Err(PErr::new(
                            d.span,
                            "expected a duration such as 7d, 12h or 30m",
                        )
                        .expected(&["7d", "12h", "30m"]));
                    };
                    offset = sign
                        * duration(dw).ok_or_else(|| {
                            PErr::new(
                                d.span,
                                "expected a duration such as 7d, 12h or 30m (at most 36500 days)",
                            )
                            .expected(&["7d", "12h", "30m"])
                        })?;
                    span = span.to(d.span);
                }
                Ok(Raw::Func {
                    name: lower,
                    arg,
                    offset,
                    span,
                })
            }
            Tok::Word(s) => Ok(Raw::Text {
                s,
                quoted: false,
                span: t.span,
            }),
            _ => {
                self.back(&t);
                Err(self.unexpected(&["a value"]))
            }
        }
    }

    fn value(&mut self, spec: &AttrSpec, op: Op) -> Result<Value, PErr> {
        let raw = self.raw()?;
        let span = raw.span();
        let bad = |m: String| PErr::new(span, m);
        let v = match (spec.kind, raw) {
            (Kind::Date, Raw::Func { name, offset, .. }) if name == "now" => Val::Now(offset),
            (Kind::User, Raw::Func { name, .. }) if name == "me" => Val::Me,
            (Kind::Folder, Raw::Func { name, arg, .. }) if name == "subtree" => {
                let (s, sp) = arg.unwrap_or_default();
                Val::Subtree(Box::new(Value {
                    v: Val::Text { s, quoted: true },
                    span: sp,
                }))
            }
            (_, Raw::Func { name, .. }) => {
                return Err(bad(format!("{name}() cannot be used with {}", spec.name)));
            }
            (kind, Raw::Text { s, quoted, .. }) => {
                text_value(spec, kind, s, quoted).map_err(|(m, e)| bad(m).expected(&e))?
            }
        };
        if matches!(op, Op::Contains | Op::NotContains)
            && let Val::Text { s, .. } = &v
            && s.trim().chars().count() < MIN_CONTAINS_CHARS
        {
            return Err(bad(format!(
                "~ needs at least {MIN_CONTAINS_CHARS} characters"
            )));
        }
        if matches!(op, Op::In | Op::NotIn | Op::Eq | Op::Ne) && matches!(v, Val::Now(_)) {
            return Err(bad("use <, <=, > or >= with now()".into()));
        }
        Ok(Value { v, span })
    }
}

/// Words that may not start a condition.
fn keyword_here(t: &Tok) -> bool {
    ["and", "or", "in", "is", "empty", "true", "false"]
        .iter()
        .any(|k| word_is(t, k))
}

fn flatten(xs: Vec<Expr>, or: bool) -> Expr {
    if xs.len() == 1 {
        return xs.into_iter().next().unwrap_or(Expr::And(Vec::new()));
    }
    let mut out = Vec::with_capacity(xs.len());
    for x in xs {
        match x {
            Expr::Or(inner) if or => out.extend(inner),
            Expr::And(inner) if !or => out.extend(inner),
            x => out.push(x),
        }
    }
    if or { Expr::Or(out) } else { Expr::And(out) }
}

fn suggestions(entity: QueryEntity, name: &str) -> Vec<String> {
    let lower = name.to_ascii_lowercase();
    let mut out: Vec<String> = registry(entity)
        .iter()
        .filter(|a| {
            let n = a.name.to_ascii_lowercase();
            n.starts_with(lower.get(..2).unwrap_or(&lower)) || n.contains(&lower)
        })
        .map(|a| a.name.to_string())
        .collect();
    if out.is_empty() {
        out = registry(entity)
            .iter()
            .map(|a| a.name.to_string())
            .collect();
    }
    out
}

enum Raw {
    Text {
        s: String,
        quoted: bool,
        span: Span,
    },
    Func {
        name: String,
        arg: Option<(String, Span)>,
        offset: i64,
        span: Span,
    },
}

impl Raw {
    fn span(&self) -> Span {
        match self {
            Raw::Text { span, .. } | Raw::Func { span, .. } => *span,
        }
    }
}

/// Seconds of `7d`, `12h` or `30m`, at most 36500 days.
fn duration(w: &str) -> Option<i64> {
    let (n, unit) = w.split_at(w.len().checked_sub(1)?);
    if n.is_empty() || n.len() > 9 || !n.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let n: i64 = n.parse().ok()?;
    let secs = match unit {
        "d" | "D" => n.checked_mul(86_400)?,
        "h" | "H" => n.checked_mul(3600)?,
        "m" | "M" => n.checked_mul(60)?,
        _ => return None,
    };
    (secs <= 36_500 * 86_400).then_some(secs)
}

/// `YYYY-MM-DD`.
pub fn date(s: &str) -> Option<NaiveDate> {
    if s.len() != 10 || s.as_bytes()[4] != b'-' || s.as_bytes()[7] != b'-' {
        return None;
    }
    NaiveDate::parse_from_str(s, "%Y-%m-%d").ok()
}

type ValErr = (String, Vec<&'static str>);

fn text_value(spec: &AttrSpec, kind: Kind, s: String, quoted: bool) -> Result<Val, ValErr> {
    Ok(match kind {
        Kind::Int { prefix } => {
            let digits = match s.split_once('-') {
                Some((p, n)) if p.eq_ignore_ascii_case(prefix) => n,
                _ => s.as_str(),
            };
            if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
                return Err((
                    format!("{} takes a number or {prefix}-<number>", spec.name),
                    vec![],
                ));
            }
            let n: i64 = digits
                .parse()
                .ok()
                .filter(|n| *n <= i64::MAX / 2)
                .ok_or_else(|| ("the number is too large".to_string(), vec![]))?;
            Val::Int(n)
        }
        Kind::Days => {
            let n: i64 = s
                .parse()
                .ok()
                .filter(|n| (0..=36_500).contains(n))
                .ok_or_else(|| (format!("{} takes 0 to 36500 days", spec.name), vec![]))?;
            Val::Int(n)
        }
        Kind::Str | Kind::Text | Kind::User | Kind::Folder | Kind::Plan | Kind::Filter => {
            if s.is_empty() && !matches!(kind, Kind::Str | Kind::User) {
                return Err((format!("{} needs a value", spec.name), vec![]));
            }
            if s.len() > 500 {
                return Err(("a value has at most 500 bytes".into(), vec![]));
            }
            Val::Text { s, quoted }
        }
        Kind::Field => {
            if s.len() > 1024 {
                return Err(("a value has at most 1024 bytes".into(), vec![]));
            }
            Val::Text { s, quoted }
        }
        Kind::Enum => match spec.values.iter().find(|v| v.eq_ignore_ascii_case(&s)) {
            Some(v) => Val::Text {
                s: v.to_string(),
                quoted: false,
            },
            None => {
                return Err((
                    format!("{} takes one of: {}", spec.name, spec.values.join(", ")),
                    spec.values.to_vec(),
                ));
            }
        },
        Kind::Key => {
            let ok = s.len() <= 32
                && s.chars().next().is_some_and(|c| c.is_ascii_lowercase())
                && s.chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
            if !ok {
                return Err((format!("{} takes a workflow state key", spec.name), vec![]));
            }
            Val::Text { s, quoted: false }
        }
        Kind::Bool => match s.to_ascii_lowercase().as_str() {
            "true" if !quoted => Val::Bool(true),
            "false" if !quoted => Val::Bool(false),
            _ => {
                return Err((
                    format!("{} takes true or false", spec.name),
                    vec!["true", "false"],
                ));
            }
        },
        Kind::Date => match date(&s) {
            Some(d) => Val::Date(d),
            None => {
                return Err((
                    format!("{} takes a date YYYY-MM-DD or now() - 7d", spec.name),
                    vec!["YYYY-MM-DD", "now()"],
                ));
            }
        },
    })
}

/// Parses a query of an entity (static checks only; see the module docs).
pub fn parse_query(entity: QueryEntity, src: &str) -> Result<Query, PErr> {
    if src.len() > MAX_QUERY_BYTES {
        let mut at = MAX_QUERY_BYTES;
        while !src.is_char_boundary(at) {
            at -= 1;
        }
        return Err(PErr::new(
            Span::new(at, src.len()),
            format!("a query has at most {MAX_QUERY_BYTES} bytes"),
        ));
    }
    let toks = lex(src)?;
    let mut p = Parser {
        entity,
        toks,
        pos: 0,
        conditions: 0,
    };
    let root = if p.peek().tok == Tok::Eof {
        None
    } else {
        Some(p.or(0)?)
    };
    if p.peek().tok != Tok::Eof {
        let next = p.peek().tok.clone();
        let expected: &[&str] = if matches!(next, Tok::RParen) {
            &["and", "or"]
        } else {
            &["and", "or", "the end of the query"]
        };
        return Err(p.unexpected(expected));
    }
    Ok(Query {
        entity,
        root,
        src: src.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn norm(s: &str) -> String {
        parse_query(QueryEntity::Cases, s).unwrap().normalized()
    }

    fn err(s: &str) -> QueryError {
        parse_query(QueryEntity::Cases, s)
            .unwrap_err()
            .into_query_error(s)
    }

    #[test]
    fn precedence_and_normalized_form() {
        assert_eq!(
            norm("Status = ACTIVE AND automated=false or not tag = legacy"),
            "status = active and automated = false or not tag = \"legacy\""
        );
        assert_eq!(
            norm("(status = draft or status = active) and (tag = a)"),
            "(status = draft or status = active) and tag = \"a\""
        );
        assert_eq!(
            norm("not (a1 = 1)".replace("a1", "number").as_str()),
            "not number = 1"
        );
        assert_eq!(norm("not not number = 1"), "number = 1");
        assert_eq!(
            norm("not (number = 1 and number = 2)"),
            "not (number = 1 and number = 2)"
        );
        assert_eq!(
            norm("number in [TC-1, 2] and requirement = req-12 or sharedStep not in [SS-3]"),
            "number in [1, 2] and requirement = REQ-12 or sharedStep not in [SS-3]"
        );
        assert_eq!(
            norm("lastRunAt < now()-14d and createdAt >= 2026-01-31 and updatedAt > now() + 90m"),
            "lastRunAt < now() - 14d and createdAt >= 2026-01-31 and updatedAt > now() + 90m"
        );
        assert_eq!(norm("lastRunAt < now()"), "lastRunAt < now()");
        assert_eq!(
            norm("folder in [subtree(Payments)] or folder not in subtree(\"a\")"),
            "folder in subtree(\"Payments\") or folder not in subtree(\"a\")"
        );
        assert_eq!(err("folder in Payments").expected, vec!["["]);
        assert_eq!(err("tag in now()").message, "now() cannot be used with tag");
        assert_eq!(
            norm("field[\"Priority\"] in [high, 'very \"high\"'] and field[severity] > -5"),
            "field[\"Priority\"] in [\"high\", \"very \\\"high\\\"\"] and field[\"severity\"] > \"-5\""
        );
        assert_eq!(
            norm("folder in [subtree(\"Payments/Cards\"), Other] and createdBy = me()"),
            "folder in [subtree(\"Payments/Cards\"), \"Other\"] and createdBy = me()"
        );
        assert_eq!(
            norm("lastStatus is empty or review is not empty"),
            "lastStatus is empty or review is not empty"
        );
        assert_eq!(norm("   "), "");
        assert_eq!(
            norm("filter = \"Regression core\" and text ~ \"login page\""),
            "filter = \"Regression core\" and text ~ \"login page\""
        );
        for q in [
            "status = active and automated = false and field[\"Priority\"] in [\"high\", \"critical\"] and folder in subtree(\"Payments/Cards\") and not tag = \"legacy\" and (lastStatus in [failed, broken] or lastRunAt < now() - 14d)",
            "requirement = REQ-12 or sharedStep = SS-3 or filter = \"Regression core\"",
            "name !~ \"a\\\\b\\\"c\" and staleDays >= 30 and hasParameters = true",
        ] {
            assert_eq!(norm(q), q);
            assert_eq!(norm(&norm(q)), norm(q));
        }
    }

    #[test]
    fn errors_have_utf16_positions() {
        let e = err("statu = active");
        assert_eq!((e.position, e.length), (0, 5));
        assert!(e.expected.contains(&"status".to_string()));
        let e = err("status = active and");
        assert_eq!((e.position, e.length), (19, 0));
        assert_eq!(e.message, "the query ends too early");
        let e = err("status ~ active");
        assert_eq!((e.position, e.length), (7, 1));
        let e = err("name = \"日本😀\" and status = nope");
        assert_eq!(e.position, 27);
        assert_eq!(e.length, 4);
        assert_eq!(e.near, "nope");
        let e = err("name ~ ab");
        assert_eq!((e.position, e.length), (7, 2));
        let e = err("(status = active");
        assert_eq!(e.expected, vec![")"]);
        let e = err("status = active)");
        assert_eq!((e.position, e.length), (15, 1));
        let e = err("number = TC-x");
        assert_eq!((e.position, e.length), (9, 4));
        let e = err("number = 99999999999999999999999");
        assert_eq!(e.message, "the number is too large");
        let e = err("lastRunAt < now() - 7y");
        assert_eq!((e.position, e.length), (20, 2));
        let e = err("createdBy = now()");
        assert_eq!((e.position, e.length), (12, 5));
        let e = err("automated = \"true\"");
        assert_eq!(e.expected, vec!["true", "false"]);
        let e = err("status in []");
        assert_eq!((e.position, e.length), (11, 1));
        let e = err("status is nothing");
        assert_eq!(e.expected, vec!["empty", "not empty"]);
        let e = err("tag = ");
        assert_eq!((e.position, e.length), (6, 0));
        let e = err("number <=");
        assert_eq!((e.position, e.length), (9, 0));
        let e = err("number");
        assert_eq!((e.position, e.length), (6, 0));
        let e = err("and status = active");
        assert_eq!((e.position, e.length), (0, 3));
    }

    #[test]
    fn limits() {
        let many = vec!["number = 1"; MAX_QUERY_CONDITIONS].join(" or ");
        assert!(parse_query(QueryEntity::Cases, &many).is_ok());
        let over = format!("{many} or number = 2");
        let e = err(&over);
        assert!(e.message.contains("64 conditions"));
        assert_eq!(e.position as usize, over.len() - "number = 2".len());
        let deep = format!("{}number = 1{}", "(".repeat(8), ")".repeat(8));
        assert!(parse_query(QueryEntity::Cases, &deep).is_ok());
        let deeper = format!("{}number = 1{}", "(".repeat(9), ")".repeat(9));
        let e = err(&deeper);
        assert_eq!((e.position, e.length), (8, 1));
        let nots = format!("{}number = 1", "not ".repeat(9));
        assert!(err(&nots).message.contains("8 levels"));
        let huge = format!("name = \"{}\"", "x".repeat(MAX_QUERY_BYTES));
        let e = err(&huge);
        assert_eq!(e.position as usize, MAX_QUERY_BYTES);
        let stack = "(".repeat(100_000);
        assert!(err(&stack).message.contains("bytes"));
        let stack = "not ".repeat(1000);
        assert!(err(&stack).message.contains("8 levels"));
    }

    #[test]
    fn requirement_entity_has_its_own_attributes() {
        assert!(
            parse_query(
                QueryEntity::Requirements,
                "title ~ \"abc\" and coverage = notrun"
            )
            .is_ok()
        );
        assert!(
            parse_query(
                QueryEntity::Requirements,
                "parent = REQ-3 or issue is empty"
            )
            .is_ok()
        );
        assert!(parse_query(QueryEntity::Requirements, "automated = true").is_err());
        assert!(parse_query(QueryEntity::Cases, "coverage = failed").is_err());
    }
}
