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
    pub attempts: i32,
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
        attempts: r.try_get("sync_attempts")?,
    })
}

fn clip(s: &str, max: usize) -> String {
    let s = no_nul(s);
    match s.char_indices().nth(max) {
        Some((i, _)) => s[..i].to_string(),
        None => s,
    }
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
               and (cardinality($2::text[]) = 0 or url = any($2)) \
               and (cardinality($3::text[]) = 0 or issue_key = any($3)) \
               and ($4::text is null or tracker_id = $4) \
               and ($5::text is null or category = $5) \
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

    /// Issues of the project and issues created in it since `since` (for limits).
    pub async fn tracker_issue_counts(
        &self,
        project_id: Uuid,
        since: DateTime<Utc>,
    ) -> anyhow::Result<(i64, i64)> {
        let r = sqlx::query(
            "select count(*) as total, count(*) filter (where created_at >= $2) as recent \
             from tracker_issues where project_id = $1",
        )
        .bind(project_id)
        .bind(since)
        .fetch_one(&self.pool)
        .await?;
        Ok((r.try_get("total")?, r.try_get("recent")?))
    }

    /// Starts a create: inserts a `creating` row for the request id, or returns the issue a
    /// request with this id already made (a retried request creates nothing twice).
    pub async fn begin_tracker_issue(
        &self,
        project_id: Uuid,
        tracker_id: &str,
        tracker_type: &str,
        request_id: Uuid,
        by: &str,
    ) -> anyhow::Result<Begin> {
        let id = Uuid::now_v7();
        let inserted: Option<Uuid> = sqlx::query_scalar(
            "insert into tracker_issues (id, project_id, tracker_id, tracker_type, request_id, created_by) \
             values ($1, $2, $3, $4, $5, $6) on conflict (project_id, request_id) do nothing returning id",
        )
        .bind(id)
        .bind(project_id)
        .bind(tracker_id)
        .bind(tracker_type)
        .bind(request_id)
        .bind(clip(by, 200))
        .fetch_optional(&self.pool)
        .await?;
        if let Some(id) = inserted {
            return Ok(Begin::New(id));
        }
        let r = sqlx::query(sqlx::AssertSqlSafe(format!(
            "select {COLUMNS} from tracker_issues where project_id = $1 and request_id = $2"
        )))
        .bind(project_id)
        .bind(request_id)
        .fetch_one(&self.pool)
        .await?;
        Ok(Begin::Existing(Box::new(issue_row(&r)?)))
    }

    /// Drops a `creating` row whose create failed.
    pub async fn abandon_tracker_issue(&self, id: Uuid) -> anyhow::Result<()> {
        sqlx::query("delete from tracker_issues where id = $1 and category = 'creating'")
            .bind(id)
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
                self.abandon_tracker_issue(id).await?;
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
             order by r.full_name, r.id limit $4",
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

    /// Asks for a read of an issue soon: now, or `min_gap` after the last read. `false` when
    /// the issue is not tracked or still being created.
    pub async fn request_tracker_sync(
        &self,
        project_id: Uuid,
        id: Uuid,
        min_gap: Duration,
    ) -> anyhow::Result<bool> {
        let n = sqlx::query(
            "update tracker_issues set next_sync_at = greatest(now(), \
               coalesce(synced_at, now()) + make_interval(secs => $3)) \
             where project_id = $1 and id = $2 and category <> 'creating'",
        )
        .bind(project_id)
        .bind(id)
        .bind(min_gap.as_secs_f64())
        .execute(&self.pool)
        .await?
        .rows_affected();
        Ok(n > 0)
    }

    /// The same for a webhook that names an issue by key or tracker id.
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
            "update tracker_issues set next_sync_at = greatest(now(), \
               coalesce(synced_at, now()) + make_interval(secs => $4)) \
             where project_id = $1 and tracker_id = $2 and category <> 'creating' \
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

    /// The read of one issue (for a read asked for by a user), without claiming it.
    pub async fn tracker_sync_job(
        &self,
        project_id: Uuid,
        id: Uuid,
    ) -> anyhow::Result<Option<SyncJob>> {
        let r = sqlx::query(
            "select i.id, i.project_id, p.key, i.tracker_id, i.tracker_type, i.issue_key, i.remote_id, \
               i.etag, i.category, i.sync_attempts from tracker_issues i join projects p on p.id = i.project_id \
             where i.project_id = $1 and i.id = $2 and i.issue_key is not null and i.category <> 'creating'",
        )
        .bind(project_id)
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        r.as_ref().map(sync_row).transpose()
    }

    /// Claims issues due for a read (one replica each, until the lease ends).
    pub async fn claim_tracker_syncs(
        &self,
        limit: i64,
        lease: Duration,
    ) -> anyhow::Result<Vec<SyncJob>> {
        let rows = sqlx::query(
            "update tracker_issues i set lease_until = now() + make_interval(secs => $2), \
               next_sync_at = now() + make_interval(secs => $2) \
             from projects p where p.id = i.project_id and i.id in (select id from tracker_issues \
               where next_sync_at <= now() and category <> 'creating' and issue_key is not null \
               order by next_sync_at limit $1 for update skip locked) \
             returning i.id, i.project_id, p.key, i.tracker_id, i.tracker_type, i.issue_key, i.remote_id, \
               i.etag, i.category, i.sync_attempts",
        )
        .bind(limit)
        .bind(lease.as_secs_f64())
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
                       next_sync_at = $3, lease_until = null where id = $1",
                )
                .bind(job.id)
                .bind(clip(err, 500))
                .bind(retry_at)
                .execute(&self.pool)
                .await?;
                Ok(false)
            }
            SyncOutcome::Unchanged => {
                sqlx::query(
                    "update tracker_issues set synced_at = now(), sync_error = null, sync_attempts = 0, \
                       next_sync_at = $2, lease_until = null where id = $1",
                )
                .bind(job.id)
                .bind(next)
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
                       updated_at = case when state is distinct from $3 or title is distinct from $2 \
                         then now() else updated_at end \
                     where id = $1",
                )
                .bind(job.id)
                .bind(clip(&s.title, 500))
                .bind(clip(&s.state, 100))
                .bind(s.closed)
                .bind(suggest)
                .bind(clip(&s.url, 2048))
                .bind(etag.as_deref().map(|e| clip(e, 200)))
                .bind(next)
                .execute(&self.pool)
                .await?;
                Ok(closes)
            }
        }
    }
}
