//! Deletes of large row sets in bounded statements.
//!
//! Deleting a row that other tables reference queues a foreign-key trigger event per
//! referencing constraint, and an `on delete cascade` runs further deletes (with events of
//! their own) inside the same statement. One `delete` of a project or of a launch with a big
//! manual run therefore holds everything under it in the server's memory until the statement
//! ends. Here a referenced row goes only after its children, at most `n` per statement, so the
//! cascades find nothing.
//!
//! [`Mode::Steps`] runs every statement in its own transaction and touches at most `n` rows of
//! any table per statement (the purge of a deleted project: a crash keeps what was done and
//! the next run continues). [`Mode::Atomic`] runs inside the caller's transaction: rows of
//! referenced tables still go `n` at a time, while a leaf table (no foreign key references it)
//! loses the rows of a parent chunk in one statement, because such a delete queues no trigger
//! events and batching it inside one transaction would rescan the rows it already deleted.

use sqlx::{AssertSqlSafe, PgConnection};
use uuid::Uuid;

/// Default bound on the rows a purge statement deletes ([`crate::Store::with_purge_batch`]).
pub const PURGE_BATCH: i64 = 5_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Mode {
    Steps,
    Atomic,
}

/// How a statement picks single rows of a table.
#[derive(Clone, Copy)]
enum RowKey {
    Ctid,
    /// `results` and `attempts`: a `ctid` is not unique across partitions, so rows are picked
    /// by `id` (a UUIDv7) together with the condition, which keeps the launch index usable.
    Partitioned,
}

/// A table that no foreign key references.
#[derive(Clone, Copy)]
struct Leaf {
    table: &'static str,
    key: RowKey,
}

const fn leaf(table: &'static str) -> Leaf {
    Leaf {
        table,
        key: RowKey::Ctid,
    }
}

const RESULTS: Leaf = Leaf {
    table: "results",
    key: RowKey::Partitioned,
};
const ATTEMPTS: Leaf = Leaf {
    table: "attempts",
    key: RowKey::Partitioned,
};

/// Leaves under a launch, by `launch_id`, except results and attempts.
const LAUNCH_LEAVES: [Leaf; 6] = [
    leaf("attempt_log_chunks"),
    leaf("launch_containers"),
    leaf("attachment_links"),
    leaf("attachment_refs"),
    leaf("launch_gates"),
    leaf("case_sync_queue"),
];

/// Leaves under a case, by `case_id` (run items are not leaves and go separately).
const CASE_LEAVES: [Leaf; 5] = [
    leaf("case_versions"),
    leaf("case_keys"),
    leaf("case_attachments"),
    leaf("case_suggestions"),
    leaf("test_plan_items"),
];

/// Tables of a project with nothing left under them once launches, plans, test cases and
/// cases are gone, by `project_id`. Attachments come after cases (case attachments restrict
/// their deletion) and after their media rows; the last ones are normally empty by then.
const PROJECT_LEAVES: [&str; 14] = [
    "tracker_issue_links",
    "tracker_issues",
    "attachment_media",
    "attachments",
    "case_field_defs",
    "case_keys",
    "tm_jobs",
    "defect_types",
    "rules",
    "analysis_tasks",
    "project_secrets",
    "integration_deliveries",
    "launch_gates",
    "attempt_log_chunks",
];

/// Statistics of series, deleted by `project_id` before test cases and series.
const SERIES_LEAVES: [&str; 6] = [
    "series_daily",
    "series_stats",
    "series_mutes",
    "failure_fingerprints",
    "launch_daily",
    "series",
];

pub(crate) struct Purge<'c> {
    conn: &'c mut PgConnection,
    n: i64,
    mode: Mode,
    /// Statements left before the purge stops (tests simulate a crash with it).
    budget: Option<u64>,
}

impl<'c> Purge<'c> {
    pub(crate) fn new(conn: &'c mut PgConnection, n: i64, mode: Mode) -> Self {
        Self {
            conn,
            n: n.max(1),
            mode,
            budget: None,
        }
    }

    pub(crate) fn with_budget(mut self, statements: Option<u64>) -> Self {
        self.budget = statements;
        self
    }

    fn tick(&mut self) -> anyhow::Result<()> {
        if let Some(left) = &mut self.budget {
            if *left == 0 {
                anyhow::bail!("purge interrupted: statement budget spent");
            }
            *left -= 1;
        }
        Ok(())
    }

