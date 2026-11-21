//! Launches: numbered per project, idempotent by external id, counts denormalized.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::postgres::PgRow;
use sqlx::{Postgres, QueryBuilder, Row};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    Attribute, Counts, Launch, LaunchCi, LaunchEnvironment, LaunchVcs, Page, Store, UploadCounts,
    json_no_nul, no_nul,
};

/// Body of `POST /api/v1/projects/{key}/launches`.
#[derive(Clone, Debug, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LaunchStart {
    /// Client key: starting a launch with an external id that exists returns that launch.
    pub external_id: Option<String>,
    pub name: String,
    pub attributes: Option<Vec<Attribute>>,
    pub ci: Option<LaunchCi>,
    pub vcs: Option<LaunchVcs>,
    pub environment: Option<LaunchEnvironment>,
    pub debug: Option<bool>,
}

/// Filters of the launch list.
#[derive(Clone, Debug, Default)]
pub struct LaunchFilter {
    pub branch: Option<String>,
    pub environment: Option<String>,
    pub status: Option<String>,
    pub attr: Option<(String, String)>,
    pub q: Option<String>,
    pub debug: bool,
    pub before: Option<i64>,
    pub limit: i64,
}

pub(crate) const LAUNCH_SELECT: &str = "select l.id, l.project_id, l.number, l.name, l.status, l.debug, \
    l.external_id, l.created_at, l.started_at, l.finished_at, l.first_result_at, l.last_result_at, \
    l.ci, l.vcs, l.environment, l.attributes, l.count_total, l.count_passed, l.count_failed, \
    l.count_broken, l.count_skipped, l.count_unknown, l.count_flaky, l.count_muted, l.important, \
    coalesce(u.queued, 0) as uploads_queued, coalesce(u.processing, 0) as uploads_processing, \
    coalesce(u.failed, 0) as uploads_failed \
    from launches l left join lateral (select count(*) filter (where status = 'queued') as queued, \
    count(*) filter (where status = 'processing') as processing, \
    count(*) filter (where status = 'failed') as failed from uploads where launch_id = l.id) u on true ";

fn json_opt<T: serde::de::DeserializeOwned>(
    r: &PgRow,
    col: &str,
) -> Result<Option<T>, sqlx::Error> {
    let v: Option<serde_json::Value> = r.try_get(col)?;
    Ok(v.and_then(|v| serde_json::from_value(v).ok()))
}

pub(crate) fn launch_row(r: &PgRow) -> Result<Launch, sqlx::Error> {
    let started_at: DateTime<Utc> = r.try_get("started_at")?;
    let finished_at: Option<DateTime<Utc>> = r.try_get("finished_at")?;
    let first: Option<DateTime<Utc>> = r.try_get("first_result_at")?;
    let last: Option<DateTime<Utc>> = r.try_get("last_result_at")?;
    let duration_ms = match (first, last, finished_at) {
        (Some(a), Some(b), _) if b >= a => Some((b - a).num_milliseconds()),
        (_, _, Some(f)) => Some((f - started_at).num_milliseconds().max(0)),
        _ => None,
    };
    let c = |n: &str| -> Result<i64, sqlx::Error> { Ok(r.try_get::<i32, _>(n)? as i64) };
    let ci: Option<LaunchCi> = json_opt(r, "ci")?;
    Ok(Launch {
        id: r.try_get("id")?,
        project_id: r.try_get("project_id")?,
        number: r.try_get("number")?,
        name: r.try_get("name")?,
        status: r.try_get("status")?,
        debug: r.try_get("debug")?,
        external_id: r.try_get("external_id")?,
        created_at: r.try_get("created_at")?,
        started_at,
        finished_at,
        duration_ms,
        ci: ci.clone(),
        vcs: json_opt::<LaunchVcs>(r, "vcs")?.map(|v| {
            let provider = ci.as_ref().and_then(|c: &LaunchCi| c.provider.clone());
            v.with_commit_url(provider.as_deref())
        }),
        environment: json_opt(r, "environment")?,
        attributes: json_opt(r, "attributes")?.unwrap_or_default(),
        counts: Counts {
            total: c("count_total")?,
            passed: c("count_passed")?,
            failed: c("count_failed")?,
            broken: c("count_broken")?,
            skipped: c("count_skipped")?,
            unknown: c("count_unknown")?,
            flaky: c("count_flaky")?,
            muted: c("count_muted")?,
        },
        uploads: UploadCounts {
            queued: r.try_get("uploads_queued")?,
            processing: r.try_get("uploads_processing")?,
            failed: r.try_get("uploads_failed")?,
        },
        important: r.try_get("important")?,
    })
}

