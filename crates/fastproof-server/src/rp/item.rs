//! Test items of the ReportPortal endpoint: start (one insert), finish, read and update.

use std::collections::VecDeque;

use axum::body::Bytes;
use axum::extract::rejection::BytesRejection;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::Response;
use fastproof_rp::classify::{self, Parent, Role};
use fastproof_rp::ids::{self, IssueType};
use fastproof_rp::model::{FinishItem, StartItem, UpdateItem, clean_attributes, clean_parameters};
use fastproof_rp::status::{item_status, item_type, opens_running_result};
use fastproof_rp::time::{bounded_or_now, iso, now_ms};
use fastproof_rp::{MAX_DEPTH, MAX_DESCRIPTION_BYTES, MAX_REF_BYTES, clean_name, clean_text, cut};
use fastproof_store::Project;
use fastproof_store::rp::{ItemFinish, Placement, RpItem};
use franshub_auth::Auth;
use franshub_http::ApiError;
use serde_json::json;
use uuid::Uuid;

use super::{ItemRoute, RpError, RpResult, Version, json, parent_of, parse, version};
use crate::state::AppState;

/// The stored form of a placement.
pub fn stored(p: &classify::Placement) -> Placement {
    Placement {
        role: p.role.as_str().to_string(),
        depth: p.depth,
        owner_uuid: p.owner_uuid.clone(),
        log_key: p.log_key.clone(),
        log_step: p.log_step.clone(),
        seq_uuid: p.seq_uuid.clone(),
        error_uuid: p.error_uuid.clone(),
        suite_path: json!(p.suite_path),
    }
}

fn text(v: Option<&str>, max: usize) -> Option<String> {
    v.map(|t| cut(&clean_text(t), max))
        .filter(|t| !t.trim().is_empty())
}

fn started(ver: Version, row: &RpItem) -> Response {
    match ver {
        Version::V1 => {
            let params: Vec<(Option<String>, Option<String>)> = serde_json::from_value::<
                Vec<fastproof_rp::model::ItemParameter>,
            >(row.params.clone())
            .unwrap_or_default()
            .into_iter()
            .map(|p| (p.key, p.value))
            .collect();
            let path = row
                .code_ref
                .clone()
                .unwrap_or_else(|| format!("{}/{}", row.rp_launch, row.name));
            json(
                StatusCode::CREATED,
                json!({"id": row.uuid, "uniqueId": ids::unique_id(&path, &params)}),
            )
        }
        Version::V2 => json(StatusCode::CREATED, json!({"id": row.uuid})),
    }
}

/// `POST item`: a root item.
pub async fn start_root(
    State(s): State<AppState>,
    auth: Result<Auth, ApiError>,
    Path((v, project)): Path<(String, String)>,
    body: Result<Bytes, BytesRejection>,
) -> RpResult<Response> {
    start(s, auth, v, project, None, body).await
}

/// `POST item/{parentUuid}`: a child item.
pub async fn start_child(
    State(s): State<AppState>,
    auth: Result<Auth, ApiError>,
    Path((v, project, parent)): Path<(String, String, String)>,
    body: Result<Bytes, BytesRejection>,
) -> RpResult<Response> {
    start(s, auth, v, project, Some(parent), body).await
}

