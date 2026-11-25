//! Case versions: a full zstd snapshot (`codec.rs`) per content change, with the content keys
//! that changed, listed newest first; restoring an old version writes a new one
//! (`source = restore`). Old versions are pruned per project by the retention settings
//! `caseVersionsKeep` / `caseVersionsDays` ([`Store::prune_case_versions`]).

use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use sqlx::{PgConnection, Row};
use uuid::Uuid;

use super::cases::{CaseCtx, Commit, commit, lock_case_by_number};
use super::lookup::case_by_number;
use super::{
    CaseRow, CaseSnapshot, CaseVersion, CaseVersionSummary, TmError, TmResult, VersionSource,
};
use crate::Store;

/// The content keys a version can change, in display order (`body` is split into its keys).
pub const CONTENT_KEYS: &[&str] = &[
    "name",
    "status",
    "folderId",
    "description",
    "preconditions",
    "steps",
    "parameters",
    "links",
    "fields",
    "tags",
];

/// The keys version 1 of a case lists as changed: every content key, `parameters` only when
/// the case has them.
pub fn initial_keys(snap: &CaseSnapshot) -> Vec<String> {
    CONTENT_KEYS
        .iter()
        .filter(|k| **k != "parameters" || snap.body.parameters.is_some())
        .map(|k| k.to_string())
        .collect()
}

/// The content of a case as a snapshot.
pub fn snapshot_of(row: &CaseRow) -> CaseSnapshot {
    CaseSnapshot {
        name: row.name.clone(),
        status: row.status,
        folder_id: row.folder_id,
        body: row.body.clone(),
        fields: row.fields.clone(),
        tags: row.tags.clone(),
    }
}

/// The content keys whose values differ, in [`CONTENT_KEYS`] order.
pub fn changed_keys(old: &CaseSnapshot, new: &CaseSnapshot) -> Vec<String> {
    let diff = [
        old.name != new.name,
        old.status != new.status,
        old.folder_id != new.folder_id,
        old.body.description != new.body.description,
        old.body.preconditions != new.body.preconditions,
        old.body.steps != new.body.steps,
        old.body.parameters != new.body.parameters,
        old.body.links != new.body.links,
        old.fields != new.fields,
        old.tags != new.tags,
    ];
    CONTENT_KEYS
        .iter()
        .zip(diff)
        .filter(|(_, d)| *d)
        .map(|(k, _)| k.to_string())
        .collect()
}

/// Writes one version row.
#[allow(clippy::too_many_arguments)]
pub async fn insert_version(
    conn: &mut PgConnection,
    project_id: Uuid,
    case_id: Uuid,
    version: i32,
    source: VersionSource,
    launch_id: Option<Uuid>,
    changed: &[String],
    snapshot: &CaseSnapshot,
    by: &str,
) -> anyhow::Result<()> {
    sqlx::query(
        "insert into case_versions (case_id, version, project_id, source, launch_id, changed, snapshot_z, created_by) \
         values ($1, $2, $3, $4, $5, $6, $7, $8)",
    )
    .bind(case_id)
    .bind(version)
    .bind(project_id)
    .bind(source.as_str())
    .bind(launch_id)
    .bind(changed)
    .bind(crate::codec::encode(snapshot)?)
    .bind(crate::no_nul(by))
    .execute(&mut *conn)
    .await?;
    Ok(())
}

fn summary(r: &sqlx::postgres::PgRow) -> sqlx::Result<CaseVersionSummary> {
    let source: String = r.try_get("source")?;
    Ok(CaseVersionSummary {
        version: r.try_get("version")?,
        source: VersionSource::parse(&source).unwrap_or(VersionSource::Ui),
        changed: r.try_get("changed")?,
        created_by: r.try_get("created_by")?,
        created_at: r.try_get::<DateTime<Utc>, _>("created_at")?,
        launch_id: r.try_get("launch_id")?,
    })
}

/// Versions of a case, newest first, below `before` (a version number).
pub async fn list_versions(
    conn: &mut PgConnection,
    case_id: Uuid,
    before: Option<i32>,
    limit: i64,
) -> sqlx::Result<Vec<CaseVersionSummary>> {
    let rows = sqlx::query(
        "select version, source, changed, created_by, created_at, launch_id from case_versions \
         where case_id = $1 and ($2::int is null or version < $2) order by version desc limit $3",
    )
    .bind(case_id)
    .bind(before)
    .bind(limit)
    .fetch_all(&mut *conn)
    .await?;
    rows.iter().map(summary).collect()
}

