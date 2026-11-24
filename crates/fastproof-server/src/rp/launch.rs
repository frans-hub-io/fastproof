//! Launches, `GET /rp/api/info` and `GET settings` of the ReportPortal endpoint.

use std::collections::BTreeMap;

use axum::body::Bytes;
use axum::extract::rejection::BytesRejection;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::Response;
use chrono::{DateTime, Utc};
use fastproof_rp::model::{
    FinishLaunch, ItemAttribute, StartLaunch, UpdateLaunch, clean_attributes,
};
use fastproof_rp::settings::RpSettings;
use fastproof_rp::time::{bounded_or_now, iso, now_ms};
use fastproof_rp::{clean_name, cut, ids};
use fastproof_store::analysis::launches::LaunchPatch;
use fastproof_store::launches::LaunchStart;
use fastproof_store::{Attribute, Launch, LaunchCi, LaunchEnvironment, LaunchVcs, Project};
use franshub_auth::Auth;
use franshub_http::ApiError;
use serde_json::json;

use super::{LaunchInfo, RP_VERSION, RpError, RpResult, json, parse, version};
use crate::state::AppState;

/// `GET /rp/api/info`: the version agents adapt to (ISO-8601 times from 5.13.2 on). No
/// `extensions`, so agents see no ReportPortal analytics instance.
pub async fn info(
    State(s): State<AppState>,
    auth: Result<Auth, ApiError>,
    v: Option<Path<String>>,
) -> RpResult<Response> {
    if let Some(Path(v)) = v {
        version(&v)?;
    }
    let _ = (auth?, &s);
    Ok(json(
        StatusCode::OK,
        json!({"build": {"version": RP_VERSION, "name": "FastProof RP compatibility"}}),
    ))
}

/// The `rp` settings of a project (defaults when absent or unreadable).
pub async fn rp_settings(s: &AppState, project: &Project) -> anyhow::Result<RpSettings> {
    let doc = serde_json::to_value(s.store.project_settings(project.id).await?)?;
    Ok(RpSettings::from_doc(&doc).unwrap_or_default())
}

/// Launch fields of ReportPortal attributes and a description: system attributes are
/// environment values; mapped keys fill CI and VCS fields (and stay attributes); the
/// configured environment attribute names the environment; the description is the
/// `description` attribute.
pub fn launch_fields(
    attrs: &[ItemAttribute],
    description: Option<&str>,
    settings: &RpSettings,
) -> (
    Vec<Attribute>,
    Option<LaunchCi>,
    Option<LaunchVcs>,
    Option<LaunchEnvironment>,
) {
    let map = settings.attribute_map();
    let mut attributes = Vec::new();
    let mut ci = LaunchCi::default();
    let mut vcs = LaunchVcs::default();
    let mut env = LaunchEnvironment::default();
    for a in clean_attributes(attrs) {
        let value = a.value.clone().unwrap_or_default();
        if a.system {
            if env.values.len() < 200 {
                env.values.insert(
                    cut(a.key.as_deref().unwrap_or("system"), 256),
                    cut(&value, 2048),
                );
            }
            continue;
        }
        if let Some(k) = &a.key {
            if settings.environment_attribute.as_deref() == Some(k.as_str()) {
                env.key = Some(cut(&value, 256));
            }
            match map.get(k).map(String::as_str) {
                Some("vcs.branch") => vcs.branch = Some(value.clone()),
                Some("vcs.commit") => vcs.commit = Some(value.clone()),
                Some("vcs.repository") => vcs.repository = Some(value.clone()),
                Some("ci.url") => ci.url = Some(value.clone()),
                Some("ci.pipelineId") => ci.pipeline_id = Some(value.clone()),
                Some("ci.jobId") => ci.job_id = Some(value.clone()),
                Some("ci.provider") => ci.provider = Some(value.clone()),
                Some("ci.buildOrder") => ci.build_order = value.trim().parse().ok(),
                _ => {}
            }
        }
        if attributes.len() < 99 {
            attributes.push(Attribute {
                key: a.key.clone().unwrap_or_else(|| "tag".into()),
                value,
            });
        }
    }
    if let Some(d) = description.map(str::trim).filter(|d| !d.is_empty()) {
        attributes.push(Attribute {
            key: "description".into(),
            value: cut(d, 1024),
        });
    }
    if ci
        .url
        .as_deref()
        .is_some_and(|u| !(u.starts_with("https://") || u.starts_with("http://")))
    {
        ci.url = None;
    }
    let ci = (ci != LaunchCi::default()).then_some(ci);
    let vcs = (vcs != LaunchVcs::default()).then_some(vcs);
    let env = (env != LaunchEnvironment::default()).then_some(env);
    (attributes, ci, vcs, env)
}