async fn start(
    s: AppState,
    auth: Result<Auth, ApiError>,
    v: String,
    project: String,
    parent: Option<String>,
    body: Result<Bytes, BytesRejection>,
) -> RpResult<Response> {
    let ver = version(&v)?;
    let (auth, p) = s
        .rp_project(auth, &project, &s.perms.launches_write)
        .await?;
    let rq: StartItem = parse(&s, body, false)?;
    let lu = rq
        .launch_uuid
        .as_deref()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .ok_or_else(|| RpError::bad_request("launchUuid is required"))?;
    let info = s.rp_launch_info(&p, lu).await?;
    s.rp_contributor(&auth, &p, &info).await?;
    let client_uuid = rq
        .uuid
        .as_deref()
        .map(str::trim)
        .filter(|u| !u.is_empty())
        .map(str::to_string);
    if let Some(u) = &client_uuid
        && !ids::valid_uuid(u)
    {
        return Err(RpError::bad_request(
            "uuid must have 1 to 64 characters of letters, digits, '.', '_' and '-'",
        ));
    }
    let uuid = client_uuid
        .clone()
        .unwrap_or_else(|| Uuid::new_v4().to_string());
    let parent_uuid = parent
        .or(rq.parent_uuid.clone())
        .map(|x| x.trim().to_string())
        .filter(|x| !x.is_empty());
    if let Some(pu) = &parent_uuid
        && (!ids::valid_uuid(pu) || *pu == uuid)
    {
        return Err(RpError::not_found("parent item"));
    }
    let name = match clean_name(rq.name.as_deref().unwrap_or_default()) {
        n if n.is_empty() => "unnamed".to_string(),
        n => n,
    };
    let kind = item_type(rq.item_type.as_deref());
    let parent_route = match &parent_uuid {
        Some(pu) => s.rp_route(p.id, pu).await?,
        None => None,
    };
    if let Some(r) = &parent_route
        && r.launch_id != info.launch.id
    {
        return Err(RpError::bad_request(
            "the parent item belongs to another launch",
        ));
    }
    let parent_view = match (&parent_uuid, &parent_route) {
        (_, Some(r)) => Some(r.parent.clone()),
        (Some(pu), None) => Some(Parent {
            uuid: pu.clone(),
            role: Role::Pending,
            item_type: String::new(),
            name: String::new(),
            depth: 0,
            owner_uuid: None,
            log_key: classify::container_key(pu),
            log_step: None,
            seq_uuid: Some(pu.clone()),
            error_uuid: None,
            suite_path: Vec::new(),
        }),
        _ => None,
    };
    let pl = classify::place(&uuid, &name, &kind, rq.has_stats, parent_view.as_ref());
    if pl.depth > MAX_DEPTH {
        return Err(RpError::bad_request(format!(
            "items are nested deeper than {MAX_DEPTH} levels"
        )));
    }
    s.rp_count_items(&p, &info, 1).await?;
    let start_ms = bounded_or_now(&rq.start_time, now_ms());
    let running = pl.role == Role::Result && opens_running_result(&kind);
    let st = stored(&pl);
    let item = RpItem {
        project_id: p.id,
        uuid: uuid.clone(),
        launch_id: info.launch.id,
        rp_launch: info.uuid.clone(),
        parent_uuid: parent_uuid.clone(),
        role: st.role.clone(),
        depth: st.depth,
        owner_uuid: st.owner_uuid,
        log_key: st.log_key,
        log_step: st.log_step,
        seq_uuid: st.seq_uuid,
        error_uuid: st.error_uuid,
        suite_path: st.suite_path,
        item_type: kind,
        has_stats: rq.has_stats,
        name,
        code_ref: text(rq.code_ref.as_deref(), MAX_REF_BYTES),
        test_case_id: text(rq.test_case_id.as_deref(), MAX_REF_BYTES),
        description: text(rq.description.as_deref(), MAX_DESCRIPTION_BYTES),
        params: json!(clean_parameters(&rq.parameters)),
        attrs: json!(clean_attributes(&rq.attributes)),
        start_ms,
        retry: rq.retry,
        retry_of: rq
            .retry_of
            .as_deref()
            .map(str::trim)
            .filter(|r| ids::valid_uuid(r))
            .map(str::to_string),
        running_sent: running,
        state: if pl.role == Role::Pending {
            "pending".into()
        } else {
            "started".into()
        },
        created_by: auth.principal.name.clone(),
        ..Default::default()
    };
    let (row, created) = s.store.rp_item_insert(&item).await?;
    if !created {
        if row.launch_id != info.launch.id {
            return Err(RpError::conflict("the item uuid is used in another launch"));
        }
        return Ok(started(ver, &row));
    }
    if pl.parent_becomes_container
        && let Some(pu) = &parent_uuid
    {
        s.store.rp_item_to_container(p.id, pu).await?;
        s.rp.items.remove(&(p.id, pu.clone()));
    }
    if row.role == "pending" {
        metrics::counter!("fastproof_rp_pending_total").increment(1);
    } else {
        s.rp.items.put((p.id, uuid.clone()), ItemRoute::of(&row));
        if client_uuid.is_some() {
            resolve_pending(&s, &p, &row).await?;
        }
    }
    if running {
        super::settle::send_running(&s, &p, &info, &row);
    }
    Ok(started(ver, &row))
}

