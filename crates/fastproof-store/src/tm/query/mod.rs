//! FPQL: the query language of cases and requirements (§8). [`lexer`] and [`parser`] turn the
//! text into an [`ast::Query`] with the static checks and limits, [`compile`] resolves names
//! with the database and appends SQL fragments with bound values only. The structured
//! [`CaseFilter`] becomes the same AST ([`from_filter`]), so there is one compiler.

pub mod ast;
pub mod attrs;
pub mod compile;
pub mod lexer;
pub mod parser;
pub mod requirements;

use sqlx::{PgConnection, Postgres, QueryBuilder};
use uuid::Uuid;

use self::ast::{Attr, Cond, Expr, FieldRef, Op, Query, Span, Val, Value};
use super::{
    CaseFilter, FieldType, QueryAttribute, QueryEntity, QueryError, QueryField, QueryFunction,
    QuerySchema, QueryValidation, ResultSource, TmError, TmResult,
};
use crate::Store;
use crate::analysis::settings::ProjectSettings;

/// Bytes of a query, at most.
pub const MAX_QUERY_BYTES: usize = 4096;
/// Conditions of a query, at most.
pub const MAX_QUERY_CONDITIONS: usize = 64;
/// Nesting of parentheses and `not`, at most.
pub const MAX_QUERY_DEPTH: usize = 8;
/// Characters of a `~` value, at least.
pub const MIN_CONTAINS_CHARS: usize = 3;
/// `POST /query/validate` counts at most this many cases.
pub const QUERY_COUNT_CAP: i64 = 10_000;
/// Values of one `in [...]` list, at most.
pub const MAX_LIST_VALUES: usize = 1000;
/// Saved queries inside saved queries (`filter = "Name"`), at most.
pub const MAX_FILTER_NESTING: usize = 4;
/// Conditions of a query with its saved queries inlined, at most.
pub const MAX_EXPANDED_CONDITIONS: usize = 4 * MAX_QUERY_CONDITIONS;
/// `statement_timeout` of every statement that runs a query.
pub const STATEMENT_TIMEOUT: &str = "5s";

/// Who runs a query.
#[derive(Clone, Debug, Default)]
pub struct QueryCtx {
    /// The caller's principal name (`me()`, compared with `createdBy`/`updatedBy`).
    pub me: String,
    /// The caller's owner key of personal saved queries (`user:<name>`, `token:<id>`, ...;
    /// never just a name, which a token may share with a user). Empty: shared ones only.
    pub owner: String,
    /// The saved query being written, so a query naming itself is a cycle.
    pub saving: Option<Uuid>,
}

impl QueryCtx {
    /// A caller without personal saved queries (`filter = "Name"` finds shared ones only).
    pub fn new(me: &str) -> Self {
        Self::caller(me, "")
    }

    /// A caller with their principal name and the owner key of their saved queries.
    pub fn caller(me: &str, owner: &str) -> Self {
        Self {
            me: me.to_string(),
            owner: owner.to_string(),
            saving: None,
        }
    }
}

/// 400 `invalid_query` with `details: QueryError`.
pub fn invalid_query(e: QueryError) -> TmError {
    TmError::Invalid {
        code: "invalid_query",
        message: e.message.clone(),
        details: serde_json::to_value(&e).ok(),
    }
}

fn not_available(entity: QueryEntity) -> TmError {
    invalid_query(QueryError {
        message: format!("{} queries are not available", entity.as_str()),
        ..Default::default()
    })
}

/// Parses a query (syntax, attributes, operators, value types, limits).
pub fn parse(entity: QueryEntity, src: &str) -> Result<Query, QueryError> {
    parser::parse_query(entity, src).map_err(|e| e.into_query_error(src))
}

/// Parses and resolves a query: what the list, bulk, exports and plans run.
pub async fn prepare(
    conn: &mut PgConnection,
    project_id: Uuid,
    entity: QueryEntity,
    src: &str,
    ctx: &QueryCtx,
) -> TmResult<Query> {
    if !attrs::available(entity) {
        return Err(not_available(entity));
    }
    let mut q = parse(entity, src).map_err(invalid_query)?;
    compile::resolve(conn, project_id, &mut q, ctx).await?;
    Ok(q)
}

