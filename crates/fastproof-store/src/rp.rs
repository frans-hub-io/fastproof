//! The working set of the ReportPortal endpoint (`rp_launches`, `rp_items`): the tree of items
//! of open ReportPortal launches until their results are emitted to the native ingest. Not a
//! history: rows go a day after their launch finished. Every lookup is scoped by project.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use sqlx::Row;
use sqlx::postgres::PgRow;
use uuid::Uuid;

use crate::{Store, no_nul};

/// A ReportPortal launch: the ReportPortal uuid of a FastProof launch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RpLaunch {
    pub project_id: Uuid,
    pub uuid: String,
    pub launch_id: Uuid,
    pub creator: String,
    pub finish_requested: bool,
    pub done: bool,
}

fn launch_row(r: &PgRow) -> Result<RpLaunch, sqlx::Error> {
    let req: Option<DateTime<Utc>> = r.try_get("finish_requested_at")?;
    let done: Option<DateTime<Utc>> = r.try_get("done_at")?;
    Ok(RpLaunch {
        project_id: r.try_get("project_id")?,
        uuid: r.try_get("uuid")?,
        launch_id: r.try_get("launch_id")?,
        creator: r.try_get("creator")?,
        finish_requested: req.is_some(),
        done: done.is_some(),
    })
}

const LAUNCH_COLUMNS: &str = "project_id, uuid, launch_id, creator, finish_requested_at, done_at";

/// A row of `rp_items`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RpItem {
    pub project_id: Uuid,
    pub uuid: String,
    pub launch_id: Uuid,
    pub rp_launch: String,
    pub num_id: i64,
    pub parent_uuid: Option<String>,
    pub role: String,
    pub depth: i32,
    pub owner_uuid: Option<String>,
    pub log_key: String,
    pub log_step: Option<String>,
    pub seq_uuid: Option<String>,
    pub error_uuid: Option<String>,
    pub suite_path: serde_json::Value,
    pub item_type: String,
    pub has_stats: bool,
    pub name: String,
    pub code_ref: Option<String>,
    pub test_case_id: Option<String>,
    pub description: Option<String>,
    pub params: serde_json::Value,
    pub attrs: serde_json::Value,
    pub start_ms: i64,
    pub stop_ms: Option<i64>,
    pub status: Option<String>,
    pub issue: Option<serde_json::Value>,
    pub attachments: serde_json::Value,
    pub error: Option<serde_json::Value>,
    pub retry: bool,
    pub retry_of: Option<String>,
    pub running_sent: bool,
    pub state: String,
    pub triage_by: Option<String>,
    pub created_by: String,
}

const ITEM_COLUMNS: &str = "project_id, uuid, launch_id, rp_launch, num_id, parent_uuid, role, depth, \
    owner_uuid, log_key, log_step, seq_uuid, error_uuid, suite_path, item_type, has_stats, name, code_ref, \
    test_case_id, description, params, attrs, start_ms, stop_ms, status, issue, attachments, error, retry, \
    retry_of, running_sent, state, triage_by, created_by";

fn item_row(r: &PgRow) -> Result<RpItem, sqlx::Error> {
    Ok(RpItem {
        project_id: r.try_get("project_id")?,
        uuid: r.try_get("uuid")?,
        launch_id: r.try_get("launch_id")?,
        rp_launch: r.try_get("rp_launch")?,
        num_id: r.try_get("num_id")?,
        parent_uuid: r.try_get("parent_uuid")?,
        role: r.try_get("role")?,
        depth: r.try_get("depth")?,
        owner_uuid: r.try_get("owner_uuid")?,
        log_key: r.try_get("log_key")?,
        log_step: r.try_get("log_step")?,
        seq_uuid: r.try_get("seq_uuid")?,
        error_uuid: r.try_get("error_uuid")?,
        suite_path: r.try_get("suite_path")?,
        item_type: r.try_get("item_type")?,
        has_stats: r.try_get("has_stats")?,
        name: r.try_get("name")?,
        code_ref: r.try_get("code_ref")?,
        test_case_id: r.try_get("test_case_id")?,
        description: r.try_get("description")?,
        params: r.try_get("params")?,
        attrs: r.try_get("attrs")?,
        start_ms: r.try_get("start_ms")?,
        stop_ms: r.try_get("stop_ms")?,
        status: r.try_get("status")?,
        issue: r.try_get("issue")?,
        attachments: r.try_get("attachments")?,
        error: r.try_get("error")?,
        retry: r.try_get("retry")?,
        retry_of: r.try_get("retry_of")?,
        running_sent: r.try_get("running_sent")?,
        state: r.try_get("state")?,
        triage_by: r.try_get("triage_by")?,
        created_by: r.try_get("created_by")?,
    })
}

fn opt(s: &Option<String>) -> Option<String> {
    s.as_deref().map(no_nul)
}

/// Where an item's placement changes when its pending parent arrives.
#[derive(Clone, Debug, Default)]
pub struct Placement {
    pub role: String,
    pub depth: i32,
    pub owner_uuid: Option<String>,
    pub log_key: String,
    pub log_step: Option<String>,
    pub seq_uuid: Option<String>,
    pub error_uuid: Option<String>,
    pub suite_path: serde_json::Value,
}

