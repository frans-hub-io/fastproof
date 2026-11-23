//! Triage: defect type, comment, issue and mute on listed results, or on every result of a
//! launch with one fingerprint.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use fastproof_model::Status;
use serde::{Deserialize, Serialize};
use sqlx::PgConnection;
use utoipa::ToSchema;
use uuid::Uuid;

use super::hook::{TypeRef, type_refs, upsert_mutes};
use super::rules::IssueRef;
use crate::{Defect, Store, no_nul};

/// Results listed in one triage request, at most.
pub const MAX_TRIAGE_IDS: usize = 1000;
/// Launches notified after one triage request, at most.
const MAX_NOTIFIED: usize = 100;

/// Mute part of a triage request.
#[derive(Clone, Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MuteRequest {
    /// End of the mute; without it the mute lasts until removed.
    pub until: Option<DateTime<Utc>>,
    pub reason: String,
}

/// Issue of a triage: a URL, or a key that a project issue tracker turns into one.
#[derive(Clone, Debug, Default, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TriageIssue {
    pub url: Option<String>,
    pub key: Option<String>,
}

/// Body of `POST .../triage`.
#[derive(Clone, Debug, Default, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TriageRequest {
    pub result_ids: Option<Vec<Uuid>>,
    pub launch_id: Option<Uuid>,
    /// With `launchId`: every result of the launch with this fingerprint.
    pub fingerprint: Option<String>,
    pub defect_type_id: Option<Uuid>,
    /// Empty string removes the comment.
    pub comment: Option<String>,
    pub issue: Option<TriageIssue>,
    pub mute: Option<MuteRequest>,
    pub unmute: Option<bool>,
    /// `user` (default) or `ai`.
    pub source: Option<String>,
}

/// Outcome of a triage request.
#[derive(Clone, Debug, Serialize, ToSchema)]
pub struct TriageOutcome {
    pub updated: u64,
}

impl TriageRequest {
    /// Checks shape and sizes.
    pub fn validate(&self, now: DateTime<Utc>) -> Result<(), String> {
        match (&self.result_ids, self.launch_id, &self.fingerprint) {
            (Some(ids), _, _) => {
                if ids.is_empty() || ids.len() > MAX_TRIAGE_IDS {
                    return Err(format!("resultIds must list 1 to {MAX_TRIAGE_IDS} results"));
                }
            }
            (None, Some(_), Some(_)) => {}
            _ => return Err("give resultIds, or launchId and fingerprint".into()),
        }
        if let Some(f) = &self.fingerprint
            && !super::rules::valid_fingerprint(f)
        {
            return Err("fingerprint must be 16 hex digits".into());
        }
        let changes_defect =
            self.defect_type_id.is_some() || self.comment.is_some() || self.issue.is_some();
        if !changes_defect && self.mute.is_none() && self.unmute != Some(true) {
            return Err(
                "nothing to change: give defectTypeId, comment, issue, mute or unmute".into(),
            );
        }
        if self.mute.is_some() && self.unmute == Some(true) {
            return Err("mute and unmute exclude each other".into());
        }
        if let Some(c) = &self.comment
            && c.len() > 4000
        {
            return Err("comment must have at most 4000 bytes".into());
        }
        if let Some(i) = &self.issue {
            let Some(url) = &i.url else {
                return Err("issue.url is required (no issue tracker matches the key)".into());
            };
            super::rules::validate_issue(&IssueRef {
                url: url.clone(),
                key: i.key.clone(),
            })?;
        }
        if let Some(m) = &self.mute {
            if m.reason.trim().is_empty() || m.reason.len() > 500 {
                return Err("mute.reason must have 1 to 500 bytes".into());
            }
            if let Some(u) = m.until
                && (u <= now || u > now + chrono::Duration::days(3660))
            {
                return Err("mute.until must be in the future (at most 10 years)".into());
            }
        }
        if let Some(s) = &self.source
            && !matches!(s.as_str(), "user" | "ai")
        {
            return Err("source must be user or ai".into());
        }
        Ok(())
    }
}

/// Why a triage request was refused after validation.
#[derive(Debug)]
pub enum TriageRefusal {
    UnknownDefectType,
    UnknownLaunch,
}

type Target = (
    Uuid,
    DateTime<Utc>,
    Uuid,
    Uuid,
    String,
    Option<serde_json::Value>,
    bool,
);

