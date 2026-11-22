//! Case versions: a full zstd snapshot (`codec.rs`) per content change, with the content keys
//! that changed, listed newest first; restoring an old version writes a new one
//! (`source = restore`).

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
    "links",
    "fields",
    "tags",
];

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
}
