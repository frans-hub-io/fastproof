//! FPQL compile (§8): the resolver turns names into ids (folders, fields, saved queries,
//! `me()`), then the compiler appends SQL fragments on cases aliased `c` with `push_bind` only:
//! no value of a query ever becomes SQL text.
//!
//! Every condition is two-valued (nullable columns are guarded), so `not` and `!=` match
//! exactly the cases the positive form does not.

use std::future::Future;
use std::pin::Pin;

use chrono::{DateTime, NaiveDate, Utc};
use serde_json::json;
use sqlx::{PgConnection, Postgres, QueryBuilder};
use uuid::Uuid;

use super::ast::{Cond, Expr, FieldRef, Op, Query, Span, Val, Value};
use super::parser::{PErr, date, parse_query};
use super::{MAX_EXPANDED_CONDITIONS, MAX_FILTER_NESTING, QueryCtx};
use crate::launches::like_escape;
use crate::no_nul;
use crate::tm::fields::field_defs;
use crate::tm::{CaseFieldDef, FieldType, QueryEntity, TmError, TmResult};

/// Path segments of a folder path, at most.
const MAX_PATH_SEGMENTS: usize = 32;
/// Folder lookups of one query (path segments of all its folders), at most.
pub const MAX_FOLDER_LOOKUPS: usize = 256;
/// Values of a query with its saved queries inlined, at most (bounds the bound parameters).
pub const MAX_EXPANDED_VALUES: usize = 2000;

/// What the resolver needs besides the query.
struct Resolver<'a> {
    project_id: Uuid,
    ctx: &'a QueryCtx,
    defs: Option<Vec<CaseFieldDef>>,
    /// Saved queries being inlined (cycle detection).
    stack: Vec<(Uuid, String)>,
    /// Conditions counted so far, inlined ones included.
    budget: usize,
    /// Values counted so far, inlined ones included.
    values: usize,
    /// Folder rows looked up so far.
    lookups: usize,
}

type BoxFut<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// A resolution failure: a query error at a span, or a database error.
enum RErr {
    Query(PErr),
    Db(TmError),
}

impl From<sqlx::Error> for RErr {
    fn from(e: sqlx::Error) -> Self {
        RErr::Db(e.into())
    }
}

fn qerr(span: Span, m: impl Into<String>) -> RErr {
    RErr::Query(PErr::new(span, m))
}

/// Resolves the names of a parsed query in place; errors are 400 `invalid_query` with the
/// position of the name.
pub async fn resolve(
    conn: &mut PgConnection,
    project_id: Uuid,
    q: &mut Query,
    ctx: &QueryCtx,
) -> TmResult<()> {
    let mut r = Resolver {
        project_id,
        ctx,
        defs: None,
        stack: Vec::new(),
        budget: 0,
        values: 0,
        lookups: 0,
    };
    let entity = q.entity;
    let res = match q.root.as_mut() {
        Some(root) => r.expr(conn, entity, root).await,
        None => Ok(()),
    };
    match res {
        Ok(()) => Ok(()),
        Err(RErr::Query(e)) => Err(super::invalid_query(e.into_query_error(&q.src))),
        Err(RErr::Db(e)) => Err(e),
    }
}

