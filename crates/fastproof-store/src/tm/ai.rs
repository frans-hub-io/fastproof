//! Writes of AI agents (MCP, T3): draft cases and suggestions. An agent never writes a case
//! directly: it creates drafts (`origin = ai`, `aiUnreviewed`) and proposes changes that a
//! person accepts or rejects. These rules hold on every path (REST and MCP): the principal
//! that created an unreviewed AI draft can neither review nor activate it, and the principal
//! that made a suggestion cannot accept it ([`check_ai_review`], 403 `ai_self_review`).
//! A case has at most [`MAX_PENDING_SUGGESTIONS`] pending suggestions.

use uuid::Uuid;

use super::cases::{
    CaseCtx, NewCaseMeta, apply_patch, check_count_in_tx, create_case_in, next_number,
    prepare_write,
};
use super::fields::field_defs;
use super::versions::snapshot_of;
use super::{
    CaseOrigin, CaseProposal, CaseRow, CaseStatus, CaseWrite, TmError, TmResult, VersionSource,
};
use crate::{Store, no_nul};

/// Pending suggestions of one case, at most (409 `limit_reached`, `maxPendingSuggestions`).
pub const MAX_PENDING_SUGGESTIONS: i64 = 20;

/// Unreviewed AI draft cases one principal has in a project, at most (409 `limit_reached`,
/// `maxUnreviewedAiDrafts`); people review or archive some to make room.
pub const MAX_UNREVIEWED_AI_DRAFTS: i64 = 200;

/// Disabled triage rules one principal created in a project, at most, before an agent may draft
/// another (409 `limit_reached`, `maxRuleDrafts`).
pub const MAX_RULE_DRAFTS: i64 = 20;

/// Code of the 403 answered to a principal reviewing its own AI work.
pub const AI_SELF_REVIEW: &str = "ai_self_review";

/// Whether `by` created this unreviewed AI draft.
pub fn ai_self_review(cur: &CaseRow, by: &str) -> bool {
    cur.origin == CaseOrigin::Ai && cur.ai_unreviewed && cur.created_by.as_deref() == Some(by)
}

/// Refuses `by` reviewing or activating an unreviewed AI draft it created.
pub fn check_ai_review(cur: &CaseRow, by: &str) -> TmResult<()> {
    if ai_self_review(cur, by) {
        return Err(TmError::Forbidden {
            code: AI_SELF_REVIEW,
            message: "an AI draft is reviewed or activated by someone other than the principal that created it"
                .into(),
        });
    }
    Ok(())
}

/// Refuses `by` accepting a suggestion it made.
pub fn check_suggestion_accept(author: Option<&str>, by: &str) -> TmResult<()> {
    if author == Some(by) {
        return Err(TmError::Forbidden {
            code: AI_SELF_REVIEW,
            message: "a suggestion is accepted by someone other than the principal that made it"
                .into(),
        });
    }
    Ok(())
}

