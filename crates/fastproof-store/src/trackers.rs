//! Issues in external trackers (Jira, GitLab, GitHub, Azure DevOps) created or linked from
//! FastProof, their links to results and failure clusters, and the state synchronised back
//! from the tracker.

use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::Row;
use sqlx::postgres::PgRow;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{Store, no_nul};

/// Issues tracked per project, at most.
pub const MAX_TRACKED_ISSUES: i64 = 10_000;
/// Links of one issue, at most.
pub const MAX_LINKS_PER_ISSUE: i64 = 100;
/// Series kept on one link, at most.
pub const MAX_LINK_SERIES: i64 = 1000;
/// URLs or keys looked up in one list request, at most.
pub const MAX_LOOKUP: usize = 100;

/// An issue in a tracker.
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TrackerIssue {
    pub id: Uuid,
    /// `id` of the tracker in the `issueTrackers` settings.
    pub tracker_id: String,
    /// `jira`, `gitlab`, `github` or `azure`.
    pub tracker_type: String,
    /// `SHOP-12` (Jira), `#12` (GitLab, GitHub) or `12` (Azure DevOps); absent while creating.
    pub key: Option<String>,
    pub url: Option<String>,
    pub title: String,
    /// The state as the tracker names it (`In Progress`, `opened`, `closed`, `Done`).
    pub state: String,
    /// `creating`, `open` or `closed`.
    pub category: String,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub closed_at: Option<DateTime<Utc>>,
    /// Last successful read of the issue from the tracker.
    pub synced_at: Option<DateTime<Utc>>,
    pub next_sync_at: Option<DateTime<Utc>>,
    /// Why the last read failed (no URL or token in it).
    pub sync_error: Option<String>,
    /// The issue was closed while tests linked to it are still muted.
    pub unmute_suggested: bool,
    /// Links to results and failure clusters (in the single-issue answer only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub links: Option<Vec<TrackerIssueLink>>,
    /// Linked tests that are muted now (in the single-issue answer only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub muted_tests: Option<i64>,
}

/// A link of an issue to a result or to a failure cluster (a fingerprint in a launch).
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TrackerIssueLink {
    pub id: Uuid,
    pub launch_id: Option<Uuid>,
    pub result_id: Option<Uuid>,
    pub fingerprint: Option<String>,
    /// Tests (series) behind the link.
    pub tests: i64,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    #[serde(skip)]
    pub series_ids: Vec<Uuid>,
}

/// A result an issue is created for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IssueTarget {
    pub id: Uuid,
    pub launch_id: Uuid,
    pub series_id: Uuid,
    pub name: String,
    pub full_name: String,
}

fn target_row(r: &PgRow) -> anyhow::Result<IssueTarget> {
    Ok(IssueTarget {
        id: r.try_get("id")?,
        launch_id: r.try_get("launch_id")?,
        series_id: r.try_get("series_id")?,
        name: r.try_get("name")?,
        full_name: r.try_get("full_name")?,
    })
}

/// A link to add.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewLink {
    pub launch_id: Option<Uuid>,
    pub result_id: Option<Uuid>,
    pub fingerprint: Option<String>,
    pub series_ids: Vec<Uuid>,
}

/// Filter of the issue list.
#[derive(Clone, Debug, Default)]
pub struct IssueFilter {
    pub urls: Vec<String>,
    pub keys: Vec<String>,
    pub tracker_id: Option<String>,
    pub category: Option<String>,
    pub unmute_suggested: Option<bool>,
    pub result_id: Option<Uuid>,
    pub launch_id: Option<Uuid>,
    pub before: Option<Uuid>,
    pub limit: i64,
}

/// What the tracker said about an issue, to store.
#[derive(Clone, Debug, Default)]
pub struct IssueState {
    pub key: String,
    pub remote_id: String,
    pub url: String,
    pub title: String,
    pub state: String,
    pub closed: bool,
}

/// An issue due for a read from its tracker.
#[derive(Clone, Debug)]
pub struct SyncJob {
    pub id: Uuid,
    pub project_id: Uuid,
    pub project_key: String,
    pub tracker_id: String,
    pub tracker_type: String,
    pub key: String,
    pub remote_id: Option<String>,
    pub etag: Option<String>,
    pub closed: bool,
    pub closed_at: Option<DateTime<Utc>>,
    pub attempts: i32,
    /// The lease the read holds; its outcome is stored only while the lease is still its own.
    pub lease_id: Uuid,
}