    /// Runs `sql` with `$1` = `ids`.
    async fn exec(&mut self, sql: String, ids: &[Uuid]) -> anyhow::Result<u64> {
        self.tick()?;
        Ok(sqlx::query(AssertSqlSafe(sql))
            .bind(ids)
            .execute(&mut *self.conn)
            .await?
            .rows_affected())
    }

    /// Runs `sql` with `$1` = `ids` and `$2` = the batch size.
    async fn exec_n(&mut self, sql: &str, ids: &[Uuid]) -> anyhow::Result<u64> {
        self.tick()?;
        Ok(sqlx::query(AssertSqlSafe(sql.to_string()))
            .bind(ids)
            .bind(self.n)
            .execute(&mut *self.conn)
            .await?
            .rows_affected())
    }

    /// Ids selected by `sql` with `$1` = `ids` and `$2` = the batch size.
    async fn ids(&mut self, sql: &str, ids: &[Uuid]) -> anyhow::Result<Vec<Uuid>> {
        self.tick()?;
        Ok(sqlx::query_scalar(AssertSqlSafe(sql.to_string()))
            .bind(ids)
            .bind(self.n)
            .fetch_all(&mut *self.conn)
            .await?)
    }

    /// Deletes the rows of `table` matching `cond` (on `$1`), at most `n` per statement, in
    /// the order of `order` (may be empty).
    async fn drain(
        &mut self,
        table: &str,
        key: RowKey,
        cond: &str,
        order: &str,
        ids: &[Uuid],
    ) -> anyhow::Result<u64> {
        let sql = match key {
            RowKey::Ctid => format!(
                "delete from {table} where ctid = any(array(select ctid from {table} \
                 where {cond} {order} limit $2))"
            ),
            RowKey::Partitioned => format!(
                "delete from {table} where {cond} and id = any(array(select id from {table} \
                 where {cond} {order} limit $2))"
            ),
        };
        let mut total = 0;
        loop {
            let r = self.exec_n(&sql, ids).await?;
            total += r;
            if r < self.n as u64 {
                return Ok(total);
            }
        }
    }

    /// Deletes the rows of a leaf table whose `fk` is one of `parents`. In steps, parents are
    /// grouped by their row counts so that a statement deletes at most `n` rows; a parent
    /// with more is drained on its own.
    async fn leaves(&mut self, leaf: Leaf, fk: &str, parents: &[Uuid]) -> anyhow::Result<()> {
        if parents.is_empty() {
            return Ok(());
        }
        let table = leaf.table;
        let delete = format!("delete from {table} where {fk} = any($1)");
        if self.mode == Mode::Atomic {
            self.exec(delete, parents).await?;
            return Ok(());
        }
        self.tick()?;
        let counts: Vec<(Uuid, i64)> = sqlx::query_as(AssertSqlSafe(format!(
            "select {fk}, count(*) from {table} where {fk} = any($1) group by 1"
        )))
        .bind(parents)
        .fetch_all(&mut *self.conn)
        .await?;
        let mut group = Vec::new();
        let mut rows = 0;
        for (p, c) in counts {
            if c > self.n {
                self.drain(table, leaf.key, &format!("{fk} = any($1)"), "", &[p])
                    .await?;
                continue;
            }
            if rows + c > self.n {
                self.exec(delete.clone(), &group).await?;
                group.clear();
                rows = 0;
            }
            group.push(p);
            rows += c;
        }
        if !group.is_empty() {
            self.exec(delete, &group).await?;
        }
        Ok(())
    }

    /// Deletes run items whose `col` (`launch_id` or `case_id`) is one of `ids`, with their
    /// step results and executions.
    async fn run_items(&mut self, col: &str, ids: &[Uuid]) -> anyhow::Result<()> {
        let select = format!("select id from run_items where {col} = any($1) limit $2");
        loop {
            let items = self.ids(&select, ids).await?;
            if items.is_empty() {
                return Ok(());
            }
            self.leaves(leaf("run_step_results"), "run_item_id", &items)
                .await?;
            self.leaves(leaf("run_item_executions"), "run_item_id", &items)
                .await?;
            self.exec("delete from run_items where id = any($1)".into(), &items)
                .await?;
            if (items.len() as i64) < self.n {
                return Ok(());
            }
        }
    }

    /// Deletes the uploads of these launches with their ingest jobs.
    async fn uploads(&mut self, launches: &[Uuid]) -> anyhow::Result<()> {
        loop {
            let ups = self
                .ids(
                    "select id from uploads where launch_id = any($1) limit $2",
                    launches,
                )
                .await?;
            if ups.is_empty() {
                return Ok(());
            }
            self.leaves(leaf("ingest_jobs"), "upload_id", &ups).await?;
            self.exec("delete from uploads where id = any($1)".into(), &ups)
                .await?;
            if (ups.len() as i64) < self.n {
                return Ok(());
            }
        }
    }

