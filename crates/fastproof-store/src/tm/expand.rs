//! Pins of shared steps in run items and their expansion into executed steps (§7.1).
//!
//! `run_items.shared_pins` holds `{"<sharedStepId>": version}`: the runnable version of every
//! shared step the pinned case version references, at the time the item was created, refreshed
//! or rerun. An executed snapshot replaces each `shared` step's `steps` by the pinned version's
//! steps, so a run keeps its steps when the shared step changes later, and `step_path` numbers
//! the expanded tree.

use std::collections::{BTreeSet, HashMap};

use sqlx::{PgConnection, Row};
use uuid::Uuid;

use super::shared_steps::{load, ref_ids, rows_by_id, runnable_of};
use super::{CaseReviewSettings, CaseSnapshot, CaseStep, SharedPin, TmResult};

/// `run_items.shared_pins`: `{"<sharedStepId>": version}`.
pub type SharedPins = std::collections::BTreeMap<Uuid, i32>;

/// Pins per case id for `(case_id, case_version)`: the runnable version of every shared step
/// the case version references. Head versions are read with one query over `cases.body`;
/// older versions from their snapshots with one more. Cases without references are left out.
pub async fn pins_for_items(
    conn: &mut PgConnection,
    project_id: Uuid,
    items: &[(Uuid, i32)],
    review: &CaseReviewSettings,
) -> TmResult<HashMap<Uuid, SharedPins>> {
    if items.is_empty() {
        return Ok(HashMap::new());
    }
    let (case_ids, versions): (Vec<Uuid>, Vec<i32>) = items.iter().copied().unzip();
    let heads: Vec<(Uuid, Vec<Uuid>)> = sqlx::query_as(
        "select c.id, array(select (st -> 'shared' ->> 'id')::uuid from jsonb_array_elements(c.body -> 'steps') st \
           where jsonb_typeof(st -> 'shared') = 'object') \
         from cases c join unnest($2::uuid[], $3::int[]) as t(id, v) on t.id = c.id and t.v = c.version \
         where c.project_id = $1",
    )
    .bind(project_id)
    .bind(&case_ids)
    .bind(&versions)
    .fetch_all(&mut *conn)
    .await?;
    let mut refs: HashMap<Uuid, Vec<Uuid>> = heads.into_iter().collect();
    let older: Vec<(Uuid, i32)> = items
        .iter()
        .filter(|(c, _)| !refs.contains_key(c))
        .copied()
        .collect();
    if !older.is_empty() {
        let (oc, ov): (Vec<Uuid>, Vec<i32>) = older.into_iter().unzip();
        let rows = sqlx::query(
            "select v.case_id, v.snapshot_z from case_versions v \
             join unnest($2::uuid[], $3::int[]) as t(id, v) on t.id = v.case_id and t.v = v.version \
             where v.project_id = $1",
        )
        .bind(project_id)
        .bind(&oc)
        .bind(&ov)
        .fetch_all(&mut *conn)
        .await?;
        for r in rows {
            let z: Vec<u8> = r.try_get("snapshot_z")?;
            let snap: CaseSnapshot = crate::codec::decode(&z)?;
            refs.insert(r.try_get("case_id")?, ref_ids(&snap.body.steps));
        }
    }
    let ids: Vec<Uuid> = refs
        .values()
        .flatten()
        .copied()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    let rows = rows_by_id(conn, project_id, &ids).await?;
    let runnable: HashMap<Uuid, i32> = rows
        .iter()
        .map(|(id, r)| (*id, runnable_of(r, review)))
        .collect();
    Ok(refs
        .into_iter()
        .filter_map(|(case, ids)| {
            let pins: SharedPins = ids
                .into_iter()
                .filter_map(|id| runnable.get(&id).map(|v| (id, *v)))
                .collect();
            (!pins.is_empty()).then_some((case, pins))
        })
        .collect())
}

/// Bytes of shared step content (steps JSON) one executed snapshot takes in all: each
/// reference is charged, also a repeated one. References beyond it stay unexpanded with a note,
/// so `maxSharedRefs` references to one large shared step cannot multiply a small case body.
pub const MAX_EXPANDED_SHARED_BYTES: usize = 1024 * 1024;