/// Escapes `%`, `_` and `\` for `like`.
pub fn like_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        if matches!(c, '%' | '_' | '\\') {
            out.push('\\');
        }
        if c != '\0' {
            out.push(c);
        }
    }
    out
}

impl Store {
    /// Creates a launch, or returns the existing one with the same external id (`false`).
    pub async fn start_launch(
        &self,
        project: &crate::Project,
        start: &LaunchStart,
        created_by: &str,
    ) -> anyhow::Result<(Launch, bool)> {
        let project_id = project.id;
        if let Some(ext) = &start.external_id
            && let Some(l) = self.launch_by_external(project_id, ext).await?
        {
            return Ok((l, false));
        }
        let mut tx = self.pool.begin().await?;
        let number: i64 = sqlx::query_scalar(
            "update projects set launch_seq = launch_seq + 1, launch_count = launch_count + 1, \
             last_launch_at = now() where id = $1 returning launch_seq",
        )
        .bind(project_id)
        .fetch_one(&mut *tx)
        .await?;
        let to_json = |v: Option<serde_json::Value>| v.map(json_no_nul);
        let id = Uuid::now_v7();
        let inserted: Option<Uuid> = sqlx::query_scalar(
            "insert into launches (id, project_id, number, external_id, name, debug, ci, vcs, environment, \
             attributes, branch, env_key, created_by) \
             values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13) \
             on conflict (project_id, external_id) do nothing returning id",
        )
        .bind(id)
        .bind(project_id)
        .bind(number)
        .bind(start.external_id.as_deref().map(no_nul))
        .bind(no_nul(&start.name))
        .bind(start.debug.unwrap_or(false))
        .bind(to_json(start.ci.as_ref().map(serde_json::to_value).transpose()?))
        .bind(to_json(start.vcs.as_ref().map(serde_json::to_value).transpose()?))
        .bind(to_json(
            start
                .environment
                .as_ref()
                .map(serde_json::to_value)
                .transpose()?,
        ))
        .bind(json_no_nul(serde_json::to_value(
            start.attributes.clone().unwrap_or_default(),
        )?))
        .bind(start.vcs.as_ref().and_then(|v| v.branch.as_deref()).map(no_nul))
        .bind(
            start
                .environment
                .as_ref()
                .and_then(|e| e.key.as_deref())
                .map(no_nul),
        )
        .bind(no_nul(created_by))
        .fetch_optional(&mut *tx)
        .await?;
        if inserted.is_none() {
            tx.rollback().await?;
            let ext = start.external_id.as_deref().unwrap_or_default();
            let l = self
                .launch_by_external(project_id, ext)
                .await?
                .ok_or_else(|| anyhow::anyhow!("launch vanished after a conflict"))?;
            return Ok((l, false));
        }
        crate::events::emit(
            &mut tx,
            "launch.created",
            Some(&project.key),
            serde_json::json!({"id": id, "number": number}),
        )
        .await?;
        crate::events::notify_launch(&mut tx, &project.key, id).await?;
        tx.commit().await?;
        let l = self
            .launch(project_id, id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("launch vanished after insert"))?;
        Ok((l, true))
    }

    pub async fn launch_by_external(
        &self,
        project_id: Uuid,
        external_id: &str,
    ) -> anyhow::Result<Option<Launch>> {
        let r = sqlx::query(sqlx::AssertSqlSafe(format!(
            "{LAUNCH_SELECT} where l.project_id = $1 and l.external_id = $2"
        )))
        .bind(project_id)
        .bind(no_nul(external_id))
        .fetch_optional(&self.pool)
        .await?;
        Ok(r.as_ref().map(launch_row).transpose()?)
    }

    pub async fn launch(&self, project_id: Uuid, id: Uuid) -> anyhow::Result<Option<Launch>> {
        let r = sqlx::query(sqlx::AssertSqlSafe(format!(
            "{LAUNCH_SELECT} where l.project_id = $1 and l.id = $2"
        )))
        .bind(project_id)
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(r.as_ref().map(launch_row).transpose()?)
    }

