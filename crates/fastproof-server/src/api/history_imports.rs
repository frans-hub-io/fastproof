//! Import of the history of Allure reports: `POST /projects/{key}/history-imports` takes the
//! history files of an Allure 2 report (`history.json` and the four trend files) or an Allure 3
//! history file, spools them with per-file size limits and queues a background job that
//! turns every past report into a launch. Permission: `settings:write` to import,
//! `launches:read` to read the status.

use axum::Json;
use axum::extract::{FromRequest, Multipart, Request, State};
use axum::http::{StatusCode, header};
use fastproof_blob::BlobKey;
use fastproof_ingest::allure_history::HistoryLimits;
use fastproof_store::history_import::{CreateDenied, HistoryImport, ImportFile};
use franshub_auth::Auth;
use franshub_http::{ApiError, ApiResult};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use uuid::Uuid;

use super::uploads::{clean_file_name, identity_encoding, spool_error};
use super::{ErrorBody, P, Q};
use crate::history_import::{FIELDS, max_file_bytes, spawn_job};
use crate::limits::caller_key;
use crate::state::AppState;

/// Imports listed.
const LIST_LIMIT: i64 = 20;
/// Length of a branch or an environment.
const MAX_LABEL_BYTES: usize = 200;

pub fn router() -> OpenApiRouter<AppState> {
    let create = OpenApiRouter::<AppState>::new()
        .routes(routes!(create_import))
        .layer(axum::extract::DefaultBodyLimit::disable());
    OpenApiRouter::new()
        .merge(create)
        .routes(routes!(list_imports))
        .routes(routes!(get_import))
}

/// Query of `POST .../history-imports`.
#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(rename_all = "camelCase")]
pub struct ImportQuery {
    /// `allure2` (history directory of an Allure 2 report) or `allure3` (history file).
    pub source: String,
    /// Branch of the launches made (set it to the branch live runs report, so their flaky
    /// detection and new-failure baselines read this history).
    pub branch: Option<String>,
    /// Environment key of the launches made; part of every series key, as for live runs.
    pub environment: Option<String>,
}

/// Imports of a project, newest first.
#[derive(Serialize, ToSchema)]
pub struct ImportList {
    pub items: Vec<HistoryImport>,
}

fn denied(d: CreateDenied) -> ApiError {
    match d {
        CreateDenied::Busy => ApiError::new(
            StatusCode::CONFLICT,
            "history_import_busy",
            "an import of this project or by you is queued or running, or the server is busy; retry when it is done",
        ),
        CreateDenied::Quota => ApiError::new(
            StatusCode::CONFLICT,
            "history_import_quota",
            "the imports of the last 24 hours are used up; retry later",
        ),
    }
}

fn label(name: &str, v: Option<&str>) -> ApiResult<Option<String>> {
    let Some(v) = v.map(str::trim).filter(|v| !v.is_empty()) else {
        return Ok(None);
    };
    if v.len() > MAX_LABEL_BYTES || v.chars().any(char::is_control) {
        return Err(ApiError::bad_request(format!(
            "{name} must be printable text of at most {MAX_LABEL_BYTES} bytes"
        )));
    }
    Ok(Some(v.to_string()))
}

#[utoipa::path(post, path = "/api/v1/projects/{key}/history-imports", tag = "history-imports", operation_id = "history_imports_create",
    description = "Imports the history of Allure reports as finished launches dated at their reports, joined to the tests \
        live results use (the Allure `historyId` is the FastProof series key). Multipart fields: `history` (required: \
        `history/history.json` of an Allure 2 report, or the Allure 3 history file, a JSON line per report) and, for \
        `allure2`, `historyTrend`, `durationTrend`, `categoriesTrend` and `retryTrend`. Limits: `history.json` 32 MiB, \
        an Allure 3 file 256 MiB (a line 32 MiB, 1000 reports), each trend file 4 MiB, 500 000 results, 200 000 tests \
        in `history.json`. One import per project at a time; 20 per project per day. A second import of the same files \
        adds nothing. Reports older than the project's retention window are left out. Needs `settings:write`.",
    params(("key" = String, Path), ImportQuery),
    request_body(content = Vec<u8>, content_type = "multipart/form-data", description = "fields `history`, `historyTrend`, `durationTrend`, `categoriesTrend`, `retryTrend`"),
    responses((status = 202, body = HistoryImport), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody), (status = 409, body = ErrorBody), (status = 413, body = ErrorBody)))]