/// One version with its snapshot.
pub async fn get_version(
    conn: &mut PgConnection,
    case_id: Uuid,
    version: i32,
) -> anyhow::Result<Option<CaseVersion>> {
    let Some(r) = sqlx::query(
        "select version, source, changed, created_by, created_at, launch_id, snapshot_z from case_versions \
         where case_id = $1 and version = $2",
    )
    .bind(case_id)
    .bind(version)
    .fetch_optional(&mut *conn)
    .await?
    else {
        return Ok(None);
    };
    let z: Vec<u8> = r.try_get("snapshot_z")?;
    Ok(Some(CaseVersion {
        summary: summary(&r)?,
        snapshot: crate::codec::decode(&z)?,
    }))
}

/// The source of the newest version of a case (for 412 details).
pub async fn last_source(conn: &mut PgConnection, case_id: Uuid) -> sqlx::Result<Option<String>> {
    sqlx::query_scalar(
        "select source from case_versions where case_id = $1 order by version desc limit 1",
    )
    .bind(case_id)
    .fetch_optional(&mut *conn)
    .await
}

impl Store {
    /// Versions of the case with number `n`, newest first; `next` is the cursor of the next page.
    pub async fn case_versions(
        &self,
        project_id: Uuid,
        n: i64,
        before: Option<&str>,
        limit: i64,
    ) -> TmResult<(Vec<CaseVersionSummary>, Option<String>)> {
        let before = match before {
            None => None,
            Some(b) => Some(
                b.parse::<i32>()
                    .ok()
                    .filter(|v| *v > 0)
                    .ok_or_else(|| TmError::bad_request("invalid cursor"))?,
            ),
        };
        let mut conn = self.pool.acquire().await?;
        let case = case_by_number(&mut conn, project_id, n)
            .await?
            .ok_or(TmError::NotFound("case"))?;
        let items = list_versions(&mut conn, case.id, before, limit + 1).await?;
        let more = items.len() as i64 > limit;
        let items: Vec<_> = items.into_iter().take(limit as usize).collect();
        let next = more
            .then(|| items.last().map(|v| v.version.to_string()))
            .flatten();
        Ok((items, next))
    }

    /// Version `v` of the case with number `n`.
    pub async fn case_version(&self, project_id: Uuid, n: i64, v: i32) -> TmResult<CaseVersion> {
        let mut conn = self.pool.acquire().await?;
        let case = case_by_number(&mut conn, project_id, n)
            .await?
            .ok_or(TmError::NotFound("case"))?;
        get_version(&mut conn, case.id, v)
            .await?
            .ok_or(TmError::NotFound("version"))
    }

    /// Makes the content of version `v` current as a new version (`source = restore`). Values
    /// that code owns on an automated case stay as they are; a folder that no longer exists
    /// becomes the root. Attachments the old version references are still linked to the case
    /// (`case_attachments` rows live as long as the case).
    pub async fn restore_case_version(
        &self,
        ctx: &CaseCtx<'_>,
        n: i64,
        v: i32,
        expected_revision: i64,
    ) -> TmResult<CaseRow> {
        let mut tx = self.pool.begin().await?;
        let cur = lock_case_by_number(&mut tx, ctx.project_id, n, Some(expected_revision)).await?;
        let old = get_version(&mut tx, cur.id, v)
            .await?
            .ok_or(TmError::NotFound("version"))?;
        let mut snap = old.snapshot;
        if let Some(f) = snap.folder_id
            && super::folders::folder(&mut tx, ctx.project_id, f)
                .await?
                .is_none()
        {
            snap.folder_id = None;
        }
        if cur.automated {
            let owned = super::cases::code_owned_keys(&mut tx, ctx.project_id).await?;
            super::cases::keep_code_owned(&cur, &mut snap, &owned);
        }
        super::shared_steps::validate_refs(&mut tx, ctx, &snap.body, Some(&cur)).await?;
        let state =
            super::cases::state_for_status(&mut tx, ctx, &cur, snap.status, VersionSource::Restore)
                .await?;
        super::cases::write_state(&mut tx, cur.id, state.as_deref()).await?;
        let row = commit(
            &mut tx,
            ctx,
            &cur,
            snap,
            Commit {
                source: VersionSource::Restore,
                launch_id: None,
                force_revision: false,
            },
        )
        .await?;
        tx.commit().await?;
        Ok(row)
    }