impl<'a> Resolver<'a> {
    fn expr<'b>(
        &'b mut self,
        conn: &'b mut PgConnection,
        entity: QueryEntity,
        e: &'b mut Expr,
    ) -> BoxFut<'b, Result<(), RErr>>
    where
        'a: 'b,
    {
        Box::pin(async move {
            match e {
                Expr::And(xs) | Expr::Or(xs) => {
                    for x in xs {
                        self.expr(conn, entity, x).await?;
                    }
                }
                Expr::Not(x) => self.expr(conn, entity, x).await?,
                Expr::Cond(c) => self.cond(conn, entity, c).await?,
            }
            Ok(())
        })
    }

    async fn cond(
        &mut self,
        conn: &mut PgConnection,
        entity: QueryEntity,
        c: &mut Cond,
    ) -> Result<(), RErr> {
        self.budget += 1;
        if self.budget > MAX_EXPANDED_CONDITIONS {
            return Err(qerr(
                c.span,
                format!(
                    "the query with its saved queries has more than {MAX_EXPANDED_CONDITIONS} conditions"
                ),
            ));
        }
        self.values += c.values.len();
        if self.values > MAX_EXPANDED_VALUES {
            return Err(qerr(
                c.span,
                format!(
                    "the query with its saved queries has more than {MAX_EXPANDED_VALUES} values"
                ),
            ));
        }
        if entity != QueryEntity::Cases {
            return Ok(());
        }
        match c.attr.name {
            "field" => self.field(conn, c).await?,
            "folder" => {
                for v in &mut c.values {
                    self.folder(conn, v).await?;
                }
            }
            "createdBy" | "updatedBy" => {
                for v in &mut c.values {
                    if v.v == Val::Me {
                        if self.ctx.me.is_empty() {
                            return Err(qerr(v.span, "me() needs a signed-in caller"));
                        }
                        v.v = Val::Text {
                            s: self.ctx.me.clone(),
                            quoted: true,
                        };
                    }
                }
            }
            "filter" => {
                for v in &mut c.values {
                    self.saved(conn, v).await?;
                }
            }
            _ => {}
        }
        Ok(())
    }

    async fn field(&mut self, conn: &mut PgConnection, c: &mut Cond) -> Result<(), RErr> {
        if self.defs.is_none() {
            self.defs = Some(field_defs(conn, self.project_id, true).await?);
        }
        let defs = self.defs.as_deref().unwrap_or_default();
        let Some(fr) = c.attr.field.as_mut() else {
            return Err(qerr(c.attr.span, "expected field[key]"));
        };
        let w = fr.written.trim();
        let def = defs
            .iter()
            .find(|d| d.key == w)
            .or_else(|| {
                defs.iter()
                    .filter(|d| d.name.eq_ignore_ascii_case(w))
                    .min_by_key(|d| d.archived)
            })
            .ok_or_else(|| {
                let mut e = PErr::new(c.attr.span, format!("unknown field {w:?}"));
                e.expected = defs
                    .iter()
                    .filter(|d| !d.archived)
                    .map(|d| d.key.clone())
                    .collect();
                RErr::Query(e)
            })?;
        fr.key = Some(def.key.clone());
        fr.field_type = Some(def.field_type);
        let (op, _) = c.op.positive();
        match def.field_type {
            FieldType::Number => {
                if op == Op::Contains {
                    return Err(qerr(c.span, "~ cannot be used with a number field"));
                }
                for v in &mut c.values {
                    let n =
                        v.v.text()
                            .and_then(|s| s.trim().parse::<f64>().ok())
                            .filter(|n| n.is_finite() && n.abs() < 1e15)
                            .ok_or_else(|| qerr(v.span, format!("{} takes a number", def.key)))?;
                    v.v = Val::Float(n);
                }
            }
            FieldType::Date => {
                if op == Op::Contains {
                    return Err(qerr(c.span, "~ cannot be used with a date field"));
                }
                for v in &mut c.values {
                    let ok = v.v.text().and_then(date).is_some();
                    if !ok {
                        return Err(qerr(v.span, format!("{} takes a date YYYY-MM-DD", def.key)));
                    }
                }
            }
            FieldType::Select | FieldType::Multiselect | FieldType::Text => {
                if matches!(op, Op::Gt | Op::Ge | Op::Lt | Op::Le) {
                    return Err(qerr(
                        c.span,
                        "<, <=, > and >= work with number and date fields only",
                    ));
                }
            }
        }
        Ok(())
    }

    async fn folder(&mut self, conn: &mut PgConnection, v: &mut Value) -> Result<(), RErr> {
        let (text, subtree, span) = match &v.v {
            Val::Text { s, .. } => (s.clone(), false, v.span),
            Val::Subtree(inner) => match &inner.v {
                Val::Text { s, .. } => (s.clone(), true, inner.span),
                _ => return Ok(()),
            },
            _ => return Ok(()),
        };
        let id = if let Ok(id) = Uuid::parse_str(text.trim()) {
            self.lookups += 1;
            if self.lookups > MAX_FOLDER_LOOKUPS {
                return Err(qerr(
                    span,
                    format!("a query looks up at most {MAX_FOLDER_LOOKUPS} folder names"),
                ));
            }
            let found: Option<Uuid> =
                sqlx::query_scalar("select id from case_folders where project_id = $1 and id = $2")
                    .bind(self.project_id)
                    .bind(id)
                    .fetch_optional(&mut *conn)
                    .await?;
            found.ok_or_else(|| qerr(span, "unknown folder"))?
        } else {
            let segs: Vec<&str> = text
                .split('/')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .collect();
            if segs.is_empty() || segs.len() > MAX_PATH_SEGMENTS {
                return Err(qerr(
                    span,
                    "expected a folder path such as \"Payments/Cards\"",
                ));
            }
            self.lookups += segs.len();
            if self.lookups > MAX_FOLDER_LOOKUPS {
                return Err(qerr(
                    span,
                    format!("a query looks up at most {MAX_FOLDER_LOOKUPS} folder names"),
                ));
            }
            let mut parent: Option<Uuid> = None;
            for s in segs {
                let next: Option<Uuid> = sqlx::query_scalar(
                    "select id from case_folders where project_id = $1 \
                     and coalesce(parent_id, '00000000-0000-0000-0000-000000000000'::uuid) \
                       = coalesce($2, '00000000-0000-0000-0000-000000000000'::uuid) \
                     and lower(name) = lower($3)",
                )
                .bind(self.project_id)
                .bind(parent)
                .bind(no_nul(s))
                .fetch_optional(&mut *conn)
                .await?;
                parent = Some(next.ok_or_else(|| qerr(span, format!("unknown folder {text:?}")))?);
            }
            parent.ok_or_else(|| qerr(span, "unknown folder"))?
        };
        v.v = Val::Folder { id, subtree };
        Ok(())
    }

    async fn saved(&mut self, conn: &mut PgConnection, v: &mut Value) -> Result<(), RErr> {
        let Some(name) = v.v.text().map(str::to_string) else {
            return Ok(());
        };
        let row: Option<(Uuid, String)> = sqlx::query_as(
            "select id, query from saved_queries where project_id = $1 and entity = 'cases' \
             and name = $2 and ((owner_key = $3 and $3 <> '') or shared) \
             order by (owner_key = $3) desc, position, created_at limit 1",
        )
        .bind(self.project_id)
        .bind(no_nul(&name))
        .bind(no_nul(&self.ctx.owner))
        .fetch_optional(&mut *conn)
        .await?;
        let Some((id, text)) = row else {
            return Err(qerr(v.span, format!("unknown saved query {name:?}")));
        };
        if self.ctx.saving == Some(id) || self.stack.iter().any(|(s, _)| *s == id) {
            let mut chain: Vec<String> = self.stack.iter().map(|(_, n)| n.clone()).collect();
            chain.push(name.clone());
            return Err(qerr(
                v.span,
                format!("saved queries refer to each other: {}", chain.join(" → ")),
            ));
        }
        if self.stack.len() >= MAX_FILTER_NESTING {
            return Err(qerr(
                v.span,
                format!("saved queries nest at most {MAX_FILTER_NESTING} levels deep"),
            ));
        }
        let mut inner = parse_query(QueryEntity::Cases, &text).map_err(|e| {
            let m = e.into_query_error(&text).message;
            qerr(v.span, format!("saved query {name:?} is invalid: {m}"))
        })?;
        self.stack.push((id, name.clone()));
        let res = match inner.root.as_mut() {
            Some(root) => self.expr(conn, QueryEntity::Cases, root).await,
            None => Ok(()),
        };
        self.stack.pop();
        match res {
            Ok(()) => {}
            Err(RErr::Query(e)) => {
                let m = e.into_query_error(&text).message;
                return Err(qerr(v.span, format!("saved query {name:?}: {m}")));
            }
            Err(e) => return Err(e),
        }
        v.v = Val::Saved(Box::new(inner.root.unwrap_or(Expr::And(Vec::new()))));
        Ok(())
    }
}