/// The answer to a read asked for by a user.
#[derive(Clone, Debug)]
pub enum ReadNow {
    /// Leased for this read.
    Job(Box<SyncJob>),
    /// Not now: read recently, backing off after failures, or being read; try again after this
    /// many seconds.
    Later(i64),
    NotFound,
}

/// Why a create cannot start.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Limit {
    /// Too many creates (or links) in the last hour.
    Hourly,
    /// Too many issues followed.
    Total,
}

/// How a read ended.
#[derive(Clone, Debug)]
pub enum SyncOutcome {
    /// Read; `etag` for the next conditional read.
    Read(IssueState, Option<String>),
    /// Not modified since the last read (`304`).
    Unchanged,
    /// Failed; tried again at `retry_at` (never when `None`).
    Failed(String, Option<DateTime<Utc>>),
}

/// The start of a create: a new row, or the issue a request with the same id already made.
#[derive(Clone, Debug)]
pub enum Begin {
    New(Uuid),
    Existing(Box<TrackerIssue>),
    Limited(Limit),
}

const COLUMNS: &str = "id, tracker_id, tracker_type, issue_key, url, title, state, category, created_by, \
    created_at, updated_at, closed_at, synced_at, next_sync_at, sync_error, unmute_suggested";

fn issue_row(r: &PgRow) -> anyhow::Result<TrackerIssue> {
    Ok(TrackerIssue {
        id: r.try_get("id")?,
        tracker_id: r.try_get("tracker_id")?,
        tracker_type: r.try_get("tracker_type")?,
        key: r.try_get("issue_key")?,
        url: r.try_get("url")?,
        title: r.try_get("title")?,
        state: r.try_get("state")?,
        category: r.try_get("category")?,
        created_by: r.try_get("created_by")?,
        created_at: r.try_get("created_at")?,
        updated_at: r.try_get("updated_at")?,
        closed_at: r.try_get("closed_at")?,
        synced_at: r.try_get("synced_at")?,
        next_sync_at: r.try_get("next_sync_at")?,
        sync_error: r.try_get("sync_error")?,
        unmute_suggested: r.try_get("unmute_suggested")?,
        links: None,
        muted_tests: None,
    })
}

fn sync_row(r: &PgRow) -> anyhow::Result<SyncJob> {
    let category: String = r.try_get("category")?;
    Ok(SyncJob {
        id: r.try_get("id")?,
        project_id: r.try_get("project_id")?,
        project_key: r.try_get("key")?,
        tracker_id: r.try_get("tracker_id")?,
        tracker_type: r.try_get("tracker_type")?,
        key: r.try_get("issue_key")?,
        remote_id: r.try_get("remote_id")?,
        etag: r.try_get("etag")?,
        closed: category == "closed",
        closed_at: r.try_get("closed_at")?,
        attempts: r.try_get("sync_attempts")?,
        lease_id: r.try_get("lease_id")?,
    })
}