    /// Deletes case and shared step versions beyond each project's retention
    /// (`retention.caseVersionsKeep`, `retention.caseVersionsDays`; projects with neither are
    /// skipped; shared steps: `shared_steps::prune_versions`), walking cases by id
    /// in chunks of 500 and deleting at most `batch` versions per statement, until `budget`
    /// is spent (the next run goes on). Never deleted: the current version of a case, versions
    /// a run item or an earlier execution was run against, and on cases made from code the
    /// newest version that was not written by code (code sync checks for it), and a version
    /// replaced less than an hour ago (a launch that is being started may still record it for
    /// its run items). Projects are visited starting at `start` (modulo their number), so a
    /// run that spends its budget on the first projects does not starve the others. Returns
    /// the number of versions deleted.
    pub async fn prune_case_versions(
        &self,
        budget: Duration,
        batch: i64,
        start: usize,
    ) -> anyhow::Result<u64> {
        const CHUNK: i64 = 500;
        let started = Instant::now();
        let mut deleted = 0u64;
        let mut projects = self.all_project_settings().await?;
        if !projects.is_empty() {
            let k = start % projects.len();
            projects.rotate_left(k);
        }
        for (project_id, _, settings) in projects {
            let r = &settings.retention;
            if r.case_versions_keep == 0 && r.case_versions_days == 0 {
                continue;
            }
            loop {
                if started.elapsed() > budget {
                    return Ok(deleted);
                }
                let n = super::shared_steps::prune_versions(
                    &self.pool,
                    project_id,
                    r.case_versions_keep,
                    r.case_versions_days,
                    batch.max(1),
                )
                .await?;
                deleted += n;
                if (n as i64) < batch.max(1) {
                    break;
                }
            }
            let mut after = Uuid::nil();
            loop {
                if started.elapsed() > budget {
                    return Ok(deleted);
                }
                let ids: Vec<Uuid> = sqlx::query_scalar(
                    "select id from cases where project_id = $1 and id > $2 order by id limit $3",
                )
                .bind(project_id)
                .bind(after)
                .bind(CHUNK)
                .fetch_all(&self.pool)
                .await?;
                let Some(last) = ids.last() else {
                    break;
                };
                after = *last;
                loop {
                    let n = sqlx::query(PRUNE_SQL)
                        .bind(&ids)
                        .bind(project_id)
                        .bind(r.case_versions_keep as i32)
                        .bind(r.case_versions_days as i32)
                        .bind(batch.max(1))
                        .execute(&self.pool)
                        .await?
                        .rows_affected();
                    deleted += n;
                    if (n as i64) < batch.max(1) || started.elapsed() > budget {
                        break;
                    }
                }
                if (ids.len() as i64) < CHUNK {
                    break;
                }
            }
        }
        Ok(deleted)
    }
}

/// One bounded delete of prunable versions of a chunk of cases (`$1` ids, `$2` project,
/// `$3` keep, `$4` days, `$5` batch).
const PRUNE_SQL: &str = "with d as (\
       select v.case_id, v.version from cases c join case_versions v on v.case_id = c.id \
       where c.id = any($1) and c.project_id = $2 and v.version < c.version \
         and ($3 = 0 or v.version <= c.version - $3) \
         and ($4 = 0 or v.created_at < now() - make_interval(days => $4)) \
         and not exists (select 1 from run_items r where r.case_id = v.case_id and r.case_version = v.version) \
         and not exists (select 1 from run_items r join run_item_executions e on e.run_item_id = r.id \
           where r.case_id = v.case_id and e.case_version = v.version) \
         and not exists (select 1 from case_versions n where n.case_id = v.case_id and n.version > v.version \
           and n.created_at > now() - interval '1 hour') \
         and not (c.origin = 'code' and v.source <> 'code' and v.version = \
           (select max(w.version) from case_versions w where w.case_id = v.case_id and w.source <> 'code')) \
       limit $5) \
     delete from case_versions v using d where v.case_id = d.case_id and v.version = d.version";