/// Agents on the ReportPortal Python client send log lines in batches, often after their
/// tests finished: their results wait for the launch to finish, so that error texts and files
/// land in them (other agents are held only once a late line shows up).
pub fn batches_logs(attrs: &[ItemAttribute]) -> bool {
    attrs.iter().any(|a| {
        a.system
            && matches!(a.key.as_deref(), Some("agent" | "client"))
            && a.value.as_deref().is_some_and(|v| {
                let v = v.to_ascii_lowercase();
                [
                    "pytest-reportportal",
                    "robotframework-reportportal",
                    "behave-reportportal",
                    "reportportal-client",
                ]
                .iter()
                .any(|p| v.starts_with(p))
            })
    })
}

fn started(uuid: &str, number: i64) -> Response {
    json(StatusCode::CREATED, json!({"id": uuid, "number": number}))
}

/// The ReportPortal uuid that names a FastProof launch: an existing mapping, else its id.
async fn rp_uuid_of(
    s: &AppState,
    project: &Project,
    launch: &Launch,
    creator: &str,
) -> RpResult<String> {
    let uuid = match s.store.rp_launches_of(launch.id).await?.into_iter().next() {
        Some(r) => r.uuid,
        None => launch.id.to_string(),
    };
    s.store
        .rp_launch_insert(project.id, &uuid, launch.id, creator)
        .await?;
    Ok(uuid)
}

/// `POST launch`: starts a launch, or joins the launch the client uuid names, or (`rerun`)
/// opens the caller's earlier launch again.
pub async fn start(
    State(s): State<AppState>,
    auth: Result<Auth, ApiError>,
    Path((v, project)): Path<(String, String)>,
    body: Result<Bytes, BytesRejection>,
) -> RpResult<Response> {
    version(&v)?;
    let (auth, p) = s
        .rp_project(auth, &project, &s.perms.launches_write)
        .await?;
    let rq: StartLaunch = parse(&s, body, false)?;
    let name = clean_name(rq.name.as_deref().unwrap_or_default());
    if name.is_empty() {
        return Err(RpError::bad_request("name is required"));
    }
    if let Some(u) = &rq.uuid
        && !ids::valid_uuid(u)
    {
        return Err(RpError::bad_request(
            "uuid must have 1 to 64 characters of letters, digits, '.', '_' and '-'",
        ));
    }
    let who = auth.principal.name.clone();
    if rq.rerun {
        let target = match rq.rerun_of.as_deref().filter(|r| !r.trim().is_empty()) {
            Some(r) => Some(s.rp_launch_info(&p, r.trim()).await?.launch.id),
            None => s.store.rp_latest_launch_named(p.id, &name).await?,
        };
        if let Some(id) = target {
            let launch = s.launch_of(&p, id).await?;
            s.refuse_merged(&launch)?;
            let creator = s.store.launch_creator(id).await?.unwrap_or_default();
            if creator != who {
                return Err(RpError::conflict(
                    "rerun: only the creator of a launch may open it again",
                ));
            }
            s.store.rp_reopen(p.id, id).await?;
            let uuid = rp_uuid_of(&s, &p, &launch, &creator).await?;
            s.rp.launches.remove(&(p.id, uuid.clone()));
            s.audit(
                &auth,
                "launch.rerun",
                Some(&p.key),
                json!({"launchId": id, "number": launch.number, "source": "rp"}),
            )
            .await;
            return Ok(started(&uuid, launch.number));
        }
    }
    if let Some(u) = &rq.uuid
        && let Ok(info) = s.rp_launch_info(&p, u).await
    {
        s.rp_contributor(&auth, &p, &info).await?;
        return Ok(started(u, info.launch.number));
    }
    let settings = rp_settings(&s, &p).await?;
    let (attributes, ci, vcs, environment) =
        launch_fields(&rq.attributes, rq.description.as_deref(), &settings);
    let debug = rq
        .mode
        .as_deref()
        .is_some_and(|m| m.eq_ignore_ascii_case("debug"));
    let start = LaunchStart {
        external_id: rq.uuid.as_ref().map(|u| format!("rp:{u}")),
        name: cut(&name, 300),
        attributes: Some(attributes),
        ci,
        vcs: vcs.map(|v| v.with_commit_url(None)),
        environment,
        debug: Some(debug),
    };
    crate::api::launches::validate_start(&start)?;
    let can_sync = auth
        .principal
        .denial(&s.perms.cases_sync, Some(&p.key))
        .is_none();
    let (launch, created) = s.store.start_launch_as(&p, &start, &who, can_sync).await?;
    if created {
        s.store
            .set_launch_owner_key(launch.id, &crate::limits::caller_key(&auth))
            .await?;
        let now = now_ms();
        if rq.start_time.is_number() || rq.start_time.is_string() {
            let t = bounded_or_now(&rq.start_time, now);
            if let Some(at) = DateTime::<Utc>::from_timestamp_millis(t) {
                s.store.rp_set_started_at(p.id, launch.id, at).await?;
            }
        }
        s.audit(
            &auth,
            "launch.create",
            Some(&p.key),
            json!({"launchId": launch.id, "number": launch.number, "name": launch.name,
                "externalId": launch.external_id, "debug": launch.debug, "source": "rp"}),
        )
        .await;
    }
    let uuid = rq.uuid.clone().unwrap_or_else(|| launch.id.to_string());
    let creator = s.store.launch_creator(launch.id).await?.unwrap_or(who);
    s.store
        .rp_launch_insert(p.id, &uuid, launch.id, &creator)
        .await?;
    if created && batches_logs(&rq.attributes) {
        s.store.rp_set_hold(p.id, &uuid).await?;
    }
    Ok(started(&uuid, launch.number))
}

