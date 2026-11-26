//! Direct uploads: attachments a client puts straight into S3 through a pre-signed URL. A row
//! is issued with the declared SHA-256, size and type, claimed while the server verifies the
//! object, and finished with the attachment it became (or failed, or expired).

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use chrono::{DateTime, Utc};
use sqlx::Row;
use uuid::Uuid;

use crate::{Store, no_nul};

/// What a client declares for one attachment.
#[derive(Clone, Debug)]
pub struct NewDirectUpload {
    pub sha256: String,
    pub size: i64,
    pub content_type: String,
    pub original_name: Option<String>,
}

/// Quotas checked when uploads are issued. "Open" uploads are pending or being verified, until
/// they complete, fail or the sweep expires them.
#[derive(Clone, Copy, Debug)]
pub struct DirectLimits {
    pub max_open_per_launch: i64,
    pub max_open_per_project: i64,
    /// Declared bytes of open and completed direct uploads of one launch.
    pub max_bytes_per_launch: i64,
    /// Declared bytes of open direct uploads of one project.
    pub max_open_bytes_per_project: i64,
}

/// The outcome for one declared attachment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Issued {
    Upload {
        id: Uuid,
        expires_at: DateTime<Utc>,
    },
    /// A quota refused it: `code` names the quota.
    Refused {
        code: &'static str,
        limit: i64,
    },
}

/// A direct upload row.
#[derive(Clone, Debug)]
pub struct DirectUpload {
    pub id: Uuid,
    pub project_id: Uuid,
    pub launch_id: Uuid,
    pub sha256: String,
    pub size: i64,
    pub content_type: String,
    pub original_name: Option<String>,
    pub status: String,
    pub expires_at: DateTime<Utc>,
    pub attachment_id: Option<Uuid>,
    pub error: Option<String>,
    /// The nonce of the verification attempt that holds (or last held) the upload.
    pub claim: Option<String>,
}

/// The state a completion found.
#[derive(Clone, Debug)]
pub enum Claim {
    /// Claimed for verification by this caller, with the nonce of the attempt it took over.
    Verify {
        upload: DirectUpload,
        previous: Option<String>,
    },
    /// Already completed: the attachment id is in the row.
    Done(DirectUpload),
    /// Another call is verifying it.
    Busy,
    /// The URL expired before the upload was completed.
    Expired,
    /// Verification failed for good, with the reason.
    Failed(String),
    NotFound,
}

/// A completion is accepted this long after the URL expired (a PUT that started before the
/// expiry may finish after it).
pub const COMPLETE_GRACE: Duration = Duration::from_secs(3600);

/// The objects of a finished or expired upload that may still be in the store.
#[derive(Clone, Debug)]
pub struct Leftover {
    pub id: Uuid,
    pub claim: Option<String>,
}

const OPEN: &str = "status in ('pending', 'verifying')";

fn row(r: &sqlx::postgres::PgRow) -> anyhow::Result<DirectUpload> {
    Ok(DirectUpload {
        id: r.try_get("id")?,
        project_id: r.try_get("project_id")?,
        launch_id: r.try_get("launch_id")?,
        sha256: r.try_get("sha256")?,
        size: r.try_get("size")?,
        content_type: r.try_get("content_type")?,
        original_name: r.try_get("original_name")?,
        status: r.try_get("status")?,
        expires_at: r.try_get("expires_at")?,
        attachment_id: r.try_get("attachment_id")?,
        error: r.try_get("error")?,
        claim: r.try_get("claim")?,
    })
}

const COLUMNS: &str = "id, project_id, launch_id, sha256, size, content_type, original_name, status, \
     expires_at, attachment_id, error, claim";

impl Store {
    /// The id of this installation in direct upload keys, created on first use.
    pub async fn direct_upload_installation(&self) -> anyhow::Result<String> {
        let fresh = Uuid::new_v4().simple().to_string();
        sqlx::query(
            "insert into direct_upload_installation (id, installation) values (true, $1) \
             on conflict (id) do nothing",
        )
        .bind(&fresh)
        .execute(&self.pool)
        .await?;
        Ok(
            sqlx::query_scalar("select installation from direct_upload_installation where id")
                .fetch_one(&self.pool)
                .await?,
        )
    }

