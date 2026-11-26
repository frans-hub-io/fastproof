//! Storage helpers of the streaming mode: uploads processed from Kafka (no job queue), the
//! shared signing key of gateway records, rows loaded by the analytics sinks, and the
//! PostgreSQL lookup of similar failures.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use fastproof_model::Step;
use serde::{Deserialize, Serialize};
use sqlx::Row;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::ingest::LaunchRef;
use crate::uploads::{ClaimedJob, NewUpload, UploadDone};
use crate::{Store, Upload, UploadError, codec, json_no_nul, no_nul};

/// A launch as the Kafka writer needs it.
#[derive(Clone, Debug)]
pub struct WriterLaunch {
    pub launch: LaunchRef,
    pub env_key: Option<String>,
}

/// A result row for the analytics sinks.
#[derive(Clone, Debug, Serialize)]
pub struct SinkResult {
    pub id: Uuid,
    pub project: String,
    pub project_id: Uuid,
    pub launch_id: Uuid,
    pub launch_number: i64,
    pub launch_created_at: DateTime<Utc>,
    pub series_id: Uuid,
    pub test_case_id: Uuid,
    pub name: String,
    pub full_name: String,
    pub status: String,
    pub flaky_in_run: bool,
    pub attempt_count: i32,
    pub started_at: Option<DateTime<Utc>>,
    pub duration_ms: Option<i64>,
    pub message: Option<String>,
    pub fingerprint: Option<String>,
    pub branch: Option<String>,
    pub env_key: Option<String>,
    pub updated_at: DateTime<Utc>,
    /// The defect type of the current triage (none for an undecided result).
    pub defect_type_id: Option<Uuid>,
    /// The defect group of the current triage (`""` without one).
    pub defect_group: String,
    /// Who set the current triage: `user`, `ai`, `rule`, `auto` (`""` without one).
    pub defect_source: String,
    pub muted: bool,
    /// The fingerprint the result's error is merged into (its own when not merged).
    pub canonical_fingerprint: Option<String>,
    /// The suite path joined with ` / ` (`""` without one).
    pub suite: String,
}

/// A triage decision for the ClickHouse `triage` table (a `triage_decisions` row with the
/// result's test case, the launch's branch and environment and the error's catalog message).
#[derive(Clone, Debug, Serialize)]
pub struct SinkTriage {
    pub decision_id: i64,
    pub project: String,
    pub project_id: Uuid,
    pub launch_id: Uuid,
    pub launch_created_at: DateTime<Utc>,
    pub result_id: Uuid,
    pub batch_id: Uuid,
    pub fingerprint: String,
    pub via: String,
    pub source: String,
    pub by: String,
    pub at: DateTime<Utc>,
    pub before_type_id: Option<Uuid>,
    pub before_group: String,
    pub after_type_id: Option<Uuid>,
    pub after_group: String,
    pub message_norm: String,
    pub test_case_id: Uuid,
    pub branch: String,
    pub env_key: String,
    /// The batch an undo batch reverts.
    pub reverts_batch: Option<Uuid>,
}

/// Where the ClickHouse backfill of Analysis is (`analysis_state` row [`CH_BACKFILL_STATE`]).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChBackfill {
    /// The last `triage_decisions` id sent.
    #[serde(default)]
    pub decision: i64,
    /// The newest decision id when the backfill started: decisions after it reach ClickHouse
    /// through the sink.
    #[serde(default)]
    pub until: i64,
    /// The last launch whose results were re-sent (`created_at`, id).
    #[serde(default)]
    pub launch_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub launch_id: Option<Uuid>,
    #[serde(default)]
    pub decisions_done: bool,
    #[serde(default)]
    pub results_done: bool,
    #[serde(default)]
    pub decisions: u64,
    #[serde(default)]
    pub results: u64,
    /// The first rollup after the backfill recomputed `defect_daily` over the raw TTL.
    #[serde(default)]
    pub rollup_full: bool,
}

impl ChBackfill {
    pub fn done(&self) -> bool {
        self.decisions_done && self.results_done
    }
}