/// A finish request of an item.
#[derive(Clone, Debug, Default)]
pub struct ItemFinish {
    pub stop_ms: i64,
    pub status: Option<String>,
    pub issue: Option<serde_json::Value>,
    pub description: Option<String>,
    pub attrs: Option<serde_json::Value>,
    pub test_case_id: Option<String>,
    pub retry: bool,
    pub retry_of: Option<String>,
    pub triage_by: Option<String>,
    /// The state the item takes: `finished` (results and failed fixtures wait to settle),
    /// `emitted` (nothing to emit), `pending` (the parent did not arrive yet).
    pub state: String,
    /// Settle delay of results, in milliseconds.
    pub settle_ms: i64,
}

/// A ReportPortal launch whose finish was requested and that is not done.
#[derive(Clone, Debug)]
pub struct Finishing {
    pub project_id: Uuid,
    pub project_key: String,
    pub uuid: String,
    pub launch_id: Uuid,
    pub status: Option<String>,
    pub force: bool,
    pub requested_at: DateTime<Utc>,
}

impl Store {
    pub async fn rp_launch(
        &self,
        project_id: Uuid,
        uuid: &str,
    ) -> anyhow::Result<Option<RpLaunch>> {
        let r = sqlx::query(sqlx::AssertSqlSafe(format!(
            "select {LAUNCH_COLUMNS} from rp_launches where project_id = $1 and uuid = $2"
        )))
        .bind(project_id)
        .bind(no_nul(uuid))
        .fetch_optional(&self.pool)
        .await?;
        Ok(r.as_ref().map(launch_row).transpose()?)
    }