/// Locks launch rows (`for no key update`, in id order) before their results change. Ingest
/// locks the launch row before its automated results, so a change of automated results that
/// takes the launch rows first cannot deadlock with it. Manual writes lock their results before
/// the launch row, so changes that touch manual results keep the opposite order and are retried
/// when PostgreSQL breaks a deadlock.
pub async fn lock_launches(conn: &mut PgConnection, launches: &[Uuid]) -> anyhow::Result<()> {
    let mut ids = launches.to_vec();
    ids.sort();
    ids.dedup();
    sqlx::query("select 1 from launches where id = any($1) order by id for no key update")
        .bind(&ids)
        .execute(&mut *conn)
        .await?;
    Ok(())
}

/// Recounts muted results of launches (`count_muted`), after locking the launch rows. Every
/// change of the `muted` flag of results goes through this (or adds its exact change of the
/// counter in the same transaction), so the counter always equals a recount.
pub async fn recount_muted(conn: &mut PgConnection, launches: &[Uuid]) -> anyhow::Result<()> {
    if launches.is_empty() {
        return Ok(());
    }
    let mut ids = launches.to_vec();
    ids.sort();
    ids.dedup();
    // Locked first, so the count below sees every committed change: manual writes add their
    // change of `count_muted` to the launch row and hold it until their commit, and a count
    // taken before waiting for them would overwrite it.
    lock_launches(conn, &ids).await?;
    sqlx::query(
        "update launches l set count_muted = (select count(*) from results r \
           where r.launch_id = l.id and r.launch_created_at = l.created_at and r.muted), updated_at = now() \
         where l.id = any($1)",
    )
    .bind(&ids)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

fn base_type<'a>(
    status: &str,
    existing: Option<&Defect>,
    by_id: &'a std::collections::HashMap<Uuid, TypeRef>,
    by_group: &'a std::collections::HashMap<String, TypeRef>,
) -> Option<&'a TypeRef> {
    existing.and_then(|d| by_id.get(&d.type_id)).or_else(|| {
        by_group.get(if Status::parse_lenient(status).is_failure() {
            super::defects::TO_INVESTIGATE
        } else {
            "noDefect"
        })
    })
}

impl Store {
    /// Applies a validated triage request. The transaction runs again (at most
    /// [`crate::ingest::TX_ATTEMPTS`] times) when PostgreSQL aborts it to break a deadlock with
    /// a concurrent writer of the same results.
    pub async fn triage(
        &self,
        project_id: Uuid,
        project_key: &str,
        req: &TriageRequest,
        by: &str,
    ) -> anyhow::Result<Result<TriageOutcome, TriageRefusal>> {
        let mut attempt = 1;
        loop {
            match self.triage_once(project_id, project_key, req, by).await {
                Err(e)
                    if attempt < crate::ingest::TX_ATTEMPTS
                        && crate::ingest::retryable(&e).is_some() =>
                {
                    crate::ingest::retry_pause("triage", &e, attempt).await;
                    attempt += 1;
                }
                r => return r,
            }
        }
    }

