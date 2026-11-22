//! Writes of AI agents (MCP, T3): draft cases and suggestions. An agent never writes a case
//! directly: it creates drafts (`origin = ai`, `aiUnreviewed`) and proposes changes that a
//! person accepts or rejects.

use uuid::Uuid;

use super::cases::{CaseCtx, NewCaseMeta, apply_patch, create_case_in, prepare_write};
use super::fields::field_defs;
use super::versions::snapshot_of;
use super::{CaseOrigin, CaseProposal, CaseRow, CaseStatus, CaseWrite, TmResult, VersionSource};
use crate::Store;

impl Store {
    /// Creates a case for an agent: always `draft`, origin and version source `ai`,
    /// `aiUnreviewed`.
    pub async fn create_ai_case(&self, ctx: &CaseCtx<'_>, w: &CaseWrite) -> TmResult<CaseRow> {
        let mut w = w.clone();
        w.status = Some(CaseStatus::Draft);
        let mut tx = self.pool.begin().await?;
        let defs = field_defs(&mut tx, ctx.project_id, true).await?;
        let snap = prepare_write(&mut tx, ctx, &w, None, &defs).await?;
        let meta = NewCaseMeta {
            origin: CaseOrigin::Ai,
            source: VersionSource::Ai,
            launch_id: None,
            ai_unreviewed: true,
            automated: false,
            code_hash: None,
            number: None,
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
        let mut conn = self.pool.acquire().await?;
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
            body: snap.body,
            fields: Some(snap.fields),
            tags: Some(snap.tags),
        };
        let defs = field_defs(&mut conn, ctx.project_id, true).await?;
        prepare_write(&mut conn, ctx, &w, Some(cur), &defs).await?;
        let id = Uuid::now_v7();
        sqlx::query(
            "insert into case_suggestions (id, project_id, case_id, base_revision, proposal, rationale, agent) \
             values ($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(id)
        .bind(ctx.project_id)
        .bind(cur.id)
        .bind(cur.revision)
        .bind(crate::json_no_nul(serde_json::to_value(proposal)?))
        .bind(crate::no_nul(rationale))
        .bind(crate::no_nul(agent))
        .execute(&mut *conn)
        .await?;
        Ok(id)
    }
}