/// The `analysis_state` row of the ClickHouse backfill.
pub const CH_BACKFILL_STATE: &str = "ch.backfill";

/// An id of a `uuid` text in a JSON path, or null (malformed values never fail a load).
const UUID_RE: &str =
    "'^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$'";

fn uuid_of(path: &str) -> String {
    format!("case when ({path}) ~ {UUID_RE} then ({path})::uuid end")
}

/// The columns of a [`SinkTriage`] (`d` is the decision, `p` its project, `l` its launch).
fn triage_select() -> String {
    format!(
        "select d.id, p.key, d.project_id, d.launch_id, d.launch_created_at, d.result_id, d.batch_id, \
           coalesce(d.fingerprint, '') as fingerprint, d.via, d.source, d.by, d.at, d.reverts_batch, \
           {} as before_type_id, coalesce(d.before->'defect'->>'group', '') as before_group, \
           {} as after_type_id, coalesce(d.after->'defect'->>'group', '') as after_group, \
           coalesce((select f.message from failure_fingerprints f where f.project_id = d.project_id \
             and f.fingerprint = d.fingerprint), '') as message_norm, \
           coalesce((select r.test_case_id from results r where r.id = d.result_id \
             and r.launch_created_at = d.launch_created_at), '00000000-0000-0000-0000-000000000000'::uuid) \
             as test_case_id, \
           coalesce(l.branch, '') as branch, coalesce(l.env_key, '') as env_key \
         from triage_decisions d join projects p on p.id = d.project_id \
         left join launches l on l.id = d.launch_id",
        uuid_of("d.before->'defect'->>'typeId'"),
        uuid_of("d.after->'defect'->>'typeId'"),
    )
}

fn triage_row(r: &sqlx::postgres::PgRow) -> anyhow::Result<SinkTriage> {
    Ok(SinkTriage {
        decision_id: r.try_get("id")?,
        project: r.try_get("key")?,
        project_id: r.try_get("project_id")?,
        launch_id: r.try_get("launch_id")?,
        launch_created_at: r.try_get("launch_created_at")?,
        result_id: r.try_get("result_id")?,
        batch_id: r.try_get("batch_id")?,
        fingerprint: r.try_get("fingerprint")?,
        via: r.try_get("via")?,
        source: r.try_get("source")?,
        by: r.try_get("by")?,
        at: r.try_get("at")?,
        before_type_id: r.try_get("before_type_id")?,
        before_group: r.try_get("before_group")?,
        after_type_id: r.try_get("after_type_id")?,
        after_group: r.try_get("after_group")?,
        message_norm: r.try_get("message_norm")?,
        test_case_id: r.try_get("test_case_id")?,
        branch: r.try_get("branch")?,
        env_key: r.try_get("env_key")?,
        reverts_batch: r.try_get("reverts_batch")?,
    })
}

/// An attempt row with its steps for the analytics sinks.
#[derive(Clone, Debug)]
pub struct SinkAttempt {
    pub id: Uuid,
    pub project: String,
    pub project_id: Uuid,
    pub launch_id: Uuid,
    pub launch_created_at: DateTime<Utc>,
    pub result_id: Uuid,
    pub series_id: Uuid,
    pub client_key: String,
    pub seq: i64,
    pub status: String,
    pub started_at: Option<DateTime<Utc>>,
    pub duration_ms: Option<i64>,
    pub message: Option<String>,
    pub trace: Option<String>,
    pub steps: Vec<Step>,
    pub fingerprint: Option<String>,
    /// The newest frames of the trace (`fastproof_analysis::frames`).
    pub frames: Vec<String>,
    pub trace_id: Option<String>,
}

/// A similar failure.
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SimilarResult {
    pub result_id: Uuid,
    pub launch_id: Uuid,
    pub launch_number: i64,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// 1 for the same fingerprint, else the text similarity of the messages (0..1).
    pub score: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub defect: Option<serde_json::Value>,
}

/// The reference failure of a similar-failure lookup.
#[derive(Clone, Debug)]
pub struct SimilarSource {
    pub id: Uuid,
    pub project_id: Uuid,
    pub message: Option<String>,
    pub fingerprint: Option<String>,
}