/// Appends ` and (<query>)` for a resolved query on cases aliased `c`.
pub fn push_query(qb: &mut QueryBuilder<Postgres>, project_id: Uuid, q: &Query, ctx: &QueryCtx) {
    let _ = ctx;
    compile::push_query(qb, project_id, q);
}

/// `SET LOCAL statement_timeout` for the statements that run a query (inside a transaction).
pub async fn set_timeout(conn: &mut PgConnection) -> sqlx::Result<()> {
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "set local statement_timeout = '{STATEMENT_TIMEOUT}'"
    )))
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Back to the session's timeout for the writes that follow a selection.
pub async fn reset_timeout(conn: &mut PgConnection) -> sqlx::Result<()> {
    sqlx::query("set local statement_timeout to default")
        .execute(&mut *conn)
        .await?;
    Ok(())
}

/// Whether a database error is the statement timeout.
pub fn is_timeout(e: &sqlx::Error) -> bool {
    matches!(e, sqlx::Error::Database(d) if d.code().as_deref() == Some("57014"))
}

fn timeout_error(src: &str) -> QueryError {
    QueryError {
        position: 0,
        length: src.encode_utf16().count() as u32,
        expected: Vec::new(),
        near: String::new(),
        message: format!("the query ran longer than {STATEMENT_TIMEOUT}; narrow it down"),
    }
}

/// Maps the statement timeout of a query to 400 `invalid_query`, anything else to a database
/// error.
pub fn run_error(src: &str) -> impl Fn(sqlx::Error) -> TmError + '_ {
    move |e| {
        if is_timeout(&e) {
            invalid_query(timeout_error(src))
        } else {
            e.into()
        }
    }
}

fn cond(name: &'static str, op: Op, values: Vec<Val>) -> Expr {
    Expr::Cond(Cond {
        attr: Attr {
            name,
            field: None,
            span: Span::default(),
        },
        op,
        values: values
            .into_iter()
            .map(|v| Value {
                v,
                span: Span::default(),
            })
            .collect(),
        span: Span::default(),
    })
}

fn text(s: &str) -> Val {
    Val::Text {
        s: s.to_string(),
        quoted: true,
    }
}

/// The structured filter as a resolved query (folder ids and field keys as given; the caller
/// checks them with `cases::check_filter` and `cases::check_filter_folders`).
pub fn from_filter(f: &CaseFilter) -> Query {
    let mut xs = Vec::new();
    if let Some(ids) = f.folder_ids.as_ref().filter(|v| !v.is_empty()) {
        let subtree = f.recursive.unwrap_or(false);
        xs.push(cond(
            "folder",
            Op::In,
            ids.iter()
                .map(|id| Val::Folder { id: *id, subtree })
                .collect(),
        ));
    }
    if let Some(s) = f.statuses.as_ref().filter(|v| !v.is_empty()) {
        xs.push(cond(
            "status",
            Op::In,
            s.iter().map(|s| text(s.as_str())).collect(),
        ));
    }
    if let Some(a) = f.automation {
        xs.push(cond(
            "automated",
            Op::Eq,
            vec![Val::Bool(a == ResultSource::Automated)],
        ));
    }
    if let Some(fields) = &f.fields {
        for (k, values) in fields {
            let fr = Some(FieldRef {
                written: k.clone(),
                key: Some(k.clone()),
                field_type: None,
            });
            let (op, vals) = if values.is_empty() {
                (Op::IsNotEmpty, Vec::new())
            } else {
                (Op::In, values.iter().map(|v| text(v)).collect())
            };
            if let Expr::Cond(mut c) = cond("field", op, vals) {
                c.attr.field = fr;
                xs.push(Expr::Cond(c));
            }
        }
    }
    if let Some(t) = f.tags.as_ref().filter(|v| !v.is_empty()) {
        for t in t {
            xs.push(cond("tag", Op::Eq, vec![text(t.trim())]));
        }
    }
    if let Some(q) = f.q.as_deref().map(str::trim).filter(|q| !q.is_empty()) {
        let mut any = vec![
            cond("text", Op::Contains, vec![text(q)]),
            cond("name", Op::Contains, vec![text(q)]),
        ];
        let n = q
            .strip_prefix("TC-")
            .or(q.strip_prefix("tc-"))
            .unwrap_or(q)
            .parse::<i64>()
            .ok();
        if let Some(n) = n {
            any.push(cond("number", Op::Eq, vec![Val::Int(n)]));
        }
        xs.push(Expr::Or(any));
    }
    Query {
        entity: QueryEntity::Cases,
        root: match xs.len() {
            0 => None,
            1 => xs.pop(),
            _ => Some(Expr::And(xs)),
        },
        src: String::new(),
    }
}