/// The link of a launch in the FastProof UI.
fn link(s: &AppState, project: &Project, launch: &Launch) -> String {
    let path = format!("/p/{}/launches/{}", project.key, launch.id);
    match &s.cfg.public_url {
        Some(u) => format!("{}{path}", u.as_str().trim_end_matches('/')),
        None => path,
    }
}

/// The launch of a `{lid}` segment: a ReportPortal uuid, or a launch number.
async fn launch_of_segment(s: &AppState, project: &Project, lid: &str) -> RpResult<LaunchInfo> {
    if let Ok(n) = lid.parse::<i64>()
        && lid.len() < 19
        && let Some(l) = s.store.rp_launch_by_number(project.id, n).await?
    {
        let creator = s.store.launch_creator(l).await?.unwrap_or_default();
        let launch = s.launch_of(project, l).await?;
        let uuid = rp_uuid_of(s, project, &launch, &creator).await?;
        return s.rp_launch_info(project, &uuid).await;
    }
    s.rp_launch_info(project, lid).await
}

/// `PUT launch/{uuid}/finish`: asks to finish the launch. Results still settling are emitted
/// at once; the launch finishes when no test is open, or after
/// `FASTPROOF_RP_FINISH_GRACE_SECONDS` with the open ones interrupted.
pub async fn finish(
    State(s): State<AppState>,
    auth: Result<Auth, ApiError>,
    Path((v, project, lid)): Path<(String, String, String)>,
    body: Result<Bytes, BytesRejection>,
) -> RpResult<Response> {
    version(&v)?;
    let (auth, p) = s
        .rp_project(auth, &project, &s.perms.launches_write)
        .await?;
    let rq: FinishLaunch = parse(&s, body, true)?;
    let info = s.rp_launch_info(&p, &lid).await?;
    let current = s.launch_of(&p, info.launch.id).await?;
    s.refuse_merged(&current)?;
    if auth
        .principal
        .denial(&s.perms.cases_sync, Some(&p.key))
        .is_some()
    {
        s.untrusted_contributor(&auth, &p, &current, false).await?;
    }
    if !rq.attributes.is_empty() || rq.description.is_some() {
        let settings = rp_settings(&s, &p).await?;
        let (attrs, _, _, _) = launch_fields(&rq.attributes, rq.description.as_deref(), &settings);
        let mut merged = current.attributes.clone();
        for a in attrs {
            if !merged.contains(&a) && merged.len() < 100 {
                merged.push(a);
            }
        }
        if merged != current.attributes {
            s.store
                .patch_launch(
                    p.id,
                    &p.key,
                    current.id,
                    &LaunchPatch {
                        attributes: Some(merged),
                        ..Default::default()
                    },
                )
                .await?;
        }
    }
    let status = if fastproof_rp::status::launch_interrupted(rq.status.as_deref()) {
        "INTERRUPTED"
    } else {
        "FINISHED"
    };
    s.store
        .rp_request_finish(p.id, &info.uuid, Some(status), false)
        .await?;
    s.store
        .rp_settle_by(current.id, s.cfg.rp.settle.as_millis() as i64)
        .await?;
    s.rp.launches.remove(&(p.id, info.uuid.clone()));
    s.rp.wake.notify_one();
    Ok(json(
        StatusCode::OK,
        json!({"id": info.uuid, "number": current.number, "link": link(&s, &p, &current)}),
    ))
}