/// Normalized message used for similar failures in every backend: lower case, digit runs as
/// `0`, hex runs of 8 or more as `x`, whitespace collapsed, at most 16 KiB.
pub fn normalize_message(m: &str) -> String {
    let mut out = String::with_capacity(m.len().min(16 * 1024));
    let mut chars = m.chars().peekable();
    let mut last_space = false;
    while let Some(c) = chars.next() {
        if out.len() >= 16 * 1024 {
            break;
        }
        if c.is_ascii_digit() {
            while chars.peek().is_some_and(|d| d.is_ascii_digit()) {
                chars.next();
            }
            out.push('0');
            last_space = false;
        } else if c.is_whitespace() {
            if !last_space {
                out.push(' ');
            }
            last_space = true;
        } else {
            for l in c.to_lowercase() {
                out.push(l);
            }
            last_space = false;
        }
    }
    let trimmed = out.trim();
    let mut res = String::with_capacity(trimmed.len());
    for word in trimmed.split(' ') {
        if !res.is_empty() {
            res.push(' ');
        }
        let hexish = word.len() >= 8
            && word
                .chars()
                .all(|c| c.is_ascii_hexdigit() || c == '-' || c == '0');
        res.push_str(if hexish { "x" } else { word });
    }
    res
}

/// The message hash that stands in for the fingerprint while results carry none.
pub fn message_hash(m: &str) -> String {
    use sha2::Digest;
    let d = sha2::Sha256::digest(normalize_message(m).as_bytes());
    hex::encode(&d[..8])
}

impl Store {
    /// The signing key `name`, created on first use (shared by all replicas).
    pub async fn stream_key(&self, name: &str) -> anyhow::Result<Vec<u8>> {
        let fresh: [u8; 32] = rand::random();
        sqlx::query(
            "insert into stream_keys (name, secret) values ($1, $2) on conflict (name) do nothing",
        )
        .bind(name)
        .bind(fresh.to_vec())
        .execute(&self.pool)
        .await?;
        Ok(
            sqlx::query_scalar("select secret from stream_keys where name = $1")
                .bind(name)
                .fetch_one(&self.pool)
                .await?,
        )
    }

    /// Records an upload processed from Kafka (no job row) and notifies the launch.
    pub async fn record_upload(&self, u: &NewUpload) -> anyhow::Result<Upload> {
        let mut tx = self.pool.begin().await?;
        let r = sqlx::query(
            "insert into uploads (id, project_id, launch_id, format, file_name, size_bytes, sha256, blob_key, created_by) \
             values ($1, $2, $3, $4, $5, $6, $7, $8, $9) \
             returning id, launch_id, format, status, size_bytes, counts, warnings, error, created_at, finished_at",
        )
        .bind(u.id)
        .bind(u.project_id)
        .bind(u.launch_id)
        .bind(&u.format)
        .bind(u.file_name.as_deref().map(no_nul))
        .bind(u.size_bytes)
        .bind(&u.sha256)
        .bind(&u.blob_key)
        .bind(no_nul(&u.created_by))
        .fetch_one(&mut *tx)
        .await?;
        crate::events::emit(
            &mut tx,
            "upload.queued",
            Some(&u.project_key),
            serde_json::json!({"id": u.id, "launchId": u.launch_id, "sizeBytes": u.size_bytes}),
        )
        .await?;
        crate::events::notify_upload(&mut tx, &u.project_key, u.launch_id, u.id).await?;
        tx.commit().await?;
        Ok(crate::uploads::upload_row(&r)?)
    }