/// Appends ` and (<query>)` for a resolved query on cases aliased `c` (nothing for an empty
/// query).
pub fn push_query(qb: &mut QueryBuilder<Postgres>, project_id: Uuid, q: &Query) {
    if let Some(root) = &q.root {
        qb.push(" and (");
        push_expr(qb, project_id, q.entity, root);
        qb.push(")");
    }
}

fn push_expr(qb: &mut QueryBuilder<Postgres>, project_id: Uuid, entity: QueryEntity, e: &Expr) {
    match e {
        Expr::And(xs) | Expr::Or(xs) => {
            let or = matches!(e, Expr::Or(_));
            if xs.is_empty() {
                qb.push(if or { "false" } else { "true" });
                return;
            }
            for (i, x) in xs.iter().enumerate() {
                if i > 0 {
                    qb.push(if or { " or " } else { " and " });
                }
                qb.push("(");
                push_expr(qb, project_id, entity, x);
                qb.push(")");
            }
        }
        Expr::Not(x) => {
            qb.push("not (");
            push_expr(qb, project_id, entity, x);
            qb.push(")");
        }
        Expr::Cond(c) => match entity {
            QueryEntity::Cases => push_case_cond(qb, project_id, c),
            QueryEntity::Requirements => super::requirements::push_condition(qb, project_id, c),
        },
    }
}

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