/// Replaces the `shared` steps of an executed snapshot by the pinned versions' steps and
/// stamps the version on the reference. A shared step that is no longer there (purged) or has
/// no pin keeps an empty reference with a note in `action`; so does one that would take the
/// snapshot over [`MAX_EXPANDED_SHARED_BYTES`].
#[allow(clippy::ptr_arg)]
pub async fn expand_steps(
    conn: &mut PgConnection,
    project_id: Uuid,
    steps: &mut Vec<CaseStep>,
    pins: &SharedPins,
) -> TmResult<()> {
    let wanted: Vec<(Uuid, i32)> = steps
        .iter()
        .filter_map(|s| s.shared.as_ref())
        .filter_map(|r| pins.get(&r.id).map(|v| (r.id, *v)))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    if wanted.is_empty() {
        return Ok(());
    }
    let (_, contents) = load(conn, project_id, &wanted).await?;
    let mut sizes: HashMap<(Uuid, i32), usize> = HashMap::new();
    for (k, (_, inner)) in &contents {
        sizes.insert(*k, serde_json::to_vec(inner)?.len());
    }
    let mut left = MAX_EXPANDED_SHARED_BYTES;
    for s in steps.iter_mut() {
        let Some(r) = s.shared.as_mut() else {
            continue;
        };
        let Some(v) = pins.get(&r.id) else {
            continue;
        };
        r.version = Some(*v);
        let key = (r.id, *v);
        match contents.get(&key) {
            Some((_, inner)) => {
                let size = sizes.get(&key).copied().unwrap_or(0);
                if size > left {
                    s.steps = Vec::new();
                    s.action = format!(
                        "Shared step SS-{} is not shown: the steps of this run item reached {} KiB",
                        r.number,
                        MAX_EXPANDED_SHARED_BYTES / 1024
                    );
                    left = 0;
                } else {
                    left -= size;
                    s.steps = inner.clone();
                }
            }
            None => {
                s.steps = Vec::new();
                s.action = format!("Shared step SS-{} is no longer available", r.number);
            }
        }
    }
    Ok(())
}

/// Pins a run item again to the runnable versions (refresh, rerun): the case version stays,
/// the pins follow the shared steps' runnable versions now.
pub async fn repin_item(
    conn: &mut PgConnection,
    project_id: Uuid,
    item_id: Uuid,
    review: &CaseReviewSettings,
) -> TmResult<()> {
    let r: Option<(Uuid, i32)> = sqlx::query_as(
        "select case_id, case_version from run_items where project_id = $1 and id = $2",
    )
    .bind(project_id)
    .bind(item_id)
    .fetch_optional(&mut *conn)
    .await?;
    let Some((case_id, version)) = r else {
        return Ok(());
    };
    let pins = pins_for_items(conn, project_id, &[(case_id, version)], review)
        .await?
        .remove(&case_id);
    sqlx::query("update run_items set shared_pins = $3 where project_id = $1 and id = $2")
        .bind(project_id)
        .bind(item_id)
        .bind(pins.map(serde_json::to_value).transpose()?)
        .execute(&mut *conn)
        .await?;
    Ok(())
}

/// The pins of a run item for the API: the pinned version and the runnable one now.
pub async fn pin_views(
    conn: &mut PgConnection,
    project_id: Uuid,
    pins: &SharedPins,
    review: &CaseReviewSettings,
) -> TmResult<Vec<SharedPin>> {
    if pins.is_empty() {
        return Ok(Vec::new());
    }
    let wanted: Vec<(Uuid, i32)> = pins.iter().map(|(i, v)| (*i, *v)).collect();
    let (rows, contents) = load(conn, project_id, &wanted).await?;
    let mut out: Vec<SharedPin> = wanted
        .iter()
        .filter_map(|(id, v)| {
            let r = rows.get(id)?;
            Some(SharedPin {
                id: *id,
                number: r.number,
                name: contents
                    .get(&(*id, *v))
                    .map(|c| c.0.clone())
                    .unwrap_or_else(|| r.name.clone()),
                version: *v,
                current: runnable_of(r, review),
            })
        })
        .collect();
    out.sort_by_key(|p| p.number);
    Ok(out)
}