    /// Marks an upload processing and returns it as a job; `None` when it is finished or gone.
    pub async fn start_upload(
        &self,
        upload_id: Uuid,
        attempt: i32,
    ) -> anyhow::Result<Option<ClaimedJob>> {
        let mut tx = self.pool.begin().await?;
        let u = sqlx::query(
            "update uploads u set status = 'processing', started_at = coalesce(u.started_at, now()) \
             from launches l, projects p where u.id = $1 and l.id = u.launch_id and p.id = u.project_id \
               and u.status in ('queued', 'processing') \
             returning u.project_id, p.key as project_key, u.launch_id, l.created_at as launch_created_at, \
             l.env_key, u.format, u.file_name, u.blob_key, u.size_bytes",
        )
        .bind(upload_id)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(u) = u else {
            tx.commit().await?;
            return Ok(None);
        };
        let job = ClaimedJob {
            job_id: Uuid::nil(),
            upload_id,
            attempt,
            max_attempts: 3,
            project_id: u.try_get("project_id")?,
            project_key: u.try_get("project_key")?,
            launch_id: u.try_get("launch_id")?,
            launch_created_at: u.try_get("launch_created_at")?,
            env_key: u.try_get("env_key")?,
            format: u.try_get("format")?,
            file_name: u.try_get("file_name")?,
            blob_key: u.try_get("blob_key")?,
            size_bytes: u.try_get("size_bytes")?,
        };
        crate::events::notify_upload(&mut tx, &job.project_key, job.launch_id, upload_id).await?;
        tx.commit().await?;
        Ok(Some(job))
    }

    /// Marks an upload processed from Kafka done; false when the upload is gone (its launch
    /// was deleted meanwhile).
    pub async fn finish_upload(&self, job: &ClaimedJob, done: &UploadDone) -> anyhow::Result<bool> {
        let counts = serde_json::to_value(&done.counts)?;
        let mut tx = self.pool.begin().await?;
        let updated = sqlx::query(
            "update uploads set status = 'done', format = coalesce($2, format), counts = $3, warnings = $4, \
             globals = $5, error = null, finished_at = now() where id = $1",
        )
        .bind(job.upload_id)
        .bind(done.format.clone())
        .bind(counts.clone())
        .bind(json_no_nul(serde_json::to_value(&done.warnings)?))
        .bind(done.globals.clone().map(json_no_nul))
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if updated == 0 {
            return Ok(false);
        }
        crate::events::emit(
            &mut tx,
            "upload.done",
            Some(&job.project_key),
            serde_json::json!({"id": job.upload_id, "launchId": job.launch_id, "counts": counts}),
        )
        .await?;
        crate::events::notify_upload(&mut tx, &job.project_key, job.launch_id, job.upload_id)
            .await?;
        crate::events::notify_launch(&mut tx, &job.project_key, job.launch_id).await?;
        tx.commit().await?;
        Ok(true)
    }