fn day(d: NaiveDate) -> DateTime<Utc> {
    d.and_hms_opt(0, 0, 0).unwrap_or_default().and_utc()
}

fn next_day(d: NaiveDate) -> DateTime<Utc> {
    day(d.succ_opt().unwrap_or(d))
}

/// `col <op> <time>` for dates (whole UTC days) and `now() ± offset`.
fn push_time(qb: &mut QueryBuilder<Postgres>, col: &str, op: Op, v: &Val) {
    match v {
        Val::Date(d) => {
            let (from, to) = (day(*d), next_day(*d));
            match op {
                Op::Eq => {
                    qb.push(format!("({col} >= "))
                        .push_bind(from)
                        .push(format!(" and {col} < "))
                        .push_bind(to)
                        .push(")");
                }
                Op::Gt => {
                    qb.push(format!("{col} >= ")).push_bind(to);
                }
                Op::Ge => {
                    qb.push(format!("{col} >= ")).push_bind(from);
                }
                Op::Lt => {
                    qb.push(format!("{col} < ")).push_bind(from);
                }
                _ => {
                    qb.push(format!("{col} < ")).push_bind(to);
                }
            }
        }
        Val::Now(off) => {
            let sym = match op {
                Op::Gt => ">",
                Op::Ge => ">=",
                Op::Lt => "<",
                _ => "<=",
            };
            qb.push(format!("{col} {sym} (now() + make_interval(secs => "))
                .push_bind(*off as f64)
                .push("))");
        }
        _ => {
            qb.push("false");
        }
    }
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

/// `col = any(values)` with a null guard for nullable columns.
fn push_any(qb: &mut QueryBuilder<Postgres>, col: &str, nullable: bool, values: Vec<String>) {
    if nullable {
        qb.push(format!("({col} is not null and {col} = any("))
            .push_bind(values)
            .push("))");
    } else {
        qb.push(format!("{col} = any(")).push_bind(values).push(")");
    }
}

fn contains_pattern(c: &Cond) -> String {
    format!(
        "%{}%",
        like_escape(c.values.first().and_then(|v| v.v.text()).unwrap_or(""))
    )
}

fn push_case_cond(qb: &mut QueryBuilder<Postgres>, project_id: Uuid, c: &Cond) {
    let (op, negate) = c.op.positive();
    if negate {
        qb.push("not (");
    }
    let first = c.values.first().map(|v| &v.v);
    match c.attr.name {
        "number" => {
            if matches!(op, Op::Eq | Op::In) {
                qb.push("c.number = any(").push_bind(ints(c)).push(")");
            } else {
                qb.push("c.number")
                    .push(cmp(op))
                    .push_bind(ints(c).first().copied().unwrap_or(0));
            }
        }
        "name" => match op {
            Op::Contains => {
                qb.push("c.name ilike ").push_bind(contains_pattern(c));
            }
            _ => push_any(qb, "c.name", false, texts(c)),
        },
        "text" => {
            qb.push("to_tsvector('simple', c.search_text) @@ plainto_tsquery('simple', ")
                .push_bind(texts(c).into_iter().next().unwrap_or_default())
                .push(")");
        }
        "status" => push_any(qb, "c.status", false, texts(c)),
        "origin" => push_any(qb, "c.origin", false, texts(c)),
        "state" | "review" | "lastStatus" | "lastSource" | "createdBy" | "updatedBy" => {
            let col = match c.attr.name {
                "state" => "c.state",
                "review" => "c.review_state",
                "lastStatus" => "c.last_status",
                "lastSource" => "c.last_source",
                "createdBy" => "c.created_by",
                _ => "c.updated_by",
            };
            match op {
                Op::IsEmpty => {
                    qb.push(format!("{col} is null"));
                }
                Op::Contains => {
                    qb.push(format!("({col} is not null and {col} ilike "))
                        .push_bind(contains_pattern(c))
                        .push(")");
                }
                _ => push_any(qb, col, true, texts(c)),
            }
        }
        "automated" | "aiUnreviewed" | "hasParameters" | "archived" => {
            let sql = match c.attr.name {
                "automated" => "c.automated",
                "aiUnreviewed" => "c.ai_unreviewed",
                "hasParameters" => "c.body ? 'parameters'",
                _ => "c.archived_at is not null",
            };
            if matches!(first, Some(Val::Bool(true))) {
                qb.push(sql);
            } else {
                qb.push(format!("not ({sql})"));
            }
        }
        "folder" => {
            if op == Op::IsEmpty {
                qb.push("c.folder_id is null");
            } else {
                let mut exact = Vec::new();
                let mut sub = Vec::new();
                for v in &c.values {
                    if let Val::Folder { id, subtree } = v.v {
                        if subtree {
                            sub.push(id);
                        } else {
                            exact.push(id);
                        }
                    }
                }
                qb.push("(c.folder_id is not null and (false");
                if !exact.is_empty() {
                    qb.push(" or c.folder_id = any(").push_bind(exact).push(")");
                }
                if !sub.is_empty() {
                    qb.push(
                        " or c.folder_id in (select d.id from case_folders d where d.project_id = ",
                    )
                    .push_bind(project_id)
                    .push(" and d.path_ids && ")
                    .push_bind(sub)
                    .push(")");
                }
                qb.push("))");
            }
        }
        "tag" => match op {
            Op::IsEmpty => {
                qb.push("cardinality(c.tags) = 0");
            }
            Op::Contains => {
                qb.push("exists (select 1 from unnest(c.tags) t(v) where t.v ilike ")
                    .push_bind(contains_pattern(c))
                    .push(")");
            }
            Op::In => {
                let t: Vec<String> = texts(c).iter().map(|t| t.trim().to_string()).collect();
                qb.push("c.tags && ").push_bind(t);
            }
            _ => {
                let t: Vec<String> = texts(c).iter().map(|t| t.trim().to_string()).collect();
                qb.push("c.tags @> ").push_bind(t);
            }
        },
        "field" => push_field(qb, c, op),
        "createdAt" | "updatedAt" | "lastRunAt" => {
            let col = match c.attr.name {
                "createdAt" => "c.created_at",
                "updatedAt" => "c.updated_at",
                _ => "c.last_status_at",
            };
            match (op, first) {
                (Op::IsEmpty, _) => {
                    qb.push(format!("{col} is null"));
                }
                (_, Some(v)) => {
                    qb.push(format!("({col} is not null and "));
                    push_time(qb, col, op, v);
                    qb.push(")");
                }
                _ => {
                    qb.push("false");
                }
            }
        }
        "requirement" => {
            if op == Op::IsEmpty {
                qb.push(
                    "not exists (select 1 from requirement_links l join requirements r on r.id = l.requirement_id \
                     where l.case_id = c.id and l.status = 'active' and r.archived_at is null)",
                );
            } else {
                qb.push(
                    "exists (select 1 from requirement_links l join requirements r on r.id = l.requirement_id \
                     where l.case_id = c.id and l.status = 'active' and r.archived_at is null and r.project_id = ",
                )
                .push_bind(project_id)
                .push(" and r.number = any(")
                .push_bind(ints(c))
                .push("))");
            }
        }
        "sharedStep" => {
            if op == Op::IsEmpty {
                qb.push("not exists (select 1 from shared_step_usages u where u.case_id = c.id)");
            } else {
                qb.push(
                    "exists (select 1 from shared_step_usages u join shared_steps s on s.id = u.shared_step_id \
                     where u.case_id = c.id and s.project_id = ",
                )
                .push_bind(project_id)
                .push(" and s.number = any(")
                .push_bind(ints(c))
                .push("))");
            }
        }
        "plan" => {
            let mut ids = Vec::new();
            let mut names = Vec::new();
            for t in texts(c) {
                match Uuid::parse_str(t.trim()) {
                    Ok(id) => ids.push(id),
                    Err(_) => names.push(t.trim().to_lowercase()),
                }
            }
            qb.push(
                "exists (select 1 from test_plan_items i join test_plans p on p.id = i.plan_id \
                 where i.case_id = c.id and p.project_id = ",
            )
            .push_bind(project_id)
            .push(" and (p.id = any(")
            .push_bind(ids)
            .push(") or lower(p.name) = any(")
            .push_bind(names)
            .push(")))");
        }
        "staleDays" => {
            let d = ints(c).first().copied().unwrap_or(0).clamp(0, 36_500) as i32;
            let (guard, sym) = match op {
                Op::Gt => ("c.last_trusted_at is null or", "<"),
                Op::Ge => ("c.last_trusted_at is null or", "<="),
                Op::Lt => ("c.last_trusted_at is not null and", ">"),
                _ => ("c.last_trusted_at is not null and", ">="),
            };
            qb.push(format!(
                "c.automated and ({guard} c.last_trusted_at {sym} now() - make_interval(days => "
            ))
            .push_bind(d)
            .push("))");
        }
        "filter" => match first {
            Some(Val::Saved(e)) => {
                qb.push("(");
                push_expr(qb, project_id, QueryEntity::Cases, e);
                qb.push(")");
            }
            _ => {
                qb.push("false");
            }
        },
        _ => {
            qb.push("false");
        }
    }
    if negate {
        qb.push(")");
    }
}

fn field_text(v: &Val) -> Option<String> {
    match v {
        Val::Text { s, .. } => Some(no_nul(s)),
        Val::Float(n) => Some(if n.fract() == 0.0 {
            format!("{}", *n as i64)
        } else {
            format!("{n}")
        }),
        Val::Int(n) => Some(n.to_string()),
        _ => None,
    }
}

/// `field[key]`: containment (the GIN index of `cases.fields`) for equality, as the
/// structured filter always did; ranges for number and date fields.
fn push_field(qb: &mut QueryBuilder<Postgres>, c: &Cond, op: Op) {
    let Some(FieldRef {
        key: Some(k),
        field_type,
        ..
    }) = &c.attr.field
    else {
        qb.push("false");
        return;
    };
    let k = no_nul(k);
    match op {
        Op::IsEmpty => {
            qb.push("not (c.fields ? ").push_bind(k).push(")");
        }
        Op::Eq | Op::In => {
            qb.push("(false");
            for v in c.values.iter().filter_map(|v| field_text(&v.v)) {
                qb.push(" or c.fields @> ")
                    .push_bind(json!({ k.as_str(): v }))
                    .push(" or c.fields @> ")
                    .push_bind(json!({ k.as_str(): [v] }));
                if let Ok(n) = v.parse::<f64>()
                    && n.is_finite()
                {
                    qb.push(" or c.fields @> ")
                        .push_bind(json!({ k.as_str(): n }));
                }
            }
            qb.push(")");
        }
        Op::Contains => {
            qb.push(
                "exists (select 1 from jsonb_array_elements_text(case jsonb_typeof(c.fields -> ",
            )
            .push_bind(k.clone())
            .push(") when 'array' then c.fields -> ")
            .push_bind(k.clone())
            .push(" when 'string' then jsonb_build_array(c.fields -> ")
            .push_bind(k)
            .push(") else '[]'::jsonb end) f(v) where f.v ilike ")
            .push_bind(contains_pattern(c))
            .push(")");
        }
        _ => {
            let v = c.values.first().map(|v| &v.v);
            match (field_type, v) {
                (Some(FieldType::Number), Some(Val::Float(n))) => {
                    qb.push("coalesce(case when jsonb_typeof(c.fields -> ")
                        .push_bind(k.clone())
                        .push(") = 'number' then (c.fields ->> ")
                        .push_bind(k)
                        .push(")::float8")
                        .push(cmp(op))
                        .push_bind(*n)
                        .push(" end, false)");
                }
                (Some(FieldType::Date), Some(Val::Text { s, .. })) => {
                    qb.push("coalesce(case when jsonb_typeof(c.fields -> ")
                        .push_bind(k.clone())
                        .push(") = 'string' then (c.fields ->> ")
                        .push_bind(k)
                        .push(")")
                        .push(cmp(op))
                        .push_bind(no_nul(s))
                        .push(" end, false)");
                }
                _ => {
                    qb.push("false");
                }
            }
        }
    }
}