    pub async fn launches(
        &self,
        project_id: Uuid,
        f: &LaunchFilter,
    ) -> anyhow::Result<Page<Launch>> {
        let mut qb: QueryBuilder<Postgres> = QueryBuilder::new(LAUNCH_SELECT);
        qb.push("where l.project_id = ").push_bind(project_id);
        qb.push(" and l.debug = ").push_bind(f.debug);
        if let Some(b) = &f.branch {
            qb.push(" and l.branch = ").push_bind(no_nul(b));
        }
        if let Some(e) = &f.environment {
            qb.push(" and l.env_key = ").push_bind(no_nul(e));
        }
        if let Some(s) = &f.status {
            qb.push(" and l.status = ").push_bind(no_nul(s));
        }
        if let Some((k, v)) = &f.attr {
            qb.push(" and l.attributes @> ")
                .push_bind(json_no_nul(serde_json::json!([{"key": k, "value": v}])));
        }
        if let Some(q) = &f.q {
            qb.push(" and l.name ilike ")
                .push_bind(format!("%{}%", like_escape(q)));
        }
        if let Some(b) = f.before {
            qb.push(" and l.number < ").push_bind(b);
        }
        qb.push(" order by l.number desc limit ")
            .push_bind(f.limit + 1);
        let rows = qb.build().fetch_all(&self.pool).await?;
        let mut items: Vec<Launch> = rows.iter().map(launch_row).collect::<Result<_, _>>()?;
        let next = if items.len() as i64 > f.limit {
            items.truncate(f.limit as usize);
            items.last().map(|l| l.number.to_string())
        } else {
            None
        };
        Ok(Page { items, next })
    }

    /// Finishes a launch (`finished` or `interrupted`). `None` when it does not exist.
    pub async fn finish_launch(
        &self,
        project_id: Uuid,
        project_key: &str,
        id: Uuid,
        status: &str,
    ) -> anyhow::Result<Option<Launch>> {
        let mut tx = self.pool.begin().await?;
        let updated: Option<Uuid> = sqlx::query_scalar(
            "update launches set status = $3, finished_at = coalesce(finished_at, now()), updated_at = now() \
             where project_id = $1 and id = $2 returning id",
        )
        .bind(project_id)
        .bind(id)
        .bind(status)
        .fetch_optional(&mut *tx)
        .await?;
        if updated.is_none() {
            return Ok(None);
        }
        crate::events::emit(
            &mut tx,
            "launch.finished",
            Some(project_key),
            serde_json::json!({"id": id, "status": status}),
        )
        .await?;
        crate::events::notify_launch(&mut tx, project_key, id).await?;
        tx.commit().await?;
        self.launch(project_id, id).await
    }

    /// Deletes a launch and everything in it (attachments are collected later when nothing
    /// links them). Returns false when it does not exist.
    pub async fn delete_launch(
        &self,
        project_id: Uuid,
        project_key: &str,
        id: Uuid,
    ) -> anyhow::Result<bool> {
        let mut tx = self.pool.begin().await?;
        let created: Option<DateTime<Utc>> = sqlx::query_scalar(
            "select created_at from launches where project_id = $1 and id = $2 for update",
        )
        .bind(project_id)
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(created) = created else {
            return Ok(false);
        };
        sqlx::query("delete from attempts where launch_id = $1 and launch_created_at = $2")
            .bind(id)
            .bind(created)
            .execute(&mut *tx)
            .await?;
        sqlx::query("delete from results where launch_id = $1 and launch_created_at = $2")
            .bind(id)
            .bind(created)
            .execute(&mut *tx)
            .await?;
        sqlx::query("delete from launches where id = $1")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        sqlx::query(
            "update projects set launch_count = greatest(launch_count - 1, 0), updated_at = now() where id = $1",
        )
        .bind(project_id)
        .execute(&mut *tx)
        .await?;
        crate::events::emit(
            &mut tx,
            "launch.deleted",
            Some(project_key),
            serde_json::json!({"id": id}),
        )
        .await?;
        crate::events::notify_launch(&mut tx, project_key, id).await?;
        tx.commit().await?;
        Ok(true)
    }
}