/// `PUT launch/{id}/update`: mode, description and attributes.
pub async fn update(
    State(s): State<AppState>,
    auth: Result<Auth, ApiError>,
    Path((v, project, lid)): Path<(String, String, String)>,
    body: Result<Bytes, BytesRejection>,
) -> RpResult<Response> {
    version(&v)?;
    let (auth, p) = s
        .rp_project(auth, &project, &s.perms.launches_write)
        .await?;
    let rq: UpdateLaunch = parse(&s, body, true)?;
    let info = launch_of_segment(&s, &p, &lid).await?;
    s.rp_contributor(&auth, &p, &info).await?;
    if rq.attributes.is_some() || rq.description.is_some() {
        let settings = rp_settings(&s, &p).await?;
        let (attrs, _, _, _) = launch_fields(
            rq.attributes.as_deref().unwrap_or_default(),
            rq.description.as_deref(),
            &settings,
        );
        let current = s.launch_of(&p, info.launch.id).await?;
        let mut merged: Vec<Attribute> = if rq.attributes.is_some() {
            current
                .attributes
                .iter()
                .filter(|a| a.key == "description")
                .cloned()
                .collect()
        } else {
            current.attributes.clone()
        };
        for a in attrs {
            merged.retain(|x| !(x.key == "description" && a.key == "description"));
            if merged.len() < 100 {
                merged.push(a);
            }
        }
        s.store
            .patch_launch(
                p.id,
                &p.key,
                info.launch.id,
                &LaunchPatch {
                    attributes: Some(merged),
                    ..Default::default()
                },
            )
            .await?;
    }
    Ok(json(
        StatusCode::OK,
        json!({"message": format!("Launch with ID = '{lid}' successfully updated.")}),
    ))
}

/// A minimal `LaunchResource`.
fn resource(uuid: &str, l: &Launch) -> serde_json::Value {
    let status = match l.status.as_str() {
        "running" => "IN_PROGRESS",
        "interrupted" => "INTERRUPTED",
        _ if l.counts.failed + l.counts.broken > 0 => "FAILED",
        _ => "PASSED",
    };
    let attributes: Vec<serde_json::Value> = l
        .attributes
        .iter()
        .map(|a| json!({"key": a.key, "value": a.value, "system": false}))
        .collect();
    let mut r = json!({
        "id": l.number,
        "uuid": uuid,
        "number": l.number,
        "name": l.name,
        "status": status,
        "mode": if l.debug { "DEBUG" } else { "DEFAULT" },
        "startTime": iso(l.started_at.timestamp_millis()),
        "attributes": attributes,
        "statistics": {"executions": {"total": l.counts.total, "passed": l.counts.passed,
            "failed": l.counts.failed + l.counts.broken, "skipped": l.counts.skipped}, "defects": {}},
    });
    if let Some(f) = l.finished_at {
        r["endTime"] = json!(iso(f.timestamp_millis()));
    }
    r
}

/// `GET launch/uuid/{uuid}`.
pub async fn by_uuid(
    State(s): State<AppState>,
    auth: Result<Auth, ApiError>,
    Path((v, project, lid)): Path<(String, String, String)>,
) -> RpResult<Response> {
    version(&v)?;
    let (_, p) = s.rp_project(auth, &project, &s.perms.launches_read).await?;
    let info = s.rp_launch_lookup(&p, &lid).await?;
    let l = s.launch_of(&p, info.launch.id).await?;
    Ok(json(StatusCode::OK, resource(&info.uuid, &l)))
}

/// `GET launch`: a page of launches; `filter.eq.uuid` and `filter.in.uuid` (at most 50)
/// select by ReportPortal uuid, otherwise the newest come first (`page.size` at most 100).
pub async fn list(
    State(s): State<AppState>,
    auth: Result<Auth, ApiError>,
    Path((v, project)): Path<(String, String)>,
    Query(q): Query<BTreeMap<String, String>>,
) -> RpResult<Response> {
    version(&v)?;
    let (_, p) = s.rp_project(auth, &project, &s.perms.launches_read).await?;
    let size: i64 = q
        .get("page.size")
        .and_then(|v| v.parse().ok())
        .unwrap_or(20)
        .clamp(1, 100);
    let mut content = Vec::new();
    let uuids: Vec<String> = q
        .get("filter.in.uuid")
        .or(q.get("filter.eq.uuid"))
        .map(|v| {
            v.split(',')
                .map(|x| x.trim().to_string())
                .filter(|x| !x.is_empty())
                .collect()
        })
        .unwrap_or_default();
    if q.contains_key("filter.in.uuid") || q.contains_key("filter.eq.uuid") {
        if uuids.len() > 50 {
            return Err(RpError::bad_request("at most 50 uuids"));
        }
        for u in uuids {
            if let Ok(info) = s.rp_launch_lookup(&p, &u).await {
                let l = s.launch_of(&p, info.launch.id).await?;
                content.push(resource(&info.uuid, &l));
            }
        }
    } else {
        let f = fastproof_store::launches::LaunchFilter {
            limit: size,
            ..Default::default()
        };
        for l in s.store.launches(p.id, &f).await?.items {
            let uuid = match s.store.rp_launches_of(l.id).await?.into_iter().next() {
                Some(r) => r.uuid,
                None => l.id.to_string(),
            };
            content.push(resource(&uuid, &l));
        }
    }
    let n = content.len();
    Ok(json(
        StatusCode::OK,
        json!({"content": content, "page": {"number": 1, "size": size, "totalElements": n, "totalPages": 1}}),
    ))
}