/// Places the items that waited for `parent` (and theirs, in turn).
pub async fn resolve_pending(
    s: &AppState,
    project: &Project,
    parent: &RpItem,
) -> anyhow::Result<()> {
    let settle = s.cfg.rp.settle.as_millis() as i64;
    let mut queue = VecDeque::from([parent.clone()]);
    let mut done = 0usize;
    while let Some(par) = queue.pop_front() {
        let view = parent_of(&par);
        for child in s
            .store
            .rp_pending_children(project.id, par.launch_id, &par.uuid)
            .await?
        {
            done += 1;
            if done > 100_000 {
                return Ok(());
            }
            let pl = classify::place(
                &child.uuid,
                &child.name,
                &child.item_type,
                child.has_stats,
                Some(&view),
            );
            if pl.parent_becomes_container {
                s.store.rp_item_to_container(project.id, &par.uuid).await?;
                s.rp.items.remove(&(project.id, par.uuid.clone()));
            }
            if let Some(r) = s
                .store
                .rp_resolve_pending(project.id, &child.uuid, &stored(&pl), false, settle)
                .await?
            {
                s.rp.items
                    .put((project.id, r.uuid.clone()), ItemRoute::of(&r));
                queue.push_back(r);
            }
        }
    }
    Ok(())
}

/// The issue of a finish request as stored: `{group, custom, comment, externalSystemIssues}`;
/// `None` for no issue or `NOT_ISSUE`.
async fn stored_issue(
    s: &AppState,
    project: &Project,
    issue: &fastproof_rp::model::Issue,
) -> anyhow::Result<Option<serde_json::Value>> {
    let Some(t) = issue.issue_type.as_deref().filter(|t| !t.trim().is_empty()) else {
        return Ok((!issue.external_system_issues.is_empty())
            .then(|| json!({"externalSystemIssues": issue.external_system_issues.iter().take(20).collect::<Vec<_>>()})));
    };
    let lower = t.trim().to_ascii_lowercase();
    let builtin = lower == "not_issue"
        || ids::GROUPS
            .iter()
            .any(|g| g.locator == lower || g.type_ref.eq_ignore_ascii_case(t.trim()));
    let custom: Vec<(String, String)> = if builtin {
        Vec::new()
    } else {
        s.store
            .defect_types(project.id)
            .await?
            .into_iter()
            .filter(|d| !d.built_in)
            .map(|d| (d.id.to_string(), d.group))
            .collect()
    };
    let resolved = ids::issue_type(t, &custom);
    let (group, custom) = match resolved {
        None | Some(IssueType::NotIssue) => return Ok(None),
        Some(IssueType::Defect { group, custom }) => (group, custom),
    };
    Ok(Some(json!({
        "group": group,
        "custom": custom,
        "comment": issue.comment.as_deref().map(|c| cut(&clean_text(c), 4000)),
        "externalSystemIssues": issue.external_system_issues.iter().take(20).collect::<Vec<_>>(),
    })))
}

/// `PUT item/{uuid}`: finishes an item. Results settle (`FASTPROOF_RP_SETTLE_MS`) before they
/// are emitted; a repeated finish is a no-op.
pub async fn finish(
    State(s): State<AppState>,
    auth: Result<Auth, ApiError>,
    Path((v, project, iid)): Path<(String, String, String)>,
    body: Result<Bytes, BytesRejection>,
) -> RpResult<Response> {
    version(&v)?;
    let (auth, p) = s
        .rp_project(auth, &project, &s.perms.launches_write)
        .await?;
    let rq: FinishItem = parse(&s, body, true)?;
    let message = json!({"message": format!("TestItem with ID = '{iid}' successfully finished.")});
    if !ids::valid_uuid(&iid) {
        return Err(RpError::not_found("test item"));
    }
    let row = s
        .store
        .rp_item(p.id, &iid)
        .await?
        .ok_or_else(|| RpError::not_found("test item"))?;
    let info = s.rp_launch_info(&p, &row.rp_launch).await?;
    s.rp_contributor(&auth, &p, &info).await?;
    if !(row.state == "started" || (row.state == "pending" && row.stop_ms.is_none())) {
        return Ok(json(StatusCode::OK, message));
    }
    let role = Role::parse(&row.role).unwrap_or(Role::Container);
    let status = rq
        .status
        .as_deref()
        .map(|x| cut(x.trim(), 32))
        .filter(|x| !x.is_empty());
    let failed = matches!(
        item_status(status.as_deref()),
        Some((
            fastproof_model::Status::Failed | fastproof_model::Status::Broken,
            _
        ))
    );
    let issue = match (&rq.issue, role) {
        (Some(i), Role::Result | Role::Fixture | Role::Pending) => stored_issue(&s, &p, i).await?,
        _ => None,
    };
    let triage_by = issue
        .as_ref()
        .filter(|i| i.get("group").is_some_and(|g| g.is_string()))
        .filter(|_| {
            auth.principal
                .denial(&s.perms.results_triage, Some(&p.key))
                .is_none()
        })
        .map(|_| auth.principal.name.clone());
    let state = match role {
        Role::Fixture if !failed => "emitted",
        Role::Pending => "pending",
        _ => "finished",
    };
    let f = ItemFinish {
        stop_ms: bounded_or_now(&rq.end_time, now_ms()),
        status,
        issue,
        description: text(rq.description.as_deref(), MAX_DESCRIPTION_BYTES),
        attrs: rq.attributes.as_ref().map(|a| json!(clean_attributes(a))),
        test_case_id: text(rq.test_case_id.as_deref(), MAX_REF_BYTES),
        retry: rq.retry,
        retry_of: rq
            .retry_of
            .as_deref()
            .map(str::trim)
            .filter(|r| ids::valid_uuid(r))
            .map(str::to_string),
        triage_by,
        state: state.to_string(),
        settle_ms: s.cfg.rp.settle.as_millis() as i64,
    };
    if let Some(u) = s.store.rp_item_finish(p.id, &iid, &f).await? {
        if u.role == "container" && u.running_sent {
            super::settle::end_flipped(&s, &p, &info, &u).await;
        }
        if u.role == "fixture" {
            metrics::counter!("fastproof_rp_fixtures_total", "failed" => failed.to_string())
                .increment(1);
        }
    }
    s.rp_count_items(&p, &info, 0).await?;
    Ok(json(StatusCode::OK, message))
}