/// Functions of FPQL.
fn functions() -> Vec<QueryFunction> {
    vec![
        QueryFunction {
            name: "now".into(),
            signature: "now() [+|-] N(d|h|m)".into(),
            description: "The current time, optionally shifted: lastRunAt < now() - 14d.".into(),
        },
        QueryFunction {
            name: "me".into(),
            signature: "me()".into(),
            description: "You: createdBy = me().".into(),
        },
        QueryFunction {
            name: "subtree".into(),
            signature: "subtree(path | id)".into(),
            description: "A folder and its subfolders: folder in subtree(\"Payments\").".into(),
        },
    ]
}

impl Store {
    /// What FPQL offers for an entity: attributes with operators and values, functions, the
    /// project's fields and the keywords.
    pub async fn query_schema(
        &self,
        project_id: Uuid,
        entity: QueryEntity,
    ) -> TmResult<QuerySchema> {
        if !attrs::available(entity) {
            return Err(not_available(entity));
        }
        let mut conn = self.pool.acquire().await?;
        let settings: Option<serde_json::Value> =
            sqlx::query_scalar("select settings from projects where id = $1")
                .bind(project_id)
                .fetch_optional(&mut *conn)
                .await?;
        let settings = ProjectSettings::from_value(settings.unwrap_or(serde_json::Value::Null));
        let states: Vec<String> = settings
            .case_workflow
            .as_ref()
            .map(|w| w.statuses.iter().map(|s| s.key.clone()).collect())
            .unwrap_or_default();
        let defs = if entity == QueryEntity::Cases {
            super::fields::field_defs(&mut conn, project_id, false).await?
        } else {
            Vec::new()
        };
        let attributes = attrs::registry(entity)
            .iter()
            .map(|a| QueryAttribute {
                name: a.name.to_string(),
                attr_type: a.ty,
                operators: a.ops.iter().map(|o| o.as_str().to_string()).collect(),
                values: if a.name == "state" && entity == QueryEntity::Cases {
                    Some(states.clone())
                } else if a.values.is_empty() {
                    None
                } else {
                    Some(a.values.iter().map(|v| v.to_string()).collect())
                },
                description: a.description.to_string(),
            })
            .collect();
        let fields =
            defs.iter()
                .map(|d| QueryField {
                    key: d.key.clone(),
                    name: d.name.clone(),
                    field_type: d.field_type,
                    values: matches!(d.field_type, FieldType::Select | FieldType::Multiselect)
                        .then(|| {
                            d.options
                                .iter()
                                .filter(|o| !o.archived)
                                .map(|o| o.value.clone())
                                .collect()
                        }),
                })
                .collect();
        Ok(QuerySchema {
            entity,
            attributes,
            functions: functions(),
            fields,
            keywords: ast::keywords(),
        })
    }