impl Store {
    /// Creates a case for an agent: always `draft`, origin and version source `ai`,
    /// `aiUnreviewed`.
    pub async fn create_ai_case(&self, ctx: &CaseCtx<'_>, w: &CaseWrite) -> TmResult<CaseRow> {
        let mut w = w.clone();
        w.status = Some(CaseStatus::Draft);
        let mut conn = self.pool.acquire().await?;
        let defs = field_defs(&mut conn, ctx.project_id, true).await?;
        let snap = prepare_write(&mut conn, ctx, &w, None, &defs).await?;
        let n = next_number(&mut conn, ctx.project_id).await?;
        let mut tx = sqlx::Connection::begin(&mut *conn).await?;
        sqlx::query("select pg_advisory_xact_lock(hashtextextended($1 || ':' || $2, 7901))")
            .bind(ctx.project_id.to_string())
            .bind(no_nul(ctx.by))
            .execute(&mut *tx)
            .await?;
        let drafts: i64 = sqlx::query_scalar(
            "select count(*) from cases where project_id = $1 and origin = 'ai' and ai_unreviewed \
             and archived_at is null and created_by = $2",
        )
        .bind(ctx.project_id)
        .bind(no_nul(ctx.by))
        .fetch_one(&mut *tx)
        .await?;
        if drafts >= MAX_UNREVIEWED_AI_DRAFTS {
            return Err(TmError::limit(
                "maxUnreviewedAiDrafts",
                MAX_UNREVIEWED_AI_DRAFTS as u64,
            ));
        }
        check_count_in_tx(&mut tx, ctx.project_id, n, ctx.limits).await?;
        let meta = NewCaseMeta {
            origin: CaseOrigin::Ai,
            source: VersionSource::Ai,
            launch_id: None,
            ai_unreviewed: true,
            automated: false,
            code_hash: None,
            number: Some((n, true)),
        };
        let row = create_case_in(&mut tx, ctx, snap, &meta).await?;
        tx.commit().await?;
        Ok(row)
    }

    /// Stores a pending suggestion against the case's current revision after checking that
    /// the proposed content would be a valid case. Returns its id.
    pub async fn create_case_suggestion(
        &self,
        ctx: &CaseCtx<'_>,
        cur: &CaseRow,
        proposal: &CaseProposal,
        rationale: &str,
        agent: &str,
    ) -> TmResult<Uuid> {
        let mut conn = self.pool.begin().await?;
        sqlx::query("select 1 from cases where id = $1 and project_id = $2 for update")
            .bind(cur.id)
            .bind(ctx.project_id)
            .execute(&mut *conn)
            .await?;
        let pending: i64 = sqlx::query_scalar(
            "select count(*) from case_suggestions where case_id = $1 and status = 'pending'",
        )
        .bind(cur.id)
        .fetch_one(&mut *conn)
        .await?;
        if pending >= MAX_PENDING_SUGGESTIONS {
            return Err(TmError::limit(
                "maxPendingSuggestions",
                MAX_PENDING_SUGGESTIONS as u64,
            ));
        }
        let mut snap = snapshot_of(cur);
        apply_patch(&mut snap, &proposal.patch);
        if let Some(n) = &proposal.name {
            snap.name = n.clone();
        }
        if let Some(b) = &proposal.body {
            snap.body = b.clone();
        }
        let w = CaseWrite {
            name: snap.name,
            folder_id: snap.folder_id,
            status: Some(snap.status),
            body: Some(snap.body),
            fields: Some(snap.fields),
            tags: Some(snap.tags),
        };
        let defs = field_defs(&mut conn, ctx.project_id, true).await?;
        prepare_write(&mut conn, ctx, &w, Some(cur), &defs).await?;
        let id = Uuid::now_v7();
        sqlx::query(
            "insert into case_suggestions (id, project_id, case_id, base_revision, proposal, rationale, agent, \
             created_by) values ($1, $2, $3, $4, $5, $6, $7, $8)",
        )
        .bind(id)
        .bind(ctx.project_id)
        .bind(cur.id)
        .bind(cur.revision)
        .bind(crate::json_no_nul(serde_json::to_value(proposal)?))
        .bind(no_nul(rationale))
        .bind(no_nul(agent))
        .bind(no_nul(ctx.by))
        .execute(&mut *conn)
        .await?;
        conn.commit().await?;
        Ok(id)
    }

    /// Disabled triage rules `by` created in the project (the rule drafts an agent may still
    /// add are [`MAX_RULE_DRAFTS`] minus this).
    pub async fn disabled_rules_by(&self, project_id: Uuid, by: &str) -> anyhow::Result<i64> {
        Ok(sqlx::query_scalar(
            "select count(*) from rules where project_id = $1 and not enabled and created_by = $2",
        )
        .bind(project_id)
        .bind(no_nul(by))
        .fetch_one(&self.pool)
        .await?)
    }
}