/// `GET item/uuid/{uuid}`: the numeric id and the basics of an item.
pub async fn by_uuid(
    State(s): State<AppState>,
    auth: Result<Auth, ApiError>,
    Path((v, project, iid)): Path<(String, String, String)>,
) -> RpResult<Response> {
    version(&v)?;
    let (_, p) = s.rp_project(auth, &project, &s.perms.launches_read).await?;
    if !ids::valid_uuid(&iid) {
        return Err(RpError::not_found("test item"));
    }
    let r = s
        .store
        .rp_item(p.id, &iid)
        .await?
        .ok_or_else(|| RpError::not_found("test item"))?;
    let mut out = json!({
        "id": r.num_id,
        "uuid": r.uuid,
        "name": r.name,
        "type": r.item_type,
        "hasStats": r.has_stats,
        "launchUuid": r.rp_launch,
        "status": r.status.clone().map(|x| x.to_ascii_uppercase()).unwrap_or_else(|| "IN_PROGRESS".into()),
        "startTime": iso(r.start_ms),
        "codeRef": r.code_ref,
        "testCaseId": r.test_case_id,
        "description": r.description,
        "attributes": r.attrs,
        "parameters": r.params,
    });
    if let Some(t) = r.stop_ms {
        out["endTime"] = json!(iso(t));
    }
    if let Some(pu) = &r.parent_uuid
        && let Some(par) = s.store.rp_item(p.id, pu).await?
    {
        out["parent"] = json!(par.num_id);
    }
    Ok(json(StatusCode::OK, out))
}

/// `PUT item/{id}/update`: description and attributes of an item that was not emitted yet
/// (by numeric id or uuid).
pub async fn update(
    State(s): State<AppState>,
    auth: Result<Auth, ApiError>,
    Path((v, project, iid)): Path<(String, String, String)>,
    body: Result<Bytes, BytesRejection>,
) -> RpResult<Response> {
    version(&v)?;
    let (auth, p) = s
        .rp_project(auth, &project, &s.perms.launches_write)
        .await?;
    let rq: UpdateItem = parse(&s, body, true)?;
    let row = match iid.parse::<i64>() {
        Ok(n) if iid.len() < 19 => s.store.rp_item_by_num(p.id, n).await?,
        _ if ids::valid_uuid(&iid) => s.store.rp_item(p.id, &iid).await?,
        _ => None,
    }
    .ok_or_else(|| RpError::not_found("test item"))?;
    let info = s.rp_launch_info(&p, &row.rp_launch).await?;
    s.rp_contributor(&auth, &p, &info).await?;
    let attrs = rq.attributes.as_ref().map(|a| json!(clean_attributes(a)));
    s.store
        .rp_item_update(
            p.id,
            &row.uuid,
            text(rq.description.as_deref(), MAX_DESCRIPTION_BYTES).as_deref(),
            attrs.as_ref(),
        )
        .await?;
    Ok(json(
        StatusCode::OK,
        json!({"message": format!("TestItem with ID = '{iid}' successfully updated.")}),
    ))
}