    /// Checks a query: `ok` with the normalized text and the number of matching cases (not
    /// archived unless the query names `archived`; capped at [`QUERY_COUNT_CAP`]), or the
    /// error with its position.
    pub async fn validate_query(
        &self,
        project_id: Uuid,
        entity: QueryEntity,
        src: &str,
        ctx: &QueryCtx,
    ) -> TmResult<QueryValidation> {
        if !attrs::available(entity) {
            return Err(not_available(entity));
        }
        let invalid = |error: QueryError| QueryValidation {
            ok: false,
            error: Some(error),
            ..Default::default()
        };
        let mut q = match parse(entity, src) {
            Ok(q) => q,
            Err(e) => return Ok(invalid(e)),
        };
        let normalized = q.normalized();
        let mut tx = self.pool.begin().await?;
        set_timeout(&mut tx).await?;
        match compile::resolve(&mut tx, project_id, &mut q, ctx).await {
            Ok(()) => {}
            Err(TmError::Invalid {
                code: "invalid_query",
                details: Some(d),
                ..
            }) => {
                return Ok(invalid(serde_json::from_value(d).unwrap_or_default()));
            }
            Err(e) => return Err(e),
        }
        let mut qb: QueryBuilder<Postgres> =
            QueryBuilder::new("select count(*) from (select 1 from cases c where c.project_id = ");
        qb.push_bind(project_id);
        if !q.mentions("archived") {
            qb.push(" and c.archived_at is null");
        }
        compile::push_query(&mut qb, project_id, &q);
        qb.push(" limit ")
            .push_bind(QUERY_COUNT_CAP + 1)
            .push(") x");
        let n: i64 = match qb.build_query_scalar().fetch_one(&mut *tx).await {
            Ok(n) => n,
            Err(e) if is_timeout(&e) => return Ok(invalid(timeout_error(src))),
            Err(e) => return Err(e.into()),
        };
        tx.commit().await?;
        Ok(QueryValidation {
            ok: true,
            normalized: Some(normalized),
            count: Some(n.min(QUERY_COUNT_CAP)),
            count_capped: Some(n > QUERY_COUNT_CAP),
            error: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tm::CaseStatus;

    #[test]
    fn structured_filter_prints_as_fpql() {
        let f = CaseFilter {
            statuses: Some(vec![CaseStatus::Active, CaseStatus::Draft]),
            automation: Some(ResultSource::Manual),
            tags: Some(vec![" smoke ".into(), "ui".into()]),
            fields: Some(
                [
                    ("priority".to_string(), vec!["high".to_string()]),
                    ("owner".to_string(), vec![]),
                ]
                .into_iter()
                .collect(),
            ),
            q: Some("TC-7".into()),
            ..Default::default()
        };
        assert_eq!(
            from_filter(&f).normalized(),
            "status in [active, draft] and automated = false and field[\"owner\"] is not empty \
             and field[\"priority\"] in [\"high\"] and tag = \"smoke\" and tag = \"ui\" \
             and (text ~ \"TC-7\" or name ~ \"TC-7\" or number = 7)"
        );
        assert!(from_filter(&CaseFilter::default()).root.is_none());
    }

    #[test]
    fn injection_corpus_never_reaches_sql_text() {
        let pid = Uuid::nil();
        let corpus = [
            "name = \"'; drop table cases; --\"",
            "name ~ \"%_\\\\x' or 1=1 --\"",
            "tag in [\"$1\", \"$2::text\", \"/* c */\", \"--\"]",
            "text ~ \"a' || pg_sleep(10) || '\"",
            "createdBy = \"\\\"); select 1; --\"",
            "plan = \"x') or true or ('\"",
            "status in [active, draft] and not (number in [1, 2, 3] or number > 99999999999)",
            "name = \"ʼ＇″ unicode \u{202e}rtl\"",
            "field[\"a'b\"] = \"x\"",
        ];
        let mut texts = Vec::new();
        for src in corpus {
            let q = parse(QueryEntity::Cases, src).unwrap_or_else(|e| panic!("{src}: {e:?}"));
            let mut qb: QueryBuilder<Postgres> =
                QueryBuilder::new("select 1 from cases c where true");
            compile::push_query(&mut qb, pid, &q);
            let sql = qb.sql().as_str().to_string();
            for bad in ["drop", "pg_sleep", "--", "/*", "select 1;", "or true", "'x"] {
                assert!(!sql.contains(bad), "{src} -> {sql}");
            }
            texts.push(sql);
        }
        let a = {
            let q = parse(QueryEntity::Cases, "name = \"a\"").unwrap();
            let mut qb: QueryBuilder<Postgres> = QueryBuilder::new("");
            compile::push_query(&mut qb, pid, &q);
            qb.sql().as_str().to_string()
        };
        let b = {
            let q = parse(QueryEntity::Cases, "name = \"'; drop table cases; --\"").unwrap();
            let mut qb: QueryBuilder<Postgres> = QueryBuilder::new("");
            compile::push_query(&mut qb, pid, &q);
            qb.sql().as_str().to_string()
        };
        assert_eq!(a, b, "the value never changes the SQL text");
    }
}