    /// The ReportPortal launches of a FastProof launch.
    pub async fn rp_launches_of(&self, launch_id: Uuid) -> anyhow::Result<Vec<RpLaunch>> {
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "select {LAUNCH_COLUMNS} from rp_launches where launch_id = $1 order by created_at limit 100"
        )))
        .bind(launch_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.iter().map(launch_row).collect::<Result<_, _>>()?)
    }

    /// Records the ReportPortal uuid of a launch (the existing row wins).
    pub async fn rp_launch_insert(
        &self,
        project_id: Uuid,
        uuid: &str,
        launch_id: Uuid,
        creator: &str,
    ) -> anyhow::Result<RpLaunch> {
        sqlx::query(
            "insert into rp_launches (project_id, uuid, launch_id, creator) values ($1, $2, $3, $4) \
             on conflict (project_id, uuid) do nothing",
        )
        .bind(project_id)
        .bind(no_nul(uuid))
        .bind(launch_id)
        .bind(no_nul(creator))
        .execute(&self.pool)
        .await?;
        self.rp_launch(project_id, uuid)
            .await?
            .ok_or_else(|| anyhow::anyhow!("rp launch vanished after insert"))
    }

    /// Notes calls into a launch: `items` more items (refused, `None`, past `max`), and the
    /// time of the last call (for the idle timeout).
    pub async fn rp_launch_touch(
        &self,
        project_id: Uuid,
        uuid: &str,
        items: i64,
        max: i64,
    ) -> anyhow::Result<Option<i64>> {
        Ok(sqlx::query_scalar(
            "update rp_launches set item_count = item_count + $3, last_call_at = now() \
             where project_id = $1 and uuid = $2 and item_count + $3 <= $4 returning item_count",
        )
        .bind(project_id)
        .bind(no_nul(uuid))
        .bind(items)
        .bind(max)
        .fetch_optional(&self.pool)
        .await?)
    }

    /// Asks to finish a launch; the scheduler finishes it once its items settled.
    pub async fn rp_request_finish(
        &self,
        project_id: Uuid,
        uuid: &str,
        status: Option<&str>,
        force: bool,
    ) -> anyhow::Result<bool> {
        let r = sqlx::query(
            "update rp_launches set finish_requested_at = coalesce(finish_requested_at, now()), \
               finish_status = coalesce($3, finish_status), finish_force = finish_force or $4, last_call_at = now() \
             where project_id = $1 and uuid = $2",
        )
        .bind(project_id)
        .bind(no_nul(uuid))
        .bind(status.map(no_nul))
        .bind(force)
        .execute(&self.pool)
        .await?;
        Ok(r.rows_affected() > 0)
    }

    /// Opens a finished launch again for a ReportPortal rerun.
    pub async fn rp_reopen(&self, project_id: Uuid, launch_id: Uuid) -> anyhow::Result<()> {
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "update rp_launches set finish_requested_at = null, finish_status = null, finish_force = false, \
               done_at = null, hold = false, last_call_at = now() where project_id = $1 and launch_id = $2",
        )
        .bind(project_id)
        .bind(launch_id)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "update launches set status = 'running', finished_at = null, updated_at = now() \
             where project_id = $1 and id = $2",
        )
        .bind(project_id)
        .bind(launch_id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Sets the start of a launch (the start time of a ReportPortal launch, for imports).
    pub async fn rp_set_started_at(
        &self,
        project_id: Uuid,
        launch_id: Uuid,
        at: DateTime<Utc>,
    ) -> anyhow::Result<()> {
        sqlx::query("update launches set started_at = $3 where project_id = $1 and id = $2")
            .bind(project_id)
            .bind(launch_id)
            .bind(at)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// The newest launch of a project with this name created by `creator` (reruns).
    pub async fn rp_latest_launch_named(
        &self,
        project_id: Uuid,
        name: &str,
    ) -> anyhow::Result<Option<Uuid>> {
        Ok(sqlx::query_scalar(
            "select id from launches where project_id = $1 and name = $2 and merged_into is null \
             order by number desc limit 1",
        )
        .bind(project_id)
        .bind(no_nul(name))
        .fetch_optional(&self.pool)
        .await?)
    }

    /// The launch with this number in a project.
    pub async fn rp_launch_by_number(
        &self,
        project_id: Uuid,
        number: i64,
    ) -> anyhow::Result<Option<Uuid>> {
        Ok(
            sqlx::query_scalar("select id from launches where project_id = $1 and number = $2")
                .bind(project_id)
                .bind(number)
                .fetch_optional(&self.pool)
                .await?,
        )
    }

    /// The key of a project that is not deleted.
    pub async fn rp_project_key(&self, project_id: Uuid) -> anyhow::Result<Option<String>> {
        Ok(
            sqlx::query_scalar("select key from projects where id = $1 and deleted_at is null")
                .bind(project_id)
                .fetch_optional(&self.pool)
                .await?,
        )
    }

    /// Who created a launch.
    pub async fn launch_creator(&self, launch_id: Uuid) -> anyhow::Result<Option<String>> {
        Ok(
            sqlx::query_scalar("select created_by from launches where id = $1")
                .bind(launch_id)
                .fetch_optional(&self.pool)
                .await?
                .flatten(),
        )
    }

    pub async fn rp_item(&self, project_id: Uuid, uuid: &str) -> anyhow::Result<Option<RpItem>> {
        let r = sqlx::query(sqlx::AssertSqlSafe(format!(
            "select {ITEM_COLUMNS} from rp_items where project_id = $1 and uuid = $2"
        )))
        .bind(project_id)
        .bind(no_nul(uuid))
        .fetch_optional(&self.pool)
        .await?;
        Ok(r.as_ref().map(item_row).transpose()?)
    }

    pub async fn rp_item_by_num(
        &self,
        project_id: Uuid,
        num: i64,
    ) -> anyhow::Result<Option<RpItem>> {
        let r = sqlx::query(sqlx::AssertSqlSafe(format!(
            "select {ITEM_COLUMNS} from rp_items where project_id = $1 and num_id = $2"
        )))
        .bind(project_id)
        .bind(num)
        .fetch_optional(&self.pool)
        .await?;
        Ok(r.as_ref().map(item_row).transpose()?)
    }

    /// Inserts an item; when one with the uuid exists, returns that one (`false`).
    pub async fn rp_item_insert(&self, i: &RpItem) -> anyhow::Result<(RpItem, bool)> {
        let r = sqlx::query(sqlx::AssertSqlSafe(format!(
            "insert into rp_items (project_id, uuid, launch_id, rp_launch, parent_uuid, role, depth, owner_uuid, \
               log_key, log_step, seq_uuid, error_uuid, suite_path, item_type, has_stats, name, code_ref, \
               test_case_id, description, params, attrs, start_ms, retry, retry_of, running_sent, state, created_by) \
             values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19, $20, \
               $21, $22, $23, $24, $25, $26, $27) \
             on conflict (project_id, uuid) do nothing returning {ITEM_COLUMNS}"
        )))
        .bind(i.project_id)
        .bind(no_nul(&i.uuid))
        .bind(i.launch_id)
        .bind(no_nul(&i.rp_launch))
        .bind(opt(&i.parent_uuid))
        .bind(&i.role)
        .bind(i.depth)
        .bind(opt(&i.owner_uuid))
        .bind(no_nul(&i.log_key))
        .bind(opt(&i.log_step))
        .bind(opt(&i.seq_uuid))
        .bind(opt(&i.error_uuid))
        .bind(&i.suite_path)
        .bind(no_nul(&i.item_type))
        .bind(i.has_stats)
        .bind(no_nul(&i.name))
        .bind(opt(&i.code_ref))
        .bind(opt(&i.test_case_id))
        .bind(opt(&i.description))
        .bind(&i.params)
        .bind(&i.attrs)
        .bind(i.start_ms)
        .bind(i.retry)
        .bind(opt(&i.retry_of))
        .bind(i.running_sent)
        .bind(&i.state)
        .bind(no_nul(&i.created_by))
        .fetch_optional(&self.pool)
        .await?;
        match r {
            Some(r) => Ok((item_row(&r)?, true)),
            None => Ok((
                self.rp_item(i.project_id, &i.uuid)
                    .await?
                    .ok_or_else(|| anyhow::anyhow!("rp item vanished after a conflict"))?,
                false,
            )),
        }
    }

    /// A result turns out to have results under it: it becomes a container. Returns whether it
    /// was a result whose `running` line was sent (that line then needs an ending).
    pub async fn rp_item_to_container(
        &self,
        project_id: Uuid,
        uuid: &str,
    ) -> anyhow::Result<Option<bool>> {
        Ok(sqlx::query_scalar(
            "update rp_items set role = 'container', log_key = 'rp-container:' || uuid, error_uuid = null \
             where project_id = $1 and uuid = $2 and role = 'result' returning running_sent",
        )
        .bind(project_id)
        .bind(no_nul(uuid))
        .fetch_optional(&self.pool)
        .await?)
    }

    /// Notes that the `running` line of a result was sent.
    pub async fn rp_running_sent(&self, project_id: Uuid, uuid: &str) -> anyhow::Result<()> {
        sqlx::query("update rp_items set running_sent = true where project_id = $1 and uuid = $2")
            .bind(project_id)
            .bind(no_nul(uuid))
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Finishes an item that is started (or pending). `None`: no such item, or it was
    /// finished before (a repeated finish is a no-op).
    pub async fn rp_item_finish(
        &self,
        project_id: Uuid,
        uuid: &str,
        f: &ItemFinish,
    ) -> anyhow::Result<Option<RpItem>> {
        let r = sqlx::query(sqlx::AssertSqlSafe(format!(
            "update rp_items set stop_ms = greatest($3, start_ms), status = $4, issue = $5, \
               description = coalesce($6, description), attrs = coalesce($7, attrs), \
               test_case_id = coalesce(test_case_id, $8), retry = retry or $9, retry_of = coalesce(retry_of, $10), \
               triage_by = $11, state = $12, \
               settle_at = case when $12 = 'finished' and role in ('result', 'fixture') then \
                 case when exists (select 1 from rp_launches l where l.project_id = $1 and l.uuid = rp_items.rp_launch and l.hold) \
                   then 'infinity'::timestamptz else now() + make_interval(secs => $13::double precision / 1000) end \
                 else null end, \
               emitted_at = case when $12 = 'emitted' then now() else null end \
             where project_id = $1 and uuid = $2 and (state = 'started' or (state = 'pending' and stop_ms is null)) \
             returning {ITEM_COLUMNS}"
        )))
        .bind(project_id)
        .bind(no_nul(uuid))
        .bind(f.stop_ms)
        .bind(opt(&f.status))
        .bind(&f.issue)
        .bind(opt(&f.description))
        .bind(&f.attrs)
        .bind(opt(&f.test_case_id))
        .bind(f.retry)
        .bind(opt(&f.retry_of))
        .bind(opt(&f.triage_by))
        .bind(&f.state)
        .bind(f.settle_ms)
        .fetch_optional(&self.pool)
        .await?;
        Ok(r.as_ref().map(item_row).transpose()?)
    }

    /// Changes the description and attributes of an item that was not emitted yet.
    pub async fn rp_item_update(
        &self,
        project_id: Uuid,
        uuid: &str,
        description: Option<&str>,
        attrs: Option<&serde_json::Value>,
    ) -> anyhow::Result<bool> {
        let r = sqlx::query(
            "update rp_items set description = coalesce($3, description), attrs = coalesce($4, attrs) \
             where project_id = $1 and uuid = $2 and state not in ('emitting', 'emitted')",
        )
        .bind(project_id)
        .bind(no_nul(uuid))
        .bind(description.map(no_nul))
        .bind(attrs)
        .execute(&self.pool)
        .await?;
        Ok(r.rows_affected() > 0)
    }

    /// Adds attachments (a JSON array) to an item. Returns false when the item is gone or its
    /// owner was emitted already (the attachment came too late).
    pub async fn rp_item_attach(
        &self,
        project_id: Uuid,
        uuid: &str,
        owner: Option<&str>,
        attached: &serde_json::Value,
    ) -> anyhow::Result<bool> {
        let r = sqlx::query(
            "update rp_items set attachments = attachments || $3 \
             where project_id = $1 and uuid = $2 and state not in ('emitting', 'emitted') \
               and ($4::text is null or not exists (select 1 from rp_items o where o.project_id = $1 \
                 and o.uuid = $4 and o.state in ('emitting', 'emitted')))",
        )
        .bind(project_id)
        .bind(no_nul(uuid))
        .bind(attached)
        .bind(owner.map(no_nul))
        .execute(&self.pool)
        .await?;
        Ok(r.rows_affected() > 0)
    }

    /// Reserves `n` log sequence numbers on each row; returns the first number of each range.
    /// Rows are locked in uuid order, so concurrent batches do not deadlock.
    pub async fn rp_reserve_seqs(
        &self,
        project_id: Uuid,
        wants: &[(String, i64)],
    ) -> anyhow::Result<HashMap<String, i64>> {
        if wants.is_empty() {
            return Ok(HashMap::new());
        }
        let mut sorted: Vec<&(String, i64)> = wants.iter().collect();
        sorted.sort_by(|a, b| a.0.cmp(&b.0));
        let uuids: Vec<String> = sorted.iter().map(|w| no_nul(&w.0)).collect();
        let ns: Vec<i64> = sorted.iter().map(|w| w.1).collect();
        let rows: Vec<(String, i64)> = sqlx::query_as(
            "with locked as (select uuid from rp_items where project_id = $1 and uuid = any($2) \
               order by uuid for update) \
             update rp_items r set log_seq = r.log_seq + v.n \
             from unnest($2::text[], $3::bigint[]) v(u, n), locked l \
             where r.project_id = $1 and r.uuid = v.u and l.uuid = v.u \
             returning r.uuid, r.log_seq - v.n",
        )
        .bind(project_id)
        .bind(&uuids)
        .bind(&ns)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().collect())
    }

    /// Reserves `n` log sequence numbers of a launch's own lines; returns the first.
    pub async fn rp_reserve_launch_seq(
        &self,
        project_id: Uuid,
        uuid: &str,
        n: i64,
    ) -> anyhow::Result<Option<i64>> {
        Ok(sqlx::query_scalar(
            "update rp_launches set log_seq = log_seq + $3 where project_id = $1 and uuid = $2 \
             returning log_seq - $3",
        )
        .bind(project_id)
        .bind(no_nul(uuid))
        .bind(n)
        .fetch_optional(&self.pool)
        .await?)
    }

    /// Lines of items: sets the error text of their result or fixture (for ERROR lines) and
    /// moves the settle time of finished ones `settle_ms` from now. Returns the items that
    /// were emitted already (the lines came late).
    pub async fn rp_note_lines(
        &self,
        project_id: Uuid,
        notes: &[(String, Option<serde_json::Value>)],
        settle_ms: i64,
    ) -> anyhow::Result<Vec<String>> {
        if notes.is_empty() {
            return Ok(Vec::new());
        }
        let mut sorted: Vec<&(String, Option<serde_json::Value>)> = notes.iter().collect();
        sorted.sort_by(|a, b| a.0.cmp(&b.0));
        let uuids: Vec<String> = sorted.iter().map(|w| no_nul(&w.0)).collect();
        let errs: Vec<Option<serde_json::Value>> = sorted.iter().map(|w| w.1.clone()).collect();
        let rows = sqlx::query(
            "with locked as (select uuid from rp_items where project_id = $1 and uuid = any($2) \
               and state not in ('emitting', 'emitted') order by uuid for update) \
             update rp_items r set error = coalesce(v.e, r.error), \
               settle_at = case when r.state = 'finished' and r.settle_at is not null \
                 then greatest(r.settle_at, now() + make_interval(secs => $4::double precision / 1000)) \
                 else r.settle_at end \
             from unnest($2::text[], $3::jsonb[]) v(u, e), locked l \
             where r.project_id = $1 and r.uuid = v.u and l.uuid = v.u returning r.uuid",
        )
        .bind(project_id)
        .bind(&uuids)
        .bind(&errs)
        .bind(settle_ms)
        .fetch_all(&self.pool)
        .await?;
        let open: std::collections::HashSet<String> = rows
            .into_iter()
            .map(|r| r.try_get::<String, _>(0))
            .collect::<Result<_, _>>()?;
        let missing: Vec<String> = uuids.into_iter().filter(|u| !open.contains(u)).collect();
        if missing.is_empty() {
            return Ok(missing);
        }
        Ok(sqlx::query_scalar(
            "select uuid from rp_items where project_id = $1 and uuid = any($2) and state in ('emitting', 'emitted') \
             and (role = 'result' or (role = 'fixture' and upper(coalesce(status, '')) in \
               ('FAILED', 'STOPPED', 'INTERRUPTED', 'CANCELLED')))",
        )
        .bind(project_id)
        .bind(&missing)
        .fetch_all(&self.pool)
        .await?)
    }

    /// Results of a launch wait for it to finish (agents known to send log lines late).
    pub async fn rp_set_hold(&self, project_id: Uuid, uuid: &str) -> anyhow::Result<()> {
        sqlx::query("update rp_launches set hold = true where project_id = $1 and uuid = $2")
            .bind(project_id)
            .bind(no_nul(uuid))
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// The launch of these items: lines or files came after a result was emitted (agents that
    /// batch log lines, like the Python and Robot ones, send them late). From now on its
    /// results wait for the launch to finish, so their errors and files land in them.
    pub async fn rp_hold(&self, project_id: Uuid, uuids: &[String]) -> anyhow::Result<u64> {
        if uuids.is_empty() {
            return Ok(0);
        }
        let uuids: Vec<String> = uuids.iter().map(|o| no_nul(o)).collect();
        let launches: Vec<Uuid> = sqlx::query_scalar(
            "update rp_launches l set hold = true from (select distinct launch_id, rp_launch from rp_items \
               where project_id = $1 and uuid = any($2)) i \
             where l.project_id = $1 and l.uuid = i.rp_launch and not l.hold returning l.launch_id",
        )
        .bind(project_id)
        .bind(&uuids)
        .fetch_all(&self.pool)
        .await?;
        if launches.is_empty() {
            return Ok(0);
        }
        sqlx::query(
            "update rp_items set settle_at = 'infinity'::timestamptz where launch_id = any($1) \
             and state = 'finished' and role in ('result', 'fixture')",
        )
        .bind(&launches)
        .execute(&self.pool)
        .await?;
        Ok(launches.len() as u64)
    }

    /// Items waiting for this parent.
    pub async fn rp_pending_children(
        &self,
        project_id: Uuid,
        parent: &str,
    ) -> anyhow::Result<Vec<RpItem>> {
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "select {ITEM_COLUMNS} from rp_items where project_id = $1 and parent_uuid = $2 \
             and state = 'pending' order by num_id limit 10000"
        )))
        .bind(project_id)
        .bind(no_nul(parent))
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.iter().map(item_row).collect::<Result<_, _>>()?)
    }

    /// Pending items older than `ttl_ms` (their parent never came), oldest first.
    pub async fn rp_expired_pending(&self, ttl_ms: i64, limit: i64) -> anyhow::Result<Vec<RpItem>> {
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "select {ITEM_COLUMNS} from rp_items where state = 'pending' \
             and created_at < now() - make_interval(secs => $1::double precision / 1000) \
             and (parent_uuid is null or not exists (select 1 from rp_items p where p.project_id = rp_items.project_id \
               and p.uuid = rp_items.parent_uuid)) order by created_at limit $2"
        )))
        .bind(ttl_ms)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.iter().map(item_row).collect::<Result<_, _>>()?)
    }

    /// Places a pending item: its role and log routing; it becomes started, or finished when it
    /// was finished while pending. `orphan` makes it a root (its parent never came).
    pub async fn rp_resolve_pending(
        &self,
        project_id: Uuid,
        uuid: &str,
        p: &Placement,
        orphan: bool,
        settle_ms: i64,
    ) -> anyhow::Result<Option<RpItem>> {
        let r = sqlx::query(sqlx::AssertSqlSafe(format!(
            "update rp_items set role = $3, depth = $4, owner_uuid = $5, log_key = $6, log_step = $7, seq_uuid = $8, \
               error_uuid = $9, suite_path = $10, parent_uuid = case when $11 then null else parent_uuid end, \
               state = case when stop_ms is null then 'started' \
                 when $3 in ('result', 'fixture') then 'finished' else 'emitted' end, \
               settle_at = case when stop_ms is not null and $3 in ('result', 'fixture') \
                 then now() + make_interval(secs => $12::double precision / 1000) else null end, \
               emitted_at = case when stop_ms is not null and $3 not in ('result', 'fixture') then now() else null end \
             where project_id = $1 and uuid = $2 and state = 'pending' returning {ITEM_COLUMNS}"
        )))
        .bind(project_id)
        .bind(no_nul(uuid))
        .bind(&p.role)
        .bind(p.depth)
        .bind(opt(&p.owner_uuid))
        .bind(no_nul(&p.log_key))
        .bind(opt(&p.log_step))
        .bind(opt(&p.seq_uuid))
        .bind(opt(&p.error_uuid))
        .bind(&p.suite_path)
        .bind(orphan)
        .bind(settle_ms)
        .fetch_optional(&self.pool)
        .await?;
        Ok(r.as_ref().map(item_row).transpose()?)
    }

    /// Claims settled results and failed fixtures (finished, settle time passed; or claimed
    /// before by a replica that stopped): they become `emitting` for `lease_ms`.
    pub async fn rp_claim_settled(
        &self,
        limit: i64,
        lease_ms: i64,
    ) -> anyhow::Result<Vec<(RpItem, f64)>> {
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "with due as (select project_id, uuid, settle_at from rp_items \
               where state in ('finished', 'emitting') and role in ('result', 'fixture') and settle_at < now() \
               order by settle_at limit $1 for update skip locked) \
             update rp_items r set state = 'emitting', settle_at = now() + make_interval(secs => $2::double precision / 1000) \
             from due where r.project_id = due.project_id and r.uuid = due.uuid \
             returning {}, extract(epoch from (now() - due.settle_at))::double precision as lag",
            ITEM_COLUMNS
                .split(", ")
                .map(|c| format!("r.{c}"))
                .collect::<Vec<_>>()
                .join(", ")
        )))
        .bind(limit)
        .bind(lease_ms)
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|r| Ok((item_row(r)?, r.try_get::<f64, _>("lag")?)))
            .collect()
    }

    /// Steps of these owners (results or fixtures), any depth.
    pub async fn rp_steps_of(
        &self,
        project_id: Uuid,
        owners: &[String],
    ) -> anyhow::Result<Vec<RpItem>> {
        if owners.is_empty() {
            return Ok(Vec::new());
        }
        let owners: Vec<String> = owners.iter().map(|o| no_nul(o)).collect();
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "select {ITEM_COLUMNS} from rp_items where project_id = $1 and owner_uuid = any($2) and role = 'step' \
             order by owner_uuid, start_ms, num_id limit 200000"
        )))
        .bind(project_id)
        .bind(&owners)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.iter().map(item_row).collect::<Result<_, _>>()?)
    }

    /// Items by uuid within a project (retried originals).
    pub async fn rp_items(
        &self,
        project_id: Uuid,
        uuids: &[String],
    ) -> anyhow::Result<Vec<RpItem>> {
        if uuids.is_empty() {
            return Ok(Vec::new());
        }
        let uuids: Vec<String> = uuids.iter().map(|o| no_nul(o)).collect();
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "select {ITEM_COLUMNS} from rp_items where project_id = $1 and uuid = any($2) limit 10000"
        )))
        .bind(project_id)
        .bind(&uuids)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.iter().map(item_row).collect::<Result<_, _>>()?)
    }

    /// Marks claimed items emitted.
    pub async fn rp_mark_emitted(&self, project_id: Uuid, uuids: &[String]) -> anyhow::Result<()> {
        if uuids.is_empty() {
            return Ok(());
        }
        let uuids: Vec<String> = uuids.iter().map(|o| no_nul(o)).collect();
        sqlx::query(
            "update rp_items set state = 'emitted', emitted_at = now(), settle_at = null \
             where project_id = $1 and uuid = any($2) and state = 'emitting'",
        )
        .bind(project_id)
        .bind(&uuids)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Claims ReportPortal launches whose finish was requested and that are not done, for
    /// `lease_ms` (one replica works on a launch at a time).
    pub async fn rp_claim_finishing(
        &self,
        limit: i64,
        lease_ms: i64,
    ) -> anyhow::Result<Vec<Finishing>> {
        let rows = sqlx::query(
            "with due as (select project_id, uuid from rp_launches \
               where finish_requested_at is not null and done_at is null \
                 and (claimed_until is null or claimed_until < now()) \
               order by finish_requested_at limit $1 for update skip locked) \
             update rp_launches r set claimed_until = now() + make_interval(secs => $2::double precision / 1000) \
             from due, projects p where r.project_id = due.project_id and r.uuid = due.uuid and p.id = r.project_id \
             returning r.project_id, p.key, r.uuid, r.launch_id, r.finish_status, r.finish_force, r.finish_requested_at",
        )
        .bind(limit)
        .bind(lease_ms)
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|r| {
                Ok(Finishing {
                    project_id: r.try_get(0)?,
                    project_key: r.try_get(1)?,
                    uuid: r.try_get(2)?,
                    launch_id: r.try_get(3)?,
                    status: r.try_get(4)?,
                    force: r.try_get(5)?,
                    requested_at: r.try_get(6)?,
                })
            })
            .collect()
    }

    /// Releases the claim of a finishing launch that is not done yet.
    pub async fn rp_release_finishing(&self, project_id: Uuid, uuid: &str) -> anyhow::Result<()> {
        sqlx::query(
            "update rp_launches set claimed_until = null where project_id = $1 and uuid = $2",
        )
        .bind(project_id)
        .bind(no_nul(uuid))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Results and failed fixtures of a launch asked to finish settle within `settle_ms`
    /// (held ones included).
    pub async fn rp_settle_by(&self, launch_id: Uuid, settle_ms: i64) -> anyhow::Result<()> {
        sqlx::query(
            "update rp_items set settle_at = least(settle_at, now() + make_interval(secs => $2::double precision / 1000)) \
             where launch_id = $1 and state = 'finished' and role in ('result', 'fixture')",
        )
        .bind(launch_id)
        .bind(settle_ms)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Results, fixtures and pending items of a launch that are not emitted yet.
    pub async fn rp_open_items(&self, launch_id: Uuid) -> anyhow::Result<i64> {
        Ok(sqlx::query_scalar(
            "select count(*) from rp_items where launch_id = $1 \
             and state in ('pending', 'started', 'finished', 'emitting') and role in ('result', 'fixture', 'pending')",
        )
        .bind(launch_id)
        .fetch_one(&self.pool)
        .await?)
    }

    /// Ends the open items of a launch that finishes: unfinished results and pending items
    /// become `emitting` (to be emitted as interrupted), finished ones settle now, unfinished
    /// fixtures and containers are done. Returns the items to emit as interrupted.
    pub async fn rp_interrupt_open(
        &self,
        launch_id: Uuid,
        limit: i64,
    ) -> anyhow::Result<Vec<RpItem>> {
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "update rp_items set settle_at = now() where launch_id = $1 and state = 'finished' \
             and role in ('result', 'fixture')",
        )
        .bind(launch_id)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "update rp_items set state = 'emitted', emitted_at = now() where launch_id = $1 \
             and state in ('started', 'pending') and role in ('fixture', 'container', 'step')",
        )
        .bind(launch_id)
        .execute(&mut *tx)
        .await?;
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "with open as (select project_id, uuid from rp_items where launch_id = $1 \
               and state in ('started', 'pending') and role in ('result', 'pending') order by num_id limit $2 for update skip locked) \
             update rp_items r set state = 'emitting', settle_at = now() + interval '1 minute' \
             from open where r.project_id = open.project_id and r.uuid = open.uuid returning {}",
            ITEM_COLUMNS
                .split(", ")
                .map(|c| format!("r.{c}"))
                .collect::<Vec<_>>()
                .join(", ")
        )))
        .bind(launch_id)
        .bind(limit)
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(rows.iter().map(item_row).collect::<Result<_, _>>()?)
    }

    /// Notes that a ReportPortal launch is done (its FastProof launch finished).
    pub async fn rp_mark_done(&self, project_id: Uuid, uuid: &str) -> anyhow::Result<()> {
        sqlx::query("update rp_launches set done_at = now() where project_id = $1 and uuid = $2")
            .bind(project_id)
            .bind(no_nul(uuid))
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Open ReportPortal launches without a call for `idle_secs`: their finish is requested as
    /// interrupted, without the grace period. Returns how many.
    pub async fn rp_expire_idle(&self, idle_secs: i64) -> anyhow::Result<u64> {
        let r = sqlx::query(
            "update rp_launches set finish_requested_at = now(), finish_status = 'INTERRUPTED', finish_force = true \
             where finish_requested_at is null and done_at is null \
               and last_call_at < now() - make_interval(secs => $1::double precision)",
        )
        .bind(idle_secs)
        .execute(&self.pool)
        .await?;
        Ok(r.rows_affected())
    }

    /// Deletes the working set of launches done more than `keep_secs` ago, at most `limit`
    /// items per call. Returns how many rows went.
    pub async fn rp_cleanup(&self, keep_secs: i64, limit: i64) -> anyhow::Result<u64> {
        let items = sqlx::query(
            "delete from rp_items where ctid = any(array(select i.ctid from rp_items i \
               join rp_launches l on l.project_id = i.project_id and l.uuid = i.rp_launch \
               where l.done_at < now() - make_interval(secs => $1::double precision) limit $2))",
        )
        .bind(keep_secs)
        .bind(limit)
        .execute(&self.pool)
        .await?
        .rows_affected();
        if items >= limit as u64 {
            return Ok(items);
        }
        let launches = sqlx::query(
            "delete from rp_launches l where l.done_at < now() - make_interval(secs => $1::double precision) \
             and not exists (select 1 from rp_items i where i.project_id = l.project_id and i.rp_launch = l.uuid)",
        )
        .bind(keep_secs)
        .execute(&self.pool)
        .await?
        .rows_affected();
        Ok(items + launches)
    }

    /// Claims emitted items whose issue waits to be applied as triage, for `lease_ms`, with
    /// their project keys.
    pub async fn rp_claim_triage(
        &self,
        limit: i64,
        lease_ms: i64,
    ) -> anyhow::Result<Vec<(RpItem, String)>> {
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "with due as (select project_id, uuid from rp_items where triage_by is not null and state = 'emitted' \
               and (settle_at is null or settle_at < now()) order by emitted_at limit $1 for update skip locked) \
             update rp_items r set settle_at = now() + make_interval(secs => $2::double precision / 1000) \
             from due, projects p where r.project_id = due.project_id and r.uuid = due.uuid and p.id = r.project_id \
             returning {}, p.key as project_key",
            ITEM_COLUMNS
                .split(", ")
                .map(|c| format!("r.{c}"))
                .collect::<Vec<_>>()
                .join(", ")
        )))
        .bind(limit)
        .bind(lease_ms)
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|r| Ok((item_row(r)?, r.try_get::<String, _>("project_key")?)))
            .collect()
    }

    /// The triage of an item was applied (or given up).
    pub async fn rp_triage_done(&self, project_id: Uuid, uuid: &str) -> anyhow::Result<()> {
        sqlx::query("update rp_items set triage_by = null where project_id = $1 and uuid = $2")
            .bind(project_id)
            .bind(no_nul(uuid))
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Seconds since an item was emitted.
    pub async fn rp_emitted_age(&self, project_id: Uuid, uuid: &str) -> anyhow::Result<f64> {
        Ok(sqlx::query_scalar(
            "select coalesce(extract(epoch from now() - emitted_at), 0)::double precision from rp_items \
             where project_id = $1 and uuid = $2",
        )
        .bind(project_id)
        .bind(no_nul(uuid))
        .fetch_optional(&self.pool)
        .await?
        .unwrap_or(0.0))
    }

    /// The result of the attempt with this client id in a launch.
    pub async fn result_of_client(
        &self,
        launch_id: Uuid,
        client_id: &str,
    ) -> anyhow::Result<Option<Uuid>> {
        Ok(sqlx::query_scalar(
            "select result_id from attempts where launch_id = $1 and client_key = $2 limit 1",
        )
        .bind(launch_id)
        .bind(format!("r:{}", no_nul(client_id)))
        .fetch_optional(&self.pool)
        .await?)
    }

    /// Items by state, for the `fastproof_rp_items` gauge, and pending items.
    pub async fn rp_item_counts(&self) -> anyhow::Result<Vec<(String, i64)>> {
        Ok(sqlx::query_as(
            "select state, count(*) from rp_items where role in ('result', 'fixture', 'pending') group by state",
        )
        .fetch_all(&self.pool)
        .await?)
    }

    /// The project whose `rp.aliases` setting lists `alias` (without case); `None` when no
    /// project or more than one does.
    pub async fn rp_project_by_alias(&self, alias: &str) -> anyhow::Result<Option<String>> {
        let keys: Vec<String> = sqlx::query_scalar(
            "select key from projects where deleted_at is null and settings ? 'rp' \
             and exists (select 1 from jsonb_array_elements_text(case when jsonb_typeof(settings->'rp'->'aliases') = 'array' \
               then settings->'rp'->'aliases' else '[]'::jsonb end) a where lower(a) = lower($1)) limit 2",
        )
        .bind(no_nul(alias))
        .fetch_all(&self.pool)
        .await?;
        Ok(if keys.len() == 1 {
            keys.into_iter().next()
        } else {
            None
        })
    }

    /// Projects other than `except` whose key or `rp.aliases` take one of `aliases`.
    pub async fn rp_alias_conflicts(
        &self,
        except: Uuid,
        aliases: &[String],
    ) -> anyhow::Result<Vec<String>> {
        if aliases.is_empty() {
            return Ok(Vec::new());
        }
        let lower: Vec<String> = aliases
            .iter()
            .map(|a| no_nul(&a.to_ascii_lowercase()))
            .collect();
        Ok(sqlx::query_scalar(
            "select key from projects where deleted_at is null and id <> $1 and (lower(key) = any($2) or \
               (settings ? 'rp' and exists (select 1 from jsonb_array_elements_text(case \
                 when jsonb_typeof(settings->'rp'->'aliases') = 'array' then settings->'rp'->'aliases' else '[]'::jsonb end) a \
                 where lower(a) = any($2)))) order by key limit 20",
        )
        .bind(except)
        .bind(&lower)
        .fetch_all(&self.pool)
        .await?)
    }
}