    /// Issues direct uploads for a launch, in order, while the quotas allow (checked under a
    /// per-project lock, so concurrent requests cannot overshoot them).
    pub async fn create_direct_uploads(
        &self,
        project_id: Uuid,
        launch_id: Uuid,
        created_by: &str,
        items: &[NewDirectUpload],
        ttl: Duration,
        limits: &DirectLimits,
    ) -> anyhow::Result<Vec<Issued>> {
        if items.is_empty() {
            return Ok(Vec::new());
        }
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "select pg_advisory_xact_lock(hashtextextended('fastproof:direct:' || $1::text, 7600))",
        )
        .bind(project_id)
        .execute(&mut *tx)
        .await?;
        let (open_launch, open_project, bytes, open_bytes): (i64, i64, i64, i64) =
            sqlx::query_as(sqlx::AssertSqlSafe(format!(
                "select count(*) filter (where launch_id = $2 and {OPEN}), \
                    count(*) filter (where {OPEN}), \
                    coalesce(sum(size) filter (where launch_id = $2), 0)::bigint, \
                    coalesce(sum(size) filter (where {OPEN}), 0)::bigint \
             from direct_uploads where project_id = $1 \
               and ({OPEN} or (launch_id = $2 and status = 'done'))"
            )))
            .bind(project_id)
            .bind(launch_id)
            .fetch_one(&mut *tx)
            .await?;
        let expires_at: DateTime<Utc> =
            sqlx::query_scalar("select now() + make_interval(secs => $1)")
                .bind(ttl.as_secs_f64())
                .fetch_one(&mut *tx)
                .await?;
        let (mut open_launch, mut open_project, mut bytes, mut open_bytes) =
            (open_launch, open_project, bytes, open_bytes);
        let mut out = Vec::with_capacity(items.len());
        let (mut ids, mut shas, mut sizes, mut cts, mut names) =
            (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
        for it in items {
            let refused = if bytes.saturating_add(it.size) > limits.max_bytes_per_launch {
                Some(("launch_bytes", limits.max_bytes_per_launch))
            } else if open_bytes.saturating_add(it.size) > limits.max_open_bytes_per_project {
                Some(("project_bytes", limits.max_open_bytes_per_project))
            } else if open_launch >= limits.max_open_per_launch {
                Some(("launch_open", limits.max_open_per_launch))
            } else if open_project >= limits.max_open_per_project {
                Some(("project_open", limits.max_open_per_project))
            } else {
                None
            };
            if let Some((code, limit)) = refused {
                out.push(Issued::Refused { code, limit });
                continue;
            }
            bytes += it.size;
            open_bytes += it.size;
            open_launch += 1;
            open_project += 1;
            let id = Uuid::now_v7();
            ids.push(id);
            shas.push(it.sha256.clone());
            sizes.push(it.size);
            cts.push(no_nul(&it.content_type));
            names.push(it.original_name.as_deref().map(no_nul));
            out.push(Issued::Upload { id, expires_at });
        }
        if !ids.is_empty() {
            sqlx::query(
                "insert into direct_uploads (id, project_id, launch_id, sha256, size, content_type, original_name, \
                   created_by, expires_at) \
                 select i, $2, $3, s, z, c, n, $4, $5 \
                 from unnest($1::uuid[], $6::text[], $7::bigint[], $8::text[], $9::text[]) as t(i, s, z, c, n)",
            )
            .bind(&ids)
            .bind(project_id)
            .bind(launch_id)
            .bind(no_nul(created_by))
            .bind(expires_at)
            .bind(&shas)
            .bind(&sizes)
            .bind(&cts)
            .bind(&names)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(out)
    }

    /// Claims a direct upload of the launch for verification under the attempt nonce `claim`.
    /// A claim older than `lease` is taken over (its holder died); the nonce it had is returned.
    pub async fn claim_direct_upload(
        &self,
        project_id: Uuid,
        launch_id: Uuid,
        id: Uuid,
        lease: Duration,
        claim: &str,
    ) -> anyhow::Result<Claim> {
        let claimed = sqlx::query(sqlx::AssertSqlSafe(format!(
            "with old as (select id, claim from direct_uploads where id = $1 and project_id = $2 \
               and launch_id = $3 for update) \
             update direct_uploads d set status = 'verifying', verify_started_at = now(), claim = $6 \
             from old where d.id = old.id \
               and ((d.status = 'pending' and d.expires_at > now() - make_interval(secs => $4)) \
                 or (d.status = 'verifying' and d.verify_started_at < now() - make_interval(secs => $5))) \
             returning {}, old.claim as previous",
            COLUMNS
                .split(", ")
                .map(|c| format!("d.{}", c.trim()))
                .collect::<Vec<_>>()
                .join(", ")
        )))
        .bind(id)
        .bind(project_id)
        .bind(launch_id)
        .bind(COMPLETE_GRACE.as_secs_f64())
        .bind(lease.as_secs_f64())
        .bind(claim)
        .fetch_optional(&self.pool)
        .await?;
        if let Some(r) = claimed {
            return Ok(Claim::Verify {
                upload: row(&r)?,
                previous: r.try_get("previous")?,
            });
        }
        let r = sqlx::query(sqlx::AssertSqlSafe(format!(
            "select {COLUMNS} from direct_uploads where id = $1 and project_id = $2 and launch_id = $3"
        )))
        .bind(id)
        .bind(project_id)
        .bind(launch_id)
        .fetch_optional(&self.pool)
        .await?;
        let Some(r) = r else {
            return Ok(Claim::NotFound);
        };
        let u = row(&r)?;
        Ok(match u.status.as_str() {
            "done" => Claim::Done(u),
            "verifying" => Claim::Busy,
            "failed" => Claim::Failed(u.error.unwrap_or_default()),
            _ => Claim::Expired,
        })
    }

    /// Gives a claimed upload back (the object is not there yet, or the store failed); a no-op
    /// when another attempt took it over.
    pub async fn release_direct_upload(&self, id: Uuid, claim: &str) -> anyhow::Result<()> {
        sqlx::query(
            "update direct_uploads set status = 'pending', verify_started_at = null \
             where id = $1 and status = 'verifying' and claim = $2",
        )
        .bind(id)
        .bind(claim)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Records the attachment a verified upload became; false when the attempt no longer holds
    /// the upload.
    pub async fn finish_direct_upload(
        &self,
        id: Uuid,
        claim: &str,
        attachment_id: Uuid,
    ) -> anyhow::Result<bool> {
        Ok(sqlx::query(
            "update direct_uploads set status = 'done', attachment_id = $3, finished_at = now(), error = null \
             where id = $1 and status = 'verifying' and claim = $2",
        )
        .bind(id)
        .bind(claim)
        .bind(attachment_id)
        .execute(&self.pool)
        .await?
        .rows_affected()
            == 1)
    }

    /// Fails an upload for good (the object did not match what was declared): the verifying
    /// attempt `claim`, or with `None` a pending upload (the sweep found it too large).
    pub async fn fail_direct_upload(
        &self,
        id: Uuid,
        claim: Option<&str>,
        error: &str,
    ) -> anyhow::Result<bool> {
        Ok(sqlx::query(
            "update direct_uploads set status = 'failed', error = $3, finished_at = now() \
             where id = $1 and ((status = 'verifying' and claim = $2) or (status = 'pending' and $2::text is null))",
        )
        .bind(id)
        .bind(claim)
        .bind(no_nul(error))
        .execute(&self.pool)
        .await?
        .rows_affected()
            == 1)
    }

    /// Marks up to `batch` uploads expired whose URL expired more than [`COMPLETE_GRACE`] ago
    /// and that are not being verified (or whose verification was abandoned for `lease`), and
    /// swept; returns them (their objects can be deleted).
    pub async fn expire_direct_uploads(
        &self,
        lease: Duration,
        batch: i64,
    ) -> anyhow::Result<Vec<Leftover>> {
        let rows: Vec<(Uuid, Option<String>)> = sqlx::query_as(
            "update direct_uploads set status = 'expired', finished_at = now(), swept = true where id in ( \
               select id from direct_uploads \
               where status in ('pending', 'verifying') and expires_at < now() - make_interval(secs => $1) \
                 and (status = 'pending' or verify_started_at < now() - make_interval(secs => $2)) \
               order by expires_at limit $3 for update skip locked) \
             returning id, claim",
        )
        .bind(COMPLETE_GRACE.as_secs_f64())
        .bind(lease.as_secs_f64())
        .bind(batch)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|(id, claim)| Leftover { id, claim })
            .collect())
    }