    /// Fails an upload processed from Kafka.
    pub async fn fail_upload(&self, job: &ClaimedJob, error: &UploadError) -> anyhow::Result<()> {
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "update uploads set status = 'failed', error = $2, finished_at = now() where id = $1",
        )
        .bind(job.upload_id)
        .bind(json_no_nul(serde_json::to_value(error)?))
        .execute(&mut *tx)
        .await?;
        crate::events::emit(
            &mut tx,
            "upload.failed",
            Some(&job.project_key),
            serde_json::json!({"id": job.upload_id, "launchId": job.launch_id, "error": error}),
        )
        .await?;
        crate::events::notify_upload(&mut tx, &job.project_key, job.launch_id, job.upload_id)
            .await?;
        crate::events::notify_launch(&mut tx, &job.project_key, job.launch_id).await?;
        tx.commit().await?;
        Ok(())
    }

    /// Which of `keys` are client keys of attempts of the launch.
    pub async fn existing_client_keys(
        &self,
        launch_id: Uuid,
        launch_created_at: chrono::DateTime<chrono::Utc>,
        keys: &[String],
    ) -> anyhow::Result<std::collections::HashSet<String>> {
        let keys: Vec<String> = keys.iter().map(|k| crate::no_nul(k)).collect();
        let found: Vec<String> = sqlx::query_scalar(
            "select client_key from attempts where launch_id = $1 and launch_created_at = $2 \
             and client_key = any($3)",
        )
        .bind(launch_id)
        .bind(launch_created_at)
        .bind(&keys)
        .fetch_all(&self.pool)
        .await?;
        Ok(found.into_iter().collect())
    }

    /// True while the upload exists (it goes with its launch).
    pub async fn upload_exists(&self, upload_id: Uuid) -> anyhow::Result<bool> {
        Ok(
            sqlx::query_scalar::<_, i32>("select 1 from uploads where id = $1")
                .bind(upload_id)
                .fetch_optional(&self.pool)
                .await?
                .is_some(),
        )
    }

    /// Records attachments whose content an upload stored after its launch was deleted, without
    /// linking them: unlinked attachments are collected after their grace period and their
    /// blobs deleted ([`Store::collect_attachments`]), the same way as attachments of deleted
    /// launches, so no blob is left that no row knows of. Hashes stored already are only
    /// touched.
    pub async fn keep_unlinked_attachments(
        &self,
        project_id: Uuid,
        items: &[crate::attachments::NewAttachment],
    ) -> anyhow::Result<()> {
        if items.is_empty() {
            return Ok(());
        }
        let mut sorted: Vec<&crate::attachments::NewAttachment> = items.iter().collect();
        sorted.sort_by(|a, b| a.sha256.cmp(&b.sha256));
        sorted.dedup_by(|a, b| a.sha256 == b.sha256);
        sqlx::query(
            "insert into attachments (id, project_id, sha256, size, content_type, original_name, blob_key) \
             select i, $1, s, z, c, n, k from unnest($2::uuid[], $3::text[], $4::bigint[], $5::text[], \
               $6::text[], $7::text[]) as t(i, s, z, c, n, k) \
             on conflict (project_id, sha256) do update set last_seen_at = now()",
        )
        .bind(project_id)
        .bind(sorted.iter().map(|_| Uuid::now_v7()).collect::<Vec<_>>())
        .bind(sorted.iter().map(|a| a.sha256.clone()).collect::<Vec<_>>())
        .bind(sorted.iter().map(|a| a.size).collect::<Vec<_>>())
        .bind(sorted.iter().map(|a| no_nul(&a.content_type)).collect::<Vec<_>>())
        .bind(
            sorted
                .iter()
                .map(|a| a.original_name.as_deref().map(no_nul))
                .collect::<Vec<_>>(),
        )
        .bind(sorted.iter().map(|a| a.blob_key.clone()).collect::<Vec<_>>())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// The launch `id` of project `key`, for the writer.
    pub async fn writer_launch(&self, key: &str, id: Uuid) -> anyhow::Result<Option<WriterLaunch>> {
        let r = sqlx::query(
            "select l.id, l.project_id, p.key, l.created_at, l.env_key from launches l \
             join projects p on p.id = l.project_id where p.key = $1 and l.id = $2 and p.deleted_at is null",
        )
        .bind(key)
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(match r {
            None => None,
            Some(r) => Some(WriterLaunch {
                launch: LaunchRef {
                    id: r.try_get("id")?,
                    project_id: r.try_get("project_id")?,
                    project_key: r.try_get("key")?,
                    created_at: r.try_get("created_at")?,
                },
                env_key: r.try_get("env_key")?,
            }),
        })
    }

    /// Results by id with launch metadata, for the sinks.
    pub async fn sink_results(&self, ids: &[Uuid]) -> anyhow::Result<Vec<SinkResult>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "select r.id, p.key, r.project_id, r.launch_id, l.number, r.launch_created_at, r.series_id, \
               r.test_case_id, r.name, r.full_name, r.status, r.flaky_in_run, r.attempt_count, r.started_at, \
               r.duration_ms, r.message, r.fingerprint, l.branch, l.env_key, r.updated_at, r.muted, \
               {} as defect_type_id, coalesce(r.defect->>'group', '') as defect_group, \
               coalesce(r.defect->>'source', '') as defect_source, \
               array_to_string(r.suite_path, ' / ') as suite, \
               coalesce((select f.alias_of from failure_fingerprints f where f.project_id = r.project_id \
                 and f.fingerprint = r.fingerprint), r.fingerprint) as canonical_fingerprint \
             from results r join launches l on l.id = r.launch_id join projects p on p.id = r.project_id \
             where r.id = any($1)",
            uuid_of("r.defect->>'typeId'")
        )))
        .bind(ids)
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|r| {
                Ok(SinkResult {
                    id: r.try_get("id")?,
                    project: r.try_get("key")?,
                    project_id: r.try_get("project_id")?,
                    launch_id: r.try_get("launch_id")?,
                    launch_number: r.try_get("number")?,
                    launch_created_at: r.try_get("launch_created_at")?,
                    series_id: r.try_get("series_id")?,
                    test_case_id: r.try_get("test_case_id")?,
                    name: r.try_get("name")?,
                    full_name: r.try_get("full_name")?,
                    status: r.try_get("status")?,
                    flaky_in_run: r.try_get("flaky_in_run")?,
                    attempt_count: r.try_get("attempt_count")?,
                    started_at: r.try_get("started_at")?,
                    duration_ms: r.try_get("duration_ms")?,
                    message: r.try_get("message")?,
                    fingerprint: r.try_get("fingerprint")?,
                    branch: r.try_get("branch")?,
                    env_key: r.try_get("env_key")?,
                    updated_at: r.try_get("updated_at")?,
                    defect_type_id: r.try_get("defect_type_id")?,
                    defect_group: r.try_get("defect_group")?,
                    defect_source: r.try_get("defect_source")?,
                    muted: r.try_get("muted")?,
                    canonical_fingerprint: r.try_get("canonical_fingerprint")?,
                    suite: r.try_get("suite")?,
                })
            })
            .collect()
    }

    /// Attempts by id with their steps, for the sinks.
    pub async fn sink_attempts(&self, ids: &[Uuid]) -> anyhow::Result<Vec<SinkAttempt>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let rows = sqlx::query(
            "select a.id, p.key, a.project_id, a.launch_id, a.launch_created_at, a.result_id, a.series_id, \
               a.client_key, a.seq, a.status, a.started_at, a.duration_ms, a.message, a.trace, a.steps_z, \
               a.fingerprint, a.trace_id \
             from attempts a join projects p on p.id = a.project_id where a.id = any($1)",
        )
        .bind(ids)
        .fetch_all(&self.pool)
        .await?;
        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            let steps_z: Option<Vec<u8>> = r.try_get("steps_z")?;
            let trace: Option<String> = r.try_get("trace")?;
            out.push(SinkAttempt {
                frames: trace
                    .as_deref()
                    .map(fastproof_analysis::frames::frames)
                    .unwrap_or_default(),
                fingerprint: r.try_get("fingerprint")?,
                trace_id: r.try_get("trace_id")?,
                id: r.try_get("id")?,
                project: r.try_get("key")?,
                project_id: r.try_get("project_id")?,
                launch_id: r.try_get("launch_id")?,
                launch_created_at: r.try_get("launch_created_at")?,
                result_id: r.try_get("result_id")?,
                series_id: r.try_get("series_id")?,
                client_key: r.try_get("client_key")?,
                seq: r.try_get("seq")?,
                status: r.try_get("status")?,
                started_at: r.try_get("started_at")?,
                duration_ms: r.try_get("duration_ms")?,
                message: r.try_get("message")?,
                trace,
                steps: match steps_z {
                    Some(z) => codec::decode(&z).unwrap_or_default(),
                    None => Vec::new(),
                },
            });
        }
        Ok(out)
    }

    /// Decisions of the named batches (`triage.decided` events: project key and batch id)
    /// after decision id `after`, in id order, at most `limit`. Batches of deleted projects
    /// are skipped.
    pub async fn sink_triage_rows(
        &self,
        batches: &[(String, Uuid)],
        after: i64,
        limit: i64,
    ) -> anyhow::Result<Vec<SinkTriage>> {
        if batches.is_empty() {
            return Ok(Vec::new());
        }
        let keys: Vec<&str> = batches.iter().map(|b| b.0.as_str()).collect();
        let ids: Vec<Uuid> = batches.iter().map(|b| b.1).collect();
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "{} where d.project_id in (select id from projects where key = any($1) and deleted_at is null) \
               and d.batch_id = any($2) and d.id > $3 order by d.id limit $4",
            triage_select()
        )))
        .bind(&keys)
        .bind(&ids)
        .bind(after)
        .bind(limit.clamp(1, 10_000))
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(triage_row).collect()
    }

    /// Decisions after id `after` up to id `until`, except rule batches (the backfill), in id
    /// order, at most `limit`.
    pub async fn sink_triage_after(
        &self,
        after: i64,
        until: i64,
        limit: i64,
    ) -> anyhow::Result<Vec<SinkTriage>> {
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "{} where d.id > $1 and d.id <= $2 and d.via <> 'rule' and p.deleted_at is null \
             order by d.id limit $3",
            triage_select()
        )))
        .bind(after)
        .bind(until)
        .bind(limit.clamp(1, 10_000))
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(triage_row).collect()
    }

    /// The newest `triage_decisions` id (0 without any).
    pub async fn max_decision_id(&self) -> anyhow::Result<i64> {
        Ok(
            sqlx::query_scalar::<_, Option<i64>>("select max(id) from triage_decisions")
                .fetch_one(&self.pool)
                .await?
                .unwrap_or(0),
        )
    }

    /// The next launches created since `since` after the cursor `(created_at, id)`, oldest
    /// first, at most `limit` (the backfill re-sends their results).
    pub async fn backfill_launches(
        &self,
        since: DateTime<Utc>,
        after: Option<(DateTime<Utc>, Uuid)>,
        limit: i64,
    ) -> anyhow::Result<Vec<(DateTime<Utc>, Uuid)>> {
        let (at, id) = after.unwrap_or((since, Uuid::nil()));
        Ok(sqlx::query_as(
            "select l.created_at, l.id from launches l join projects p on p.id = l.project_id \
             where p.deleted_at is null and l.created_at >= $1 and (l.created_at, l.id) > ($2, $3) \
             order by l.created_at, l.id limit $4",
        )
        .bind(since)
        .bind(at)
        .bind(id)
        .bind(limit.clamp(1, 10_000))
        .fetch_all(&self.pool)
        .await?)
    }

    /// Results of a launch whose defect columns matter to ClickHouse: failed or broken, muted,
    /// or triaged.
    pub async fn backfill_result_ids(
        &self,
        launch_id: Uuid,
        launch_created_at: DateTime<Utc>,
    ) -> anyhow::Result<Vec<Uuid>> {
        Ok(sqlx::query_scalar(
            "select id from results where launch_id = $1 and launch_created_at = $2 \
               and (status in ('failed', 'broken') or muted or defect is not null) order by id",
        )
        .bind(launch_id)
        .bind(launch_created_at)
        .fetch_all(&self.pool)
        .await?)
    }

    /// The state of the ClickHouse backfill (`None` before it started).
    pub async fn ch_backfill(&self) -> anyhow::Result<Option<ChBackfill>> {
        let v: Option<serde_json::Value> =
            sqlx::query_scalar("select value from analysis_state where name = $1")
                .bind(CH_BACKFILL_STATE)
                .fetch_optional(&self.pool)
                .await?;
        Ok(v.and_then(|v| serde_json::from_value(v).ok()))
    }

    /// Stores the state of the ClickHouse backfill.
    pub async fn put_ch_backfill(&self, st: &ChBackfill) -> anyhow::Result<()> {
        sqlx::query(
            "insert into analysis_state (name, value) values ($1, $2) \
             on conflict (name) do update set value = excluded.value, updated_at = now()",
        )
        .bind(CH_BACKFILL_STATE)
        .bind(serde_json::to_value(st)?)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// The reference failure of a similar-failure lookup.
    pub async fn similar_source(
        &self,
        project_id: Uuid,
        id: Uuid,
    ) -> anyhow::Result<Option<SimilarSource>> {
        let r = sqlx::query(
            "select id, project_id, message, fingerprint from results where project_id = $1 and id = $2",
        )
        .bind(project_id)
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(match r {
            None => None,
            Some(r) => Some(SimilarSource {
                id: r.try_get("id")?,
                project_id: r.try_get("project_id")?,
                message: r.try_get("message")?,
                fingerprint: r.try_get("fingerprint")?,
            }),
        })
    }

    /// Similar failures in PostgreSQL: the same fingerprint (or, while results carry none, the
    /// same normalized message hash), then the trigram similarity of the normalized messages.
    /// Looks at the newest 5000 failures of the last 30 days only.
    pub async fn similar_by_message(
        &self,
        src: &SimilarSource,
        limit: i64,
    ) -> anyhow::Result<Vec<SimilarResult>> {
        let Some(message) = src.message.as_deref().filter(|m| !m.trim().is_empty()) else {
            return Ok(Vec::new());
        };
        let rows = sqlx::query(
            "select r.id, r.launch_id, l.number, r.name, r.message, r.fingerprint, r.defect from results r \
             join launches l on l.id = r.launch_id where r.project_id = $1 and r.id <> $2 \
               and r.status in ('failed', 'broken') and r.message is not null \
               and r.launch_created_at > now() - interval '30 days' \
             order by r.launch_created_at desc limit 5000",
        )
        .bind(src.project_id)
        .bind(src.id)
        .fetch_all(&self.pool)
        .await?;
        let key = match &src.fingerprint {
            Some(f) => format!("f:{f}"),
            None => format!("h:{}", message_hash(message)),
        };
        let norm = normalize_message(message);
        let grams = trigrams(&norm);
        let mut scored: Vec<SimilarResult> = Vec::new();
        for r in rows {
            let m: Option<String> = r.try_get("message")?;
            let fp: Option<String> = r.try_get("fingerprint")?;
            let Some(m) = m else { continue };
            let other = match &fp {
                Some(f) if src.fingerprint.is_some() => format!("f:{f}"),
                _ => format!("h:{}", message_hash(&m)),
            };
            let score = if other == key {
                1.0
            } else {
                jaccard(&grams, &trigrams(&normalize_message(&m)))
            };
            if score < 0.3 {
                continue;
            }
            scored.push(SimilarResult {
                result_id: r.try_get("id")?,
                launch_id: r.try_get("launch_id")?,
                launch_number: r.try_get("number")?,
                name: r.try_get("name")?,
                message: Some(m.chars().take(500).collect()),
                score: (score * 1000.0).round() / 1000.0,
                defect: r.try_get("defect")?,
            });
        }
        scored.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        scored.truncate(limit.max(1) as usize);
        Ok(scored)
    }

    /// Launch numbers by id (for search backends that do not store them).
    pub async fn launch_numbers(&self, ids: &[Uuid]) -> anyhow::Result<HashMap<Uuid, i64>> {
        let rows: Vec<(Uuid, i64)> =
            sqlx::query_as("select id, number from launches where id = any($1)")
                .bind(ids)
                .fetch_all(&self.pool)
                .await?;
        Ok(rows.into_iter().collect())
    }
}

fn trigrams(s: &str) -> std::collections::HashSet<[char; 3]> {
    let chars: Vec<char> = format!("  {s} ").chars().collect();
    chars.windows(3).map(|w| [w[0], w[1], w[2]]).collect()
}

fn jaccard(
    a: &std::collections::HashSet<[char; 3]>,
    b: &std::collections::HashSet<[char; 3]>,
) -> f64 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let inter = a.intersection(b).count() as f64;
    inter / (a.len() + b.len()) as f64 * 2.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalization() {
        assert_eq!(
            normalize_message("Timeout  after 3000 ms\nat Order#42 id=DEADBEEF0042"),
            "timeout after 0 ms at order#0 id=deadbeef0"
        );
        assert_eq!(
            message_hash("expected 1 got 2"),
            message_hash("Expected 10 got 20")
        );
        assert_eq!(normalize_message("trace 0f1e2d3c4b5a6978"), "trace x");
        let a = trigrams("connection refused to db");
        let b = trigrams("connection refused to cache");
        assert!(jaccard(&a, &b) > 0.5 && jaccard(&a, &b) < 1.0);
    }
}