pub async fn create_import(
    State(s): State<AppState>,
    auth: Auth,
    P(key): P<String>,
    Q(q): Q<ImportQuery>,
    req: Request,
) -> ApiResult<(StatusCode, Json<HistoryImport>)> {
    let project = s.project_for(&auth, &key, &s.perms.settings_write).await?;
    let source = match q.source.as_str() {
        "allure2" | "allure3" => q.source.clone(),
        _ => return Err(ApiError::bad_request("source must be allure2 or allure3")),
    };
    let branch = label("branch", q.branch.as_deref())?;
    let environment = label("environment", q.environment.as_deref())?;
    identity_encoding(&req)?;
    let owner = caller_key(&auth);
    s.store
        .history_import_precheck(project.id, &owner)
        .await?
        .map_err(denied)?;
    let _spool = s.guards.spool().await?;
    let id = Uuid::now_v7();
    let dir = s.cfg.spool_dir.join("history-imports-incoming");
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(anyhow::Error::from)?;
    let limits = HistoryLimits::default();
    let is_multipart = req
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.to_ascii_lowercase().starts_with("multipart/form-data"));
    if !is_multipart {
        return Err(ApiError::bad_request(
            "send multipart/form-data with the history files as fields",
        ));
    }
    let mut mp = Multipart::from_request(req, &s)
        .await
        .map_err(|e| ApiError::bad_request(format!("invalid multipart body: {}", e.body_text())))?;
    let mut spooled: Vec<(String, fastproof_blob::Spooled)> = Vec::new();
    let cleanup = |files: &[(String, fastproof_blob::Spooled)]| {
        for (_, f) in files {
            f.remove();
        }
    };
    let result: ApiResult<()> = async {
        while let Some(field) = mp.next_field().await.map_err(|e| {
            ApiError::bad_request(format!("invalid multipart body: {}", e.body_text()))
        })? {
            let field_name = field.name().unwrap_or("").to_string();
            let Some((_, blob)) = FIELDS.iter().find(|(f, _)| *f == field_name) else {
                return Err(ApiError::bad_request(format!(
                    "unknown field `{}`: use history, historyTrend, durationTrend, categoriesTrend or retryTrend",
                    clean_file_name(&field_name).unwrap_or_default()
                )));
            };
            if *blob != "history" && source == "allure3" {
                return Err(ApiError::bad_request(
                    "an Allure 3 import takes the `history` field only",
                ));
            }
            if spooled.iter().any(|(n, _)| n == blob) {
                return Err(ApiError::bad_request(format!(
                    "the field `{field_name}` is sent twice"
                )));
            }
            let path = dir.join(format!("{id}-{blob}"));
            let max = max_file_bytes(&source, blob, &limits).min(s.cfg.max_upload_bytes);
            let f = fastproof_blob::spool_stream(&path, Box::pin(field), max)
                .await
                .map_err(|e| spool_error(e, &format!("max_{}_bytes", blob.replace('-', "_"))))?;
            spooled.push((blob.to_string(), f));
        }
        if !spooled.iter().any(|(n, f)| n == "history" && f.size > 0) {
            return Err(ApiError::bad_request(
                "the multipart body has no non-empty `history` field",
            ));
        }
        Ok(())
    }
    .await;
    if let Err(e) = result {
        cleanup(&spooled);
        return Err(e);
    }
    let mut files: Vec<ImportFile> = Vec::new();
    for (name, f) in &spooled {
        files.push(ImportFile {
            name: name.clone(),
            blob_key: BlobKey::tm_job(&id.to_string(), name)?.to_string(),
            size: f.size as i64,
        });
    }
    let created = s
        .store
        .create_history_import(
            id,
            project.id,
            &source,
            branch.as_deref(),
            environment.as_deref(),
            &files,
            &auth.principal.name,
            &owner,
        )
        .await;
    let job = match created {
        Ok(Ok(j)) => j,
        Ok(Err(d)) => {
            cleanup(&spooled);
            return Err(denied(d));
        }
        Err(e) => {
            cleanup(&spooled);
            return Err(e.into());
        }
    };
    let mut put: ApiResult<()> = Ok(());
    for ((_, f), file) in spooled.iter().zip(&files) {
        let stored = match BlobKey::parse(&file.blob_key) {
            Ok(k) => s.blobs.put_file(&k, &f.path).await,
            Err(e) => Err(e),
        };
        if let Err(e) = stored {
            put = Err(e.into());
            break;
        }
    }
    cleanup(&spooled);
    if let Err(e) = put {
        let _ = s
            .store
            .fail_history_import(id, "the files could not be stored")
            .await;
        return Err(e);
    }
    s.audit(
        &auth,
        "history.import",
        Some(&project.key),
        serde_json::json!({"importId": id, "source": source, "branch": branch,
            "environment": environment,
            "bytes": files.iter().map(|f| f.size).sum::<i64>()}),
    )
    .await;
    spawn_job(&s, id);
    Ok((StatusCode::ACCEPTED, Json(job)))
}

#[utoipa::path(get, path = "/api/v1/projects/{key}/history-imports", tag = "history-imports", operation_id = "history_imports_list",
    description = "The newest 20 imports of history of a project. Needs `launches:read`.",
    params(("key" = String, Path)),
    responses((status = 200, body = ImportList), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)))]
pub async fn list_imports(
    State(s): State<AppState>,
    auth: Auth,
    P(key): P<String>,
) -> ApiResult<Json<ImportList>> {
    let project = s.project_for(&auth, &key, &s.perms.launches_read).await?;
    let items = s
        .store
        .history_imports(project.id, LIST_LIMIT)
        .await?
        .into_iter()
        .map(|(i, _)| i)
        .collect();
    Ok(Json(ImportList { items }))
}

#[utoipa::path(get, path = "/api/v1/projects/{key}/history-imports/{id}", tag = "history-imports", operation_id = "history_imports_get",
    description = "Status and counters of an import. Needs `launches:read`.",
    params(("key" = String, Path), ("id" = Uuid, Path)),
    responses((status = 200, body = HistoryImport), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)))]
pub async fn get_import(
    State(s): State<AppState>,
    auth: Auth,
    P((key, id)): P<(String, Uuid)>,
) -> ApiResult<Json<HistoryImport>> {
    let project = s.project_for(&auth, &key, &s.perms.launches_read).await?;
    let (job, _) = s
        .store
        .history_import(project.id, id)
        .await?
        .ok_or_else(|| ApiError::not_found("import"))?;
    Ok(Json(job))
}