    /// Marks up to `batch` completed or failed uploads swept once their URL can no longer be
    /// used; returns them (a PUT through the URL after completion may have left an object).
    pub async fn sweep_finished_direct_uploads(&self, batch: i64) -> anyhow::Result<Vec<Leftover>> {
        let rows: Vec<(Uuid, Option<String>)> = sqlx::query_as(
            "update direct_uploads set swept = true where id in ( \
               select id from direct_uploads \
               where not swept and status in ('done', 'failed') and expires_at < now() - make_interval(secs => $1) \
               order by expires_at limit $2 for update skip locked) \
             returning id, claim",
        )
        .bind(COMPLETE_GRACE.as_secs_f64())
        .bind(batch)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|(id, claim)| Leftover { id, claim })
            .collect())
    }

    /// Up to `batch` pending uploads whose objects may still be written, least recently
    /// checked first, as `(id, declared size)`; they are marked checked now.
    pub async fn direct_uploads_to_size_check(
        &self,
        batch: i64,
    ) -> anyhow::Result<Vec<(Uuid, i64)>> {
        Ok(sqlx::query_as(
            "update direct_uploads set size_checked_at = now() where id in ( \
               select id from direct_uploads where status = 'pending' \
                 and expires_at > now() - make_interval(secs => $1) \
               order by size_checked_at nulls first, expires_at limit $2 for update skip locked) \
             returning id, size",
        )
        .bind(COMPLETE_GRACE.as_secs_f64())
        .bind(batch)
        .fetch_all(&self.pool)
        .await?)
    }

    /// The ids among `ids` whose objects must stay: uploads that can still be completed or are
    /// being verified.
    pub async fn live_direct_uploads(&self, ids: &[Uuid]) -> anyhow::Result<HashSet<Uuid>> {
        if ids.is_empty() {
            return Ok(HashSet::new());
        }
        let rows: Vec<Uuid> = sqlx::query_scalar(
            "select id from direct_uploads where id = any($1) and status in ('pending', 'verifying')",
        )
        .bind(ids)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().collect())
    }

    /// Deletes up to `batch` swept rows older than `keep`.
    pub async fn prune_direct_uploads(&self, keep: Duration, batch: i64) -> anyhow::Result<u64> {
        Ok(sqlx::query(
            "delete from direct_uploads where id in (select id from direct_uploads \
               where swept and created_at < now() - make_interval(secs => $1) limit $2)",
        )
        .bind(keep.as_secs_f64())
        .bind(batch)
        .execute(&self.pool)
        .await?
        .rows_affected())
    }

    /// A direct upload by id (tests and diagnostics).
    pub async fn direct_upload(&self, id: Uuid) -> anyhow::Result<Option<DirectUpload>> {
        let r = sqlx::query(sqlx::AssertSqlSafe(format!(
            "select {COLUMNS} from direct_uploads where id = $1"
        )))
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        r.as_ref().map(row).transpose()
    }

    /// The hashes among `sha256` whose attachments in the project are in use: linked to a
    /// launch, held by a case or referenced by a manual step result.
    pub async fn referenced_attachment_hashes(
        &self,
        project_id: Uuid,
        sha256: &[String],
    ) -> anyhow::Result<HashSet<String>> {
        if sha256.is_empty() {
            return Ok(HashSet::new());
        }
        let rows: Vec<String> = sqlx::query_scalar(
            "select a.sha256 from attachments a where a.project_id = $1 and a.sha256 = any($2) \
               and (exists (select 1 from attachment_links l where l.attachment_id = a.id) \
                 or exists (select 1 from case_attachments c where c.attachment_id = a.id) \
                 or exists (select 1 from run_step_results s where s.attachment_ids @> array[a.id]) \
                 or exists (select 1 from attachment_refs r join run_step_results s \
                   on s.attachment_ids @> array[r.id] where r.attachment_id = a.id) \
                 or exists (select 1 from run_iteration_step_results s where s.attachment_ids @> array[a.id]) \
                 or exists (select 1 from attachment_refs r join run_iteration_step_results s \
                   on s.attachment_ids @> array[r.id] where r.attachment_id = a.id))",
        )
        .bind(project_id)
        .bind(sha256)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().collect())
    }

    /// The subset of `ids` that are attachments (or references) linked to the launch:
    /// `id -> (sha256, size)`. A report uploaded by the CLI names its directly uploaded files
    /// by these ids.
    pub async fn launch_attachments(
        &self,
        project_id: Uuid,
        launch_id: Uuid,
        ids: &[Uuid],
    ) -> anyhow::Result<HashMap<Uuid, (String, i64)>> {
        if ids.is_empty() {
            return Ok(HashMap::new());
        }
        let rows: Vec<(Uuid, String, i64)> = sqlx::query_as(
            "select a.id, a.sha256, a.size from attachments a \
               join attachment_links l on l.attachment_id = a.id and l.launch_id = $2 \
             where a.project_id = $1 and a.id = any($3) \
             union all \
             select r.id, a.sha256, a.size from attachment_refs r join attachments a on a.id = r.attachment_id \
             where a.project_id = $1 and r.launch_id = $2 and r.id = any($3)",
        )
        .bind(project_id)
        .bind(launch_id)
        .bind(ids)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(|(i, s, z)| (i, (s, z))).collect())
    }
}