/// A stable positive number of a project for `GET settings`.
fn project_number(p: &Project) -> i64 {
    let b = p.id.as_bytes();
    i64::from_be_bytes([0, 0, b[10], b[11], b[12], b[13], b[14], b[15]]) & 0x0000_ffff_ffff_ffff
}

/// `GET settings`: the defect types of the project as ReportPortal sub-types (built-in ones
/// with the ReportPortal locators, custom ones with stable `<prefix>_<hash8>` locators).
pub async fn settings(
    State(s): State<AppState>,
    auth: Result<Auth, ApiError>,
    Path((v, project)): Path<(String, String)>,
) -> RpResult<Response> {
    version(&v)?;
    let (_, p) = s.rp_project(auth, &project, &s.perms.launches_read).await?;
    let types = s.store.defect_types(p.id).await?;
    let mut sub: serde_json::Map<String, serde_json::Value> = serde_json::Map::new();
    let mut n = 0i64;
    for g in &ids::GROUPS {
        let mut list = Vec::new();
        for t in types.iter().filter(|t| t.group == g.group) {
            n += 1;
            let locator = if t.built_in {
                g.locator.to_string()
            } else {
                ids::custom_locator(g, &t.id.to_string())
            };
            list.push(json!({"id": n, "locator": locator, "typeRef": g.type_ref,
                "longName": t.name, "shortName": t.short_name, "color": t.color}));
        }
        if list.is_empty() {
            n += 1;
            list.push(json!({"id": n, "locator": g.locator, "typeRef": g.type_ref,
                "longName": g.long_name, "shortName": g.short_name, "color": g.color}));
        }
        sub.insert(g.type_ref.to_string(), serde_json::Value::Array(list));
    }
    Ok(json(
        StatusCode::OK,
        json!({"project": project_number(&p), "subTypes": sub}),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn attr(k: Option<&str>, v: &str, system: bool) -> ItemAttribute {
        ItemAttribute {
            key: k.map(Into::into),
            value: Some(v.into()),
            system,
        }
    }

    #[test]
    fn attributes_fill_launch_fields() {
        let settings = RpSettings {
            environment_attribute: Some("env".into()),
            ..Default::default()
        };
        let (attrs, ci, vcs, env) = launch_fields(
            &[
                attr(Some("build"), "42", false),
                attr(Some("branch"), "main", false),
                attr(None, "smoke", false),
                attr(Some("os"), "Linux", true),
                attr(Some("env"), "staging", false),
                attr(Some("ci.url"), "javascript:x", false),
            ],
            Some("nightly run"),
            &settings,
        );
        assert_eq!(ci.unwrap().build_order, Some(42));
        assert_eq!(vcs.unwrap().branch.as_deref(), Some("main"));
        let env = env.unwrap();
        assert_eq!(env.key.as_deref(), Some("staging"));
        assert_eq!(env.values["os"], "Linux");
        assert!(attrs.iter().any(|a| a.key == "tag" && a.value == "smoke"));
        assert!(
            attrs
                .iter()
                .any(|a| a.key == "description" && a.value == "nightly run")
        );
        assert!(!attrs.iter().any(|a| a.key == "os"));
        let (_, _, _, env) = launch_fields(
            &[attr(Some("env"), "x", false)],
            None,
            &RpSettings::default(),
        );
        assert!(env.is_none());
        assert!(batches_logs(&[attr(
            Some("agent"),
            "pytest-reportportal|5.6.13",
            true
        )]));
        assert!(!batches_logs(&[attr(
            Some("agent"),
            "agent-java-testng|5.6.8",
            true
        )]));
        assert!(!batches_logs(&[attr(
            Some("agent"),
            "pytest-reportportal",
            false
        )]));
    }
}