    /// Deletes every row under these launches (at most `n` of them), with their results and
    /// attempts when `raw`; the launch rows stay.
    pub(crate) async fn launch_contents(
        &mut self,
        launches: &[Uuid],
        raw: bool,
    ) -> anyhow::Result<()> {
        debug_assert!(launches.len() as i64 <= self.n);
        self.uploads(launches).await?;
        if raw {
            self.leaves(ATTEMPTS, "launch_id", launches).await?;
            self.leaves(RESULTS, "launch_id", launches).await?;
        }
        for l in LAUNCH_LEAVES {
            self.leaves(l, "launch_id", launches).await?;
        }
        self.run_items("launch_id", launches).await
    }

    /// Deletes these launches (at most `n`) and everything under them.
    pub(crate) async fn launches(&mut self, launches: &[Uuid]) -> anyhow::Result<()> {
        self.launch_contents(launches, true).await?;
        // Not one of the launch leaves: the log budget stays while only the contents go.
        self.leaves(leaf("launch_log_bytes"), "launch_id", launches)
            .await?;
        self.exec("delete from launches where id = any($1)".into(), launches)
            .await?;
        Ok(())
    }

    /// Deletes every row of a project except the project row itself.
    pub(crate) async fn project(&mut self, project_id: Uuid) -> anyhow::Result<()> {
        let p = [project_id];
        let by_project = "project_id = any($1)";
        loop {
            let ls = self
                .ids(
                    "select id from launches where project_id = any($1) limit $2",
                    &p,
                )
                .await?;
            if ls.is_empty() {
                break;
            }
            self.launches(&ls).await?;
        }
        loop {
            let plans = self
                .ids(
                    "select id from test_plans where project_id = any($1) limit $2",
                    &p,
                )
                .await?;
            if plans.is_empty() {
                break;
            }
            self.leaves(leaf("test_plan_items"), "plan_id", &plans)
                .await?;
            self.exec("delete from test_plans where id = any($1)".into(), &plans)
                .await?;
        }
        for t in SERIES_LEAVES {
            self.drain(t, RowKey::Ctid, by_project, "", &p).await?;
        }
        self.drain("test_cases", RowKey::Ctid, by_project, "", &p)
            .await?;
        loop {
            let cases = self
                .ids(
                    "select id from cases where project_id = any($1) limit $2",
                    &p,
                )
                .await?;
            if cases.is_empty() {
                break;
            }
            for l in CASE_LEAVES {
                self.leaves(l, "case_id", &cases).await?;
            }
            self.run_items("case_id", &cases).await?;
            self.exec("delete from cases where id = any($1)".into(), &cases)
                .await?;
        }
        self.drain(
            "case_folders",
            RowKey::Ctid,
            by_project,
            "order by depth desc",
            &p,
        )
        .await?;
        for t in PROJECT_LEAVES {
            self.drain(t, RowKey::Ctid, by_project, "", &p).await?;
        }
        Ok(())
    }

    /// Deletes a folder with its subfolders: cases in them lose the folder first, then the
    /// folders go deepest first, `n` per statement.
    pub(crate) async fn folder_tree(
        &mut self,
        project_id: Uuid,
        folder_id: Uuid,
    ) -> anyhow::Result<()> {
        self.tick()?;
        let folders: Vec<Uuid> = sqlx::query_scalar(
            "select id from case_folders where project_id = $1 and path_ids @> array[$2]::uuid[] \
             order by depth desc, id",
        )
        .bind(project_id)
        .bind(folder_id)
        .fetch_all(&mut *self.conn)
        .await?;
        self.tick()?;
        let cases: Vec<Uuid> = sqlx::query_scalar(
            "select id from cases where project_id = $1 and folder_id = any($2)",
        )
        .bind(project_id)
        .bind(&folders)
        .fetch_all(&mut *self.conn)
        .await?;
        for chunk in cases.chunks(self.n as usize) {
            self.tick()?;
            sqlx::query(
                "update cases set folder_id = null where id = any($1) and folder_id = any($2)",
            )
            .bind(chunk)
            .bind(&folders)
            .execute(&mut *self.conn)
            .await?;
        }
        for chunk in folders.chunks(self.n as usize) {
            self.exec("delete from case_folders where id = any($1)".into(), chunk)
                .await?;
        }
        Ok(())
    }
}