/// At most `max` bytes, cut at a character boundary.
fn clip(s: &str, max: usize) -> String {
    let s = no_nul(s);
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

const JOB_COLUMNS: &str = "i.id, i.project_id, p.key, i.tracker_id, i.tracker_type, i.issue_key, i.remote_id, \
    i.etag, i.category, i.closed_at, i.sync_attempts, i.lease_id";

/// Serialises the limit checks of one project's creates and links.
async fn project_lock(tx: &mut sqlx::PgConnection, project_id: Uuid) -> anyhow::Result<()> {
    sqlx::query(
        "select pg_advisory_xact_lock(hashtextextended('fastproof-tracker:' || $1::text, 0))",
    )
    .bind(project_id)
    .execute(&mut *tx)
    .await?;
    Ok(())
}

impl Store {
    /// Issues of a project, newest first.
    pub async fn tracker_issues(
        &self,
        project_id: Uuid,
        f: &IssueFilter,
    ) -> anyhow::Result<(Vec<TrackerIssue>, Option<Uuid>)> {
        let limit = f.limit.clamp(1, 200);
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "select {COLUMNS} from tracker_issues i where project_id = $1 \
               and (cardinality($2::text[]) = 0 or (md5(url) = any(array(select md5(u) from unnest($2::text[]) u)) \
                     and url = any($2))) \
               and (cardinality($3::text[]) = 0 or issue_key = any($3)) \
               and ($4::text is null or tracker_id = $4) \
               and (($5::text is null and category <> 'failed') or category = $5) \
               and ($6::boolean is null or unmute_suggested = $6) \
               and ($7::uuid is null or exists (select 1 from tracker_issue_links k \
                     where k.issue_id = i.id and k.project_id = $1 and k.result_id = $7)) \
               and ($8::uuid is null or exists (select 1 from tracker_issue_links k \
                     where k.issue_id = i.id and k.launch_id = $8)) \
               and ($9::uuid is null or id < $9) \
             order by id desc limit $10"
        )))
        .bind(project_id)
        .bind(&f.urls)
        .bind(&f.keys)
        .bind(f.tracker_id.as_deref())
        .bind(f.category.as_deref())
        .bind(f.unmute_suggested)
        .bind(f.result_id)
        .bind(f.launch_id)
        .bind(f.before)
        .bind(limit + 1)
        .fetch_all(&self.pool)
        .await?;
        let mut items = rows
            .iter()
            .map(issue_row)
            .collect::<anyhow::Result<Vec<_>>>()?;
        let next = if items.len() as i64 > limit {
            items.truncate(limit as usize);
            items.last().map(|i| i.id)
        } else {
            None
        };
        Ok((items, next))
    }

    /// One issue with its links and the number of linked tests muted now.
    pub async fn tracker_issue(
        &self,
        project_id: Uuid,
        id: Uuid,
    ) -> anyhow::Result<Option<TrackerIssue>> {
        let r = sqlx::query(sqlx::AssertSqlSafe(format!(
            "select {COLUMNS} from tracker_issues where project_id = $1 and id = $2"
        )))
        .bind(project_id)
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        let Some(r) = r else { return Ok(None) };
        let mut issue = issue_row(&r)?;
        let links = self.tracker_links(id).await?;
        issue.muted_tests = Some(self.muted_linked_series(project_id, &links).await?.len() as i64);
        issue.links = Some(links);
        Ok(Some(issue))
    }

    async fn tracker_links(&self, issue_id: Uuid) -> anyhow::Result<Vec<TrackerIssueLink>> {
        let rows = sqlx::query(
            "select id, launch_id, result_id, fingerprint, series_ids, created_by, created_at \
             from tracker_issue_links where issue_id = $1 order by created_at, id limit $2",
        )
        .bind(issue_id)
        .bind(MAX_LINKS_PER_ISSUE)
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|r| {
                let series_ids: Vec<Uuid> = r.try_get("series_ids")?;
                Ok(TrackerIssueLink {
                    id: r.try_get("id")?,
                    launch_id: r.try_get("launch_id")?,
                    result_id: r.try_get("result_id")?,
                    fingerprint: r.try_get("fingerprint")?,
                    tests: series_ids.len() as i64,
                    created_by: r.try_get("created_by")?,
                    created_at: r.try_get("created_at")?,
                    series_ids,
                })
            })
            .collect()
    }

    /// The links of an issue (for un-muting).
    pub async fn tracker_issue_links(
        &self,
        project_id: Uuid,
        id: Uuid,
    ) -> anyhow::Result<Vec<TrackerIssueLink>> {
        let owned: Option<Uuid> =
            sqlx::query_scalar("select id from tracker_issues where project_id = $1 and id = $2")
                .bind(project_id)
                .bind(id)
                .fetch_optional(&self.pool)
                .await?;
        if owned.is_none() {
            return Ok(Vec::new());
        }
        self.tracker_links(id).await
    }

    /// Linked series that are muted now.
    pub async fn muted_linked_series(
        &self,
        project_id: Uuid,
        links: &[TrackerIssueLink],
    ) -> anyhow::Result<Vec<Uuid>> {
        let mut ids: Vec<Uuid> = links.iter().flat_map(|l| l.series_ids.clone()).collect();
        ids.sort();
        ids.dedup();
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        Ok(sqlx::query_scalar(
            "select series_id from series_mutes where project_id = $1 and series_id = any($2) \
               and (until is null or until > now()) order by series_id",
        )
        .bind(project_id)
        .bind(&ids)
        .fetch_all(&self.pool)
        .await?)
    }

    /// Starts a create: under the project's lock, checks the hourly and total limits (failed
    /// attempts count toward the hourly one), records the attempt and inserts a `creating` row
    /// for the request id. A request id seen before answers the issue it made (or starts again
    /// when its create failed).
    #[allow(clippy::too_many_arguments)]
    pub async fn begin_tracker_issue(
        &self,
        project_id: Uuid,
        tracker_id: &str,
        tracker_type: &str,
        request_id: Uuid,
        by: &str,
        per_hour: i64,
        total: i64,
    ) -> anyhow::Result<Begin> {
        let mut tx = self.pool.begin().await?;
        project_lock(&mut tx, project_id).await?;
        let prev = sqlx::query(sqlx::AssertSqlSafe(format!(
            "select {COLUMNS} from tracker_issues where project_id = $1 and request_id = $2"
        )))
        .bind(project_id)
        .bind(request_id)
        .fetch_optional(&mut *tx)
        .await?;
        let prev = prev.as_ref().map(issue_row).transpose()?;
        if let Some(p) = &prev
            && p.category != "failed"
        {
            return Ok(Begin::Existing(Box::new(p.clone())));
        }
        let (followed, recent): (i64, i64) = sqlx::query_as(
            "select (select count(*) from tracker_issues where project_id = $1 and category <> 'failed'), \
               (select count(*) from tracker_requests where project_id = $1 and kind = 'create' \
                  and at > now() - interval '1 hour')",
        )
        .bind(project_id)
        .fetch_one(&mut *tx)
        .await?;
        if recent >= per_hour {
            return Ok(Begin::Limited(Limit::Hourly));
        }
        if followed >= total {
            return Ok(Begin::Limited(Limit::Total));
        }
        sqlx::query(
            "insert into tracker_requests (id, project_id, kind) values ($1, $2, 'create')",
        )
        .bind(Uuid::now_v7())
        .bind(project_id)
        .execute(&mut *tx)
        .await?;
        let id = match prev {
            Some(p) => {
                sqlx::query(
                    "update tracker_issues set category = 'creating', tracker_id = $2, tracker_type = $3, \
                       created_by = $4, created_at = now(), updated_at = now() where id = $1",
                )
                .bind(p.id)
                .bind(tracker_id)
                .bind(tracker_type)
                .bind(clip(by, 200))
                .execute(&mut *tx)
                .await?;
                p.id
            }
            None => {
                let id = Uuid::now_v7();
                sqlx::query(
                    "insert into tracker_issues (id, project_id, tracker_id, tracker_type, request_id, created_by) \
                     values ($1, $2, $3, $4, $5, $6)",
                )
                .bind(id)
                .bind(project_id)
                .bind(tracker_id)
                .bind(tracker_type)
                .bind(request_id)
                .bind(clip(by, 200))
                .execute(&mut *tx)
                .await?;
                id
            }
        };
        tx.commit().await?;
        Ok(Begin::New(id))
    }

    /// Records a link attempt under the project's lock: `None` when allowed, else the limit hit.
    /// `new_issue` counts the total of followed issues as well.
    pub async fn take_link_quota(
        &self,
        project_id: Uuid,
        per_hour: i64,
        total: Option<i64>,
    ) -> anyhow::Result<Option<Limit>> {
        let mut tx = self.pool.begin().await?;
        project_lock(&mut tx, project_id).await?;
        let (followed, recent): (i64, i64) = sqlx::query_as(
            "select (select count(*) from tracker_issues where project_id = $1 and category <> 'failed'), \
               (select count(*) from tracker_requests where project_id = $1 and kind = 'link' \
                  and at > now() - interval '1 hour')",
        )
        .bind(project_id)
        .fetch_one(&mut *tx)
        .await?;
        if recent >= per_hour {
            return Ok(Some(Limit::Hourly));
        }
        if total.is_some_and(|t| followed >= t) {
            return Ok(Some(Limit::Total));
        }
        sqlx::query("insert into tracker_requests (id, project_id, kind) values ($1, $2, 'link')")
            .bind(Uuid::now_v7())
            .bind(project_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(None)
    }

    /// Marks a `creating` row whose create failed: it keeps counting toward the hourly limit
    /// and its request id may start again.
    pub async fn abandon_tracker_issue(&self, id: Uuid) -> anyhow::Result<()> {
        sqlx::query(
            "update tracker_issues set category = 'failed', updated_at = now() \
             where id = $1 and category = 'creating'",
        )
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Housekeeping: `creating` rows left by a crash become `failed` after 10 minutes, failed
    /// rows and attempt records go after two days.
    pub async fn sweep_tracker_rows(&self) -> anyhow::Result<()> {
        sqlx::query(
            "update tracker_issues set category = 'failed', updated_at = now() \
             where category = 'creating' and created_at < now() - interval '10 minutes'",
        )
        .execute(&self.pool)
        .await?;
        sqlx::query(
            "delete from tracker_issues where category = 'failed' and updated_at < now() - interval '2 days'",
        )
        .execute(&self.pool)
        .await?;
        sqlx::query("delete from tracker_requests where at < now() - interval '2 days'")
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Records the created issue. `Ok(false)` when the key is already tracked (the row is then
    /// dropped and the caller links the existing one).
    pub async fn complete_tracker_issue(
        &self,
        id: Uuid,
        s: &IssueState,
        next_sync: Option<Duration>,
    ) -> anyhow::Result<bool> {
        let r = sqlx::query(
            "update tracker_issues set issue_key = $2, remote_id = $3, url = $4, title = $5, state = $6, \
               category = case when $7 then 'closed' else 'open' end, \
               closed_at = case when $7 then now() end, synced_at = now(), \
               next_sync_at = now() + make_interval(secs => $8::float8), updated_at = now() \
             where id = $1 and category = 'creating'",
        )
        .bind(id)
        .bind(clip(&s.key, 128))
        .bind(clip(&s.remote_id, 64))
        .bind(clip(&s.url, 2048))
        .bind(clip(&s.title, 500))
        .bind(clip(&s.state, 100))
        .bind(s.closed)
        .bind(next_sync.map(|d| d.as_secs_f64()))
        .execute(&self.pool)
        .await;
        match r {
            Ok(_) => Ok(true),
            Err(sqlx::Error::Database(e)) if e.is_unique_violation() => {
                sqlx::query("delete from tracker_issues where id = $1 and category = 'creating'")
                    .bind(id)
                    .execute(&self.pool)
                    .await?;
                Ok(false)
            }
            Err(e) => Err(e.into()),
        }
    }

    /// Tracks an existing issue (or refreshes the one already tracked under this key) and
    /// returns its id.
    pub async fn upsert_tracker_issue(
        &self,
        project_id: Uuid,
        tracker_id: &str,
        tracker_type: &str,
        s: &IssueState,
        by: &str,
        next_sync: Option<Duration>,
    ) -> anyhow::Result<Uuid> {
        let id: Uuid = sqlx::query_scalar(
            "insert into tracker_issues (id, project_id, tracker_id, tracker_type, issue_key, remote_id, url, title, \
               state, category, closed_at, synced_at, next_sync_at, created_by) \
             values ($1, $2, $3, $4, $5, $6, $7, $8, $9, case when $10 then 'closed' else 'open' end, \
               case when $10 then now() end, now(), now() + make_interval(secs => $11::float8), $12) \
             on conflict (project_id, tracker_id, issue_key) do update set remote_id = excluded.remote_id, \
               url = excluded.url, title = excluded.title, state = excluded.state, category = excluded.category, \
               closed_at = case when excluded.category = 'closed' then coalesce(tracker_issues.closed_at, now()) end, \
               synced_at = now(), sync_error = null, sync_attempts = 0, next_sync_at = excluded.next_sync_at, \
               updated_at = now() \
             returning id",
        )
        .bind(Uuid::now_v7())
        .bind(project_id)
        .bind(tracker_id)
        .bind(tracker_type)
        .bind(clip(&s.key, 128))
        .bind(clip(&s.remote_id, 64))
        .bind(clip(&s.url, 2048))
        .bind(clip(&s.title, 500))
        .bind(clip(&s.state, 100))
        .bind(s.closed)
        .bind(next_sync.map(|d| d.as_secs_f64()))
        .bind(clip(by, 200))
        .fetch_one(&self.pool)
        .await?;
        Ok(id)
    }

    /// The id of the issue tracked under this key.
    pub async fn tracker_issue_by_key(
        &self,
        project_id: Uuid,
        tracker_id: &str,
        key: &str,
    ) -> anyhow::Result<Option<Uuid>> {
        Ok(sqlx::query_scalar(
            "select id from tracker_issues where project_id = $1 and tracker_id = $2 and issue_key = $3",
        )
        .bind(project_id)
        .bind(tracker_id)
        .bind(key)
        .fetch_optional(&self.pool)
        .await?)
    }

    /// Adds links that the issue does not have yet (same result, or same launch and
    /// fingerprint), up to [`MAX_LINKS_PER_ISSUE`]. Returns how many were added.
    pub async fn add_tracker_links(
        &self,
        project_id: Uuid,
        issue_id: Uuid,
        links: &[NewLink],
        by: &str,
    ) -> anyhow::Result<u64> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("select 1 from tracker_issues where id = $1 for update")
            .bind(issue_id)
            .execute(&mut *tx)
            .await?;
        let existing: Vec<(Option<Uuid>, Option<Uuid>, Option<String>)> = sqlx::query_as(
            "select launch_id, result_id, fingerprint from tracker_issue_links where issue_id = $1",
        )
        .bind(issue_id)
        .fetch_all(&mut *tx)
        .await?;
        let mut have = existing.len() as i64;
        let mut added = 0;
        for l in links {
            if have >= MAX_LINKS_PER_ISSUE {
                break;
            }
            let dup = existing.iter().any(|(la, r, f)| match l.result_id {
                Some(id) => *r == Some(id),
                None => r.is_none() && *la == l.launch_id && *f == l.fingerprint,
            });
            if dup {
                continue;
            }
            let mut series = l.series_ids.clone();
            series.sort();
            series.dedup();
            series.truncate(MAX_LINK_SERIES as usize);
            sqlx::query(
                "insert into tracker_issue_links (id, issue_id, project_id, launch_id, result_id, fingerprint, \
                   series_ids, created_by) values ($1, $2, $3, $4, $5, $6, $7, $8)",
            )
            .bind(Uuid::now_v7())
            .bind(issue_id)
            .bind(project_id)
            .bind(l.launch_id)
            .bind(l.result_id)
            .bind(l.fingerprint.as_deref())
            .bind(&series)
            .bind(clip(by, 200))
            .execute(&mut *tx)
            .await?;
            have += 1;
            added += 1;
        }
        tx.commit().await?;
        Ok(added)
    }

    /// Results of the project with these ids, at most 1000, by id.
    pub async fn issue_targets(
        &self,
        project_id: Uuid,
        ids: &[Uuid],
    ) -> anyhow::Result<Vec<IssueTarget>> {
        let rows = sqlx::query(
            "select id, launch_id, series_id, name, full_name from results \
             where project_id = $1 and id = any($2) order by id limit 1000",
        )
        .bind(project_id)
        .bind(ids)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(target_row).collect()
    }

    /// Results of a launch with a fingerprint, at most [`MAX_LINK_SERIES`], by name.
    pub async fn cluster_targets(
        &self,
        project_id: Uuid,
        launch_id: Uuid,
        fingerprint: &str,
    ) -> anyhow::Result<Vec<IssueTarget>> {
        let rows = sqlx::query(
            "select r.id, r.launch_id, r.series_id, r.name, r.full_name from results r join launches l \
               on l.id = r.launch_id and l.created_at = r.launch_created_at \
             where l.project_id = $1 and l.id = $2 and r.fingerprint = $3 \
             order by left(r.full_name, 600), r.full_name, r.id limit $4",
        )
        .bind(project_id)
        .bind(launch_id)
        .bind(fingerprint)
        .bind(MAX_LINK_SERIES)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(target_row).collect()
    }

    /// Stops tracking an issue (its links go with it). Defects keep their issue link.
    pub async fn delete_tracker_issue(&self, project_id: Uuid, id: Uuid) -> anyhow::Result<bool> {
        let n = sqlx::query("delete from tracker_issues where project_id = $1 and id = $2")
            .bind(project_id)
            .bind(id)
            .execute(&self.pool)
            .await?
            .rows_affected();
        Ok(n > 0)
    }

    /// Clears the un-mute suggestion.
    pub async fn dismiss_unmute_suggestion(
        &self,
        project_id: Uuid,
        id: Uuid,
    ) -> anyhow::Result<bool> {
        let n = sqlx::query(
            "update tracker_issues set unmute_suggested = false, updated_at = now() \
             where project_id = $1 and id = $2",
        )
        .bind(project_id)
        .bind(id)
        .execute(&self.pool)
        .await?
        .rows_affected();
        Ok(n > 0)
    }

    /// Removes the mutes of series (those whose results are gone are not reached by triage).
    pub async fn delete_series_mutes(
        &self,
        project_id: Uuid,
        series: &[Uuid],
    ) -> anyhow::Result<u64> {
        Ok(
            sqlx::query("delete from series_mutes where project_id = $1 and series_id = any($2)")
                .bind(project_id)
                .bind(series)
                .execute(&self.pool)
                .await?
                .rows_affected(),
        )
    }

    /// Leases one issue for a read asked for by a user: not while another read holds it, not
    /// within `min_gap` of the last attempt, and not while a failing issue backs off.
    pub async fn lease_tracker_read(
        &self,
        project_id: Uuid,
        id: Uuid,
        min_gap: Duration,
        lease: Duration,
    ) -> anyhow::Result<ReadNow> {
        let r = sqlx::query(sqlx::AssertSqlSafe(format!(
            "update tracker_issues i set lease_id = gen_random_uuid(), \
               lease_until = now() + make_interval(secs => $4), last_attempt_at = now() \
             from projects p where p.id = i.project_id and i.project_id = $1 and i.id = $2 \
               and i.issue_key is not null and i.category in ('open', 'closed') \
               and (i.lease_until is null or i.lease_until < now()) \
               and (i.last_attempt_at is null or i.last_attempt_at < now() - make_interval(secs => $3)) \
               and (i.sync_attempts = 0 or i.next_sync_at is null or i.next_sync_at <= now()) \
             returning {JOB_COLUMNS}"
        )))
        .bind(project_id)
        .bind(id)
        .bind(min_gap.as_secs_f64())
        .bind(lease.as_secs_f64())
        .fetch_optional(&self.pool)
        .await?;
        if let Some(r) = r {
            return Ok(ReadNow::Job(Box::new(sync_row(&r)?)));
        }
        let wait: Option<Option<f64>> = sqlx::query_scalar(
            "select extract(epoch from greatest( \
                 coalesce(lease_until, now()), \
                 coalesce(last_attempt_at + make_interval(secs => $3), now()), \
                 case when sync_attempts > 0 then coalesce(next_sync_at, now()) else now() end) - now())::float8 \
             from tracker_issues where project_id = $1 and id = $2 and issue_key is not null \
               and category in ('open', 'closed')",
        )
        .bind(project_id)
        .bind(id)
        .bind(min_gap.as_secs_f64())
        .fetch_optional(&self.pool)
        .await?;
        Ok(match wait {
            None => ReadNow::NotFound,
            Some(w) => ReadNow::Later(w.unwrap_or(1.0).ceil().max(1.0) as i64),
        })
    }

    /// A webhook names an issue (by key or tracker id): it is read soon, at most once per
    /// `min_gap`, unless a read holds it or it backs off after failures.
    pub async fn request_tracker_sync_by_ref(
        &self,
        project_id: Uuid,
        tracker_id: &str,
        refs: &[String],
        min_gap: Duration,
    ) -> anyhow::Result<u64> {
        if refs.is_empty() {
            return Ok(0);
        }
        Ok(sqlx::query(
            "update tracker_issues set next_sync_at = least(coalesce(next_sync_at, 'infinity'), \
               greatest(now(), coalesce(last_attempt_at, now()) + make_interval(secs => $4))) \
             where project_id = $1 and tracker_id = $2 and category in ('open', 'closed') \
               and sync_attempts = 0 and (lease_until is null or lease_until < now()) \
               and (issue_key = any($3) or remote_id = any($3))",
        )
        .bind(project_id)
        .bind(tracker_id)
        .bind(refs)
        .bind(min_gap.as_secs_f64())
        .execute(&self.pool)
        .await?
        .rows_affected())
    }

    /// Claims issues due for a read (one replica each, until the lease ends), at most
    /// `per_tracker` of one project's tracker per round so that one tracker cannot fill it.
    pub async fn claim_tracker_syncs(
        &self,
        limit: i64,
        per_tracker: i64,
        lease: Duration,
    ) -> anyhow::Result<Vec<SyncJob>> {
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "update tracker_issues i set lease_id = gen_random_uuid(), \
               lease_until = now() + make_interval(secs => $2), last_attempt_at = now() \
             from projects p where p.id = i.project_id and i.id in ( \
               select id from tracker_issues where id in ( \
                 select id from (select id, next_sync_at, row_number() over \
                     (partition by project_id, tracker_id order by next_sync_at) as n \
                   from (select id, project_id, tracker_id, next_sync_at from tracker_issues \
                     where next_sync_at <= now() and category in ('open', 'closed') and issue_key is not null \
                       and (lease_until is null or lease_until < now()) \
                     order by next_sync_at limit 2000) due) ranked \
                 where n <= $3 order by next_sync_at limit $1) \
               and (lease_until is null or lease_until < now()) \
               for update skip locked) \
             returning {JOB_COLUMNS}"
        )))
        .bind(limit)
        .bind(lease.as_secs_f64())
        .bind(per_tracker)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(sync_row).collect()
    }

    /// Stores the outcome of a read; `next` is when to read again after a success. When the
    /// issue becomes closed and linked tests are still muted, it suggests un-muting them.
    /// Returns `true` when the issue was closed by this read.
    pub async fn finish_tracker_sync(
        &self,
        job: &SyncJob,
        outcome: &SyncOutcome,
        next: Option<DateTime<Utc>>,
    ) -> anyhow::Result<bool> {
        match outcome {
            SyncOutcome::Failed(err, retry_at) => {
                sqlx::query(
                    "update tracker_issues set sync_error = $2, sync_attempts = sync_attempts + 1, \
                       next_sync_at = $3, lease_until = null, lease_id = null where id = $1 and lease_id = $4",
                )
                .bind(job.id)
                .bind(clip(err, 500))
                .bind(retry_at)
                .bind(job.lease_id)
                .execute(&self.pool)
                .await?;
                Ok(false)
            }
            SyncOutcome::Unchanged => {
                sqlx::query(
                    "update tracker_issues set synced_at = now(), sync_error = null, sync_attempts = 0, \
                       next_sync_at = $2, lease_until = null, lease_id = null where id = $1 and lease_id = $3",
                )
                .bind(job.id)
                .bind(next)
                .bind(job.lease_id)
                .execute(&self.pool)
                .await?;
                Ok(false)
            }
            SyncOutcome::Read(s, etag) => {
                let closes = s.closed && !job.closed;
                let mut suggest = false;
                if closes {
                    let links = self.tracker_links(job.id).await?;
                    suggest = !self
                        .muted_linked_series(job.project_id, &links)
                        .await?
                        .is_empty();
                }
                sqlx::query(
                    "update tracker_issues set title = $2, state = $3, \
                       category = case when $4 then 'closed' else 'open' end, \
                       closed_at = case when $4 then coalesce(closed_at, now()) end, \
                       unmute_suggested = case when $5 then true when $4 then unmute_suggested else false end, \
                       url = case when $6 = '' then url else $6 end, etag = $7, synced_at = now(), \
                       sync_error = null, sync_attempts = 0, next_sync_at = $8, lease_until = null, \
                       lease_id = null, \
                       updated_at = case when state is distinct from $3 or title is distinct from $2 \
                         then now() else updated_at end \
                     where id = $1 and lease_id = $9",
                )
                .bind(job.id)
                .bind(clip(&s.title, 500))
                .bind(clip(&s.state, 100))
                .bind(s.closed)
                .bind(suggest)
                .bind(clip(&s.url, 2048))
                .bind(etag.as_deref().map(|e| clip(e, 200)))
                .bind(next)
                .bind(job.lease_id)
                .execute(&self.pool)
                .await?;
                Ok(closes)
            }
        }
    }
}