    async fn triage_once(
        &self,
        project_id: Uuid,
        project_key: &str,
        req: &TriageRequest,
        by: &str,
    ) -> anyhow::Result<Result<TriageOutcome, TriageRefusal>> {
        let now = Utc::now();
        let mut tx = self.pool.begin().await?;
        let targets: Vec<Target> = if let Some(ids) = &req.result_ids {
            sqlx::query_as(
                "select id, launch_created_at, launch_id, series_id, status, defect, \
                   run_item_id is not null from results \
                 where project_id = $1 and id = any($2) and ($3::uuid is null or launch_id = $3) \
                   and ($4::text is null or fingerprint = $4) order by id limit $5",
            )
            .bind(project_id)
            .bind(ids)
            .bind(req.launch_id)
            .bind(req.fingerprint.as_deref())
            .bind(MAX_TRIAGE_IDS as i64)
            .fetch_all(&mut *tx)
            .await?
        } else {
            let launch = req.launch_id.unwrap_or_default();
            let created: Option<DateTime<Utc>> = sqlx::query_scalar(
                "select created_at from launches where project_id = $1 and id = $2",
            )
            .bind(project_id)
            .bind(launch)
            .fetch_optional(&mut *tx)
            .await?;
            let Some(created) = created else {
                return Ok(Err(TriageRefusal::UnknownLaunch));
            };
            sqlx::query_as(
                "select id, launch_created_at, launch_id, series_id, status, defect, \
                   run_item_id is not null from results \
                 where launch_id = $1 and launch_created_at = $2 and fingerprint = $3 order by id",
            )
            .bind(launch)
            .bind(created)
            .bind(req.fingerprint.as_deref())
            .fetch_all(&mut *tx)
            .await?
        };
        if targets.is_empty() {
            return Ok(Ok(TriageOutcome { updated: 0 }));
        }
        let (by_id, by_group) = type_refs(&mut tx, project_id).await?;
        let chosen = match req.defect_type_id {
            Some(t) => match by_id.get(&t) {
                Some(t) => Some(t),
                None => return Ok(Err(TriageRefusal::UnknownDefectType)),
            },
            None => None,
        };
        let source = req.source.clone().unwrap_or_else(|| "user".into());
        let launches: Vec<Uuid> = targets
            .iter()
            .map(|t| t.2)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let ids: Vec<Uuid> = targets.iter().map(|t| t.0).collect();
        let created: Vec<DateTime<Utc>> = targets.iter().map(|t| t.1).collect();
        if !targets.iter().any(|t| t.6) {
            // Automated results only: the launch rows first, in the order ingest takes them.
            lock_launches(&mut tx, &launches).await?;
        }

        if req.defect_type_id.is_some() || req.comment.is_some() || req.issue.is_some() {
            let mut defects = Vec::with_capacity(targets.len());
            for (_, _, _, _, status, defect, _) in &targets {
                let existing: Option<Defect> =
                    defect.clone().and_then(|d| serde_json::from_value(d).ok());
                let t = chosen
                    .or_else(|| base_type(status, existing.as_ref(), &by_id, &by_group))
                    .ok_or_else(|| anyhow::anyhow!("built-in defect types are missing"))?;
                let mut d = Defect {
                    type_id: t.id,
                    group: t.group.clone(),
                    name: t.name.clone(),
                    comment: existing.as_ref().and_then(|d| d.comment.clone()),
                    issue_url: existing.as_ref().and_then(|d| d.issue_url.clone()),
                    issue_key: existing.as_ref().and_then(|d| d.issue_key.clone()),
                    source: source.clone(),
                    rule_id: None,
                    at: now,
                    by: no_nul(by),
                    accepted: existing.as_ref().is_some_and(|d| d.accepted),
                };
                if let Some(c) = &req.comment {
                    d.comment = (!c.is_empty()).then(|| no_nul(c));
                }
                if let Some(i) = &req.issue {
                    d.issue_url = i.url.clone();
                    d.issue_key = i.key.clone();
                }
                defects.push(crate::json_no_nul(serde_json::to_value(d)?));
            }
            sqlx::query(
                "update results r set defect = t.d, updated_at = now() \
                 from unnest($1::uuid[], $2::timestamptz[], $3::jsonb[]) as t(i, c, d) \
                 where r.id = t.i and r.launch_created_at = t.c",
            )
            .bind(&ids)
            .bind(&created)
            .bind(&defects)
            .execute(&mut *tx)
            .await?;
        }
        let series: BTreeSet<Uuid> = targets.iter().map(|t| t.3).collect();
        if let Some(m) = &req.mute {
            let mutes: BTreeMap<Uuid, (Option<DateTime<Utc>>, String)> = series
                .iter()
                .map(|s| (*s, (m.until, m.reason.trim().to_string())))
                .collect();
            upsert_mutes(&mut tx, project_id, &mutes, by, true).await?;
            sqlx::query(
                "update results r set muted = true, mute_until = $3, updated_at = now() \
                 from unnest($1::uuid[], $2::timestamptz[]) as t(i, c) \
                 where r.id = t.i and r.launch_created_at = t.c",
            )
            .bind(&ids)
            .bind(&created)
            .bind(m.until)
            .execute(&mut *tx)
            .await?;
        }
        if req.unmute == Some(true) {
            let s: Vec<Uuid> = series.iter().copied().collect();
            sqlx::query("delete from series_mutes where project_id = $1 and series_id = any($2)")
                .bind(project_id)
                .bind(&s)
                .execute(&mut *tx)
                .await?;
            sqlx::query(
                "update results r set muted = false, mute_until = null, updated_at = now() \
                 from unnest($1::uuid[], $2::timestamptz[]) as t(i, c) \
                 where r.id = t.i and r.launch_created_at = t.c",
            )
            .bind(&ids)
            .bind(&created)
            .execute(&mut *tx)
            .await?;
        }
        if req.mute.is_some() || req.unmute == Some(true) {
            recount_muted(&mut tx, &launches).await?;
        }
        crate::explorer::touch(&mut tx, &launches).await?;
        crate::events::emit(
            &mut tx,
            "results.triaged",
            Some(project_key),
            serde_json::json!({"launchIds": launches, "count": ids.len(), "by": by,
                "defectTypeId": req.defect_type_id, "mute": req.mute.is_some(), "unmute": req.unmute == Some(true)}),
        )
        .await?;
        for l in launches.iter().take(MAX_NOTIFIED) {
            crate::events::notify_launch(&mut tx, project_key, *l).await?;
        }
        tx.commit().await?;
        Ok(Ok(TriageOutcome {
            updated: ids.len() as u64,
        }))
    }
}
