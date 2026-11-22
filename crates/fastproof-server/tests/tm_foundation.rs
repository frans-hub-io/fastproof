//! Test-management foundation over HTTP: roles and permissions, folder and field routes with
//! their IDOR checks, error codes and audit, `caseSync` settings, `created_by_can_sync`, and
//! the TM part of the OpenAPI document.

mod common;

use std::time::Duration;

use common::{ADMIN_TOKEN, Server, TestDb};
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};

async fn audit_actions(s: &Server, want: &[&str]) -> Vec<Value> {
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    loop {
        let (st, a) = s.get("/api/v1/audit?limit=500", ADMIN_TOKEN).await;
        assert_eq!(st, StatusCode::OK, "{a}");
        let items = a["items"].as_array().cloned().unwrap_or_default();
        if want.iter().all(|w| items.iter().any(|i| i["action"] == *w)) {
            return items;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "audit lacks {want:?}: {items:?}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test]
async fn roles_include_test_management_permissions() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("roles").await;
    let viewer = s.token("v", "viewer", &["roles"]).await;
    let tester = s.token("t", "tester", &["roles"]).await;
    let uploader = s.token("u", "uploader", &["roles"]).await;
    let editor = s.token("e", "editor", &["roles"]).await;
    let folders = "/api/v1/projects/roles/case-folders";
    let fields = "/api/v1/projects/roles/case-fields";
    for t in [&viewer, &tester, &uploader, &editor] {
        assert_eq!(s.get(folders, t).await.0, StatusCode::OK);
        assert_eq!(s.get(fields, t).await.0, StatusCode::OK);
    }
    for t in [&viewer, &tester, &uploader] {
        let (st, b) = s.post(folders, t, json!({"name": "x"})).await;
        assert_eq!(st, StatusCode::FORBIDDEN, "{b}");
    }
    let (st, b) = s.post(folders, &editor, json!({"name": "x"})).await;
    assert_eq!(st, StatusCode::CREATED, "{b}");
    let field = json!({"key": "team", "name": "Team", "type": "text"});
    let (st, _) = s.post(fields, &editor, field.clone()).await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (st, b) = s.post(fields, ADMIN_TOKEN, field).await;
    assert_eq!(st, StatusCode::CREATED, "{b}");

    let (st, me) = s.get("/api/v1/tokens/options", ADMIN_TOKEN).await;
    assert_eq!(st, StatusCode::OK, "{me}");
    let text = me.to_string();
    for p in [
        "cases:read",
        "cases:write",
        "cases:import",
        "cases:sync",
        "plans:write",
        "runs:execute",
        "tester",
    ] {
        assert!(text.contains(p), "{p} missing from {me}");
    }

    let start = json!({"name": "ci"});
    let (st, l) = s
        .post("/api/v1/projects/roles/launches", &uploader, start.clone())
        .await;
    assert_eq!(st, StatusCode::CREATED, "{l}");
    let (st, _) = s
        .post("/api/v1/projects/roles/launches", &tester, start)
        .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let can_sync: bool =
        sqlx::query_scalar("select created_by_can_sync from launches where id = $1::uuid")
            .bind(l["id"].as_str().unwrap())
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert!(can_sync);
}

#[tokio::test]
async fn folder_routes_permissions_idor_and_codes() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start_with(&db, &[("FASTPROOF_TM_MAX_FOLDERS", "4")]).await;
    s.project("fa").await;
    s.project("fb").await;
    let outsider = s.token("o", "editor", &["fb"]).await;
    let base = "/api/v1/projects/fa/case-folders";
    let (st, root) = s.post(base, ADMIN_TOKEN, json!({"name": "Checkout"})).await;
    assert_eq!(st, StatusCode::CREATED, "{root}");
    assert_eq!(
        (root["depth"].as_i64(), root["hasChildren"].as_bool()),
        (Some(1), Some(false))
    );
    assert_eq!(
        root["counts"],
        json!({"total": 0, "manual": 0, "automated": 0})
    );
    let rid = root["id"].as_str().unwrap().to_string();
    let (st, child) = s
        .post(base, ADMIN_TOKEN, json!({"name": "Cards", "parentId": rid}))
        .await;
    assert_eq!(st, StatusCode::CREATED, "{child}");
    let cid = child["id"].as_str().unwrap().to_string();
    let (st, dup) = s.post(base, ADMIN_TOKEN, json!({"name": "checkout"})).await;
    assert_eq!(
        (st, dup["code"].as_str()),
        (StatusCode::CONFLICT, Some("folder_name_taken"))
    );
    let (st, b) = s
        .post(base, ADMIN_TOKEN, json!({"name": "x", "unknown": 1}))
        .await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY, "{b}");
    let (st, l) = s.get(base, ADMIN_TOKEN).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(l["items"].as_array().unwrap().len(), 1);
    assert_eq!(l["items"][0]["hasChildren"], true);
    let (st, l) = s.get(&format!("{base}?parent={rid}"), ADMIN_TOKEN).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(l["items"][0]["name"], "Cards");
    let (st, _) = s.get(&format!("{base}?parent=nope"), ADMIN_TOKEN).await;
    assert_eq!(st, StatusCode::BAD_REQUEST);

    let other = "/api/v1/projects/fb/case-folders";
    let (st, b) = s.get(&format!("{other}?parent={rid}"), ADMIN_TOKEN).await;
    assert_eq!(
        (st, b["code"].as_str()),
        (StatusCode::NOT_FOUND, Some("not_found"))
    );
    let (st, _) = s
        .post(
            other,
            ADMIN_TOKEN,
            json!({"name": "Sneaky", "parentId": rid}),
        )
        .await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    let (st, _) = s
        .call(
            Method::PATCH,
            &format!("{other}/{cid}"),
            Some(ADMIN_TOKEN),
            Some(json!({"name": "x"})),
        )
        .await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    let (st, _) = s
        .call(
            Method::DELETE,
            &format!("{other}/{cid}"),
            Some(ADMIN_TOKEN),
            None,
        )
        .await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    let (st, mine) = s.post(other, ADMIN_TOKEN, json!({"name": "Mine"})).await;
    assert_eq!(st, StatusCode::CREATED);
    let (st, _) = s
        .call(
            Method::PATCH,
            &format!("{base}/{cid}"),
            Some(ADMIN_TOKEN),
            Some(json!({"parentId": mine["id"]})),
        )
        .await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    for (m, path, body) in [
        (Method::GET, base.to_string(), None),
        (Method::POST, base.to_string(), Some(json!({"name": "y"}))),
        (
            Method::PATCH,
            format!("{base}/{cid}"),
            Some(json!({"name": "y"})),
        ),
        (Method::DELETE, format!("{base}/{cid}"), None),
        (
            Method::GET,
            "/api/v1/projects/nope/case-folders".to_string(),
            None,
        ),
    ] {
        let (st, b) = s
            .call(m.clone(), &path, Some(&outsider), body.clone())
            .await;
        assert_eq!(st, StatusCode::FORBIDDEN, "{m} {path}: {b}");
        let (st, _) = s.call(m.clone(), &path, None, body).await;
        assert_eq!(st, StatusCode::UNAUTHORIZED, "{m} {path}");
    }

    let (st, moved) = s
        .call(
            Method::PATCH,
            &format!("{base}/{cid}"),
            Some(ADMIN_TOKEN),
            Some(json!({"parentId": null, "name": "Cards top"})),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{moved}");
    assert_eq!(
        (moved["parentId"].clone(), moved["depth"].as_i64()),
        (Value::Null, Some(1))
    );
    let (st, back) = s
        .call(
            Method::PATCH,
            &format!("{base}/{cid}"),
            Some(ADMIN_TOKEN),
            Some(json!({"parentId": rid})),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{back}");
    let (st, cyc) = s
        .call(
            Method::PATCH,
            &format!("{base}/{rid}"),
            Some(ADMIN_TOKEN),
            Some(json!({"parentId": cid})),
        )
        .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{cyc}");

    for name in ["Third", "Fourth"] {
        let (st, _) = s.post(base, ADMIN_TOKEN, json!({"name": name})).await;
        assert_eq!(st, StatusCode::CREATED);
    }
    let (st, lim) = s.post(base, ADMIN_TOKEN, json!({"name": "Fifth"})).await;
    assert_eq!(st, StatusCode::CONFLICT, "{lim}");
    assert_eq!(lim["code"], "limit_reached");
    assert_eq!(lim["details"], json!({"limit": "maxFolders", "max": 4}));

    let (st, ne) = s
        .call(
            Method::DELETE,
            &format!("{base}/{rid}"),
            Some(ADMIN_TOKEN),
            None,
        )
        .await;
    assert_eq!(
        (st, ne["code"].as_str()),
        (StatusCode::CONFLICT, Some("folder_not_empty"))
    );
    let (st, _) = s
        .call(
            Method::DELETE,
            &format!("{base}/{rid}?mode=sideways"),
            Some(ADMIN_TOKEN),
            None,
        )
        .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    let (st, d) = s
        .call(
            Method::DELETE,
            &format!("{base}/{rid}?mode=moveUp"),
            Some(ADMIN_TOKEN),
            None,
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{d}");
    assert_eq!(
        d,
        json!({"folders": 1, "casesMoved": 0, "casesArchived": 0})
    );
    let (st, d) = s
        .call(
            Method::DELETE,
            &format!("{base}/{cid}?mode=archiveCases"),
            Some(ADMIN_TOKEN),
            None,
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{d}");

    let items = audit_actions(
        &s,
        &[
            "caseFolder.create",
            "caseFolder.update",
            "caseFolder.delete",
        ],
    )
    .await;
    let deleted = items
        .iter()
        .find(|i| i["action"] == "caseFolder.delete")
        .unwrap();
    assert_eq!(deleted["scope"], "fa");
}

#[tokio::test]
async fn field_routes_permissions_idor_and_audit() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("ka").await;
    s.project("kb").await;
    let base = "/api/v1/projects/ka/case-fields";
    let (st, l) = s.get(base, ADMIN_TOKEN).await;
    assert_eq!(st, StatusCode::OK);
    let keys: Vec<&str> = l["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["key"].as_str().unwrap())
        .collect();
    assert_eq!(
        keys,
        vec![
            "priority",
            "severity",
            "layer",
            "owner",
            "epic",
            "feature",
            "story",
            "suite",
            "component"
        ]
    );
    assert_eq!(l["items"][1]["sourceLabel"], "severity");
    assert_eq!(l["items"][0]["defaultValue"], Value::Null);
    let (st, f) = s
        .post(
            base,
            ADMIN_TOKEN,
            json!({"key": "platform", "name": "Platform", "type": "multiselect",
                "options": [{"value": "web"}, {"value": "ios", "color": "#112233"}],
                "defaultValue": ["web"]}),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED, "{f}");
    assert_eq!(f["options"][0], json!({"value": "web", "archived": false}));
    assert_eq!(f["defaultValue"], json!(["web"]));
    let id = f["id"].as_str().unwrap().to_string();
    let (st, e) = s
        .call(
            Method::PATCH,
            &format!("{base}/{id}"),
            Some(ADMIN_TOKEN),
            Some(json!({"type": "text"})),
        )
        .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{e}");
    let (st, e) = s
        .post(
            base,
            ADMIN_TOKEN,
            json!({"key": "platform", "name": "Again", "type": "text"}),
        )
        .await;
    assert_eq!(st, StatusCode::CONFLICT, "{e}");
    let (st, e) = s
        .post(
            base,
            ADMIN_TOKEN,
            json!({"key": "n", "name": "N", "type": "number", "defaultValue": "x"}),
        )
        .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{e}");
    let other = "/api/v1/projects/kb/case-fields";
    let (st, _) = s
        .call(
            Method::PATCH,
            &format!("{other}/{id}"),
            Some(ADMIN_TOKEN),
            Some(json!({"name": "x"})),
        )
        .await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    let (st, _) = s
        .call(
            Method::DELETE,
            &format!("{other}/{id}"),
            Some(ADMIN_TOKEN),
            None,
        )
        .await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    let editor = s.token("e", "editor", &["ka"]).await;
    let (st, _) = s
        .call(
            Method::PATCH,
            &format!("{base}/{id}"),
            Some(&editor),
            Some(json!({"name": "x"})),
        )
        .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (st, _) = s
        .call(Method::DELETE, &format!("{base}/{id}"), Some(&editor), None)
        .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (st, u) = s
        .call(
            Method::PATCH,
            &format!("{base}/{id}"),
            Some(ADMIN_TOKEN),
            Some(json!({"name": "Platforms", "type": "multiselect", "required": true})),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{u}");
    assert_eq!(
        (u["name"].as_str(), u["required"].as_bool()),
        (Some("Platforms"), Some(true))
    );
    let (st, _) = s
        .call(
            Method::DELETE,
            &format!("{base}/{id}"),
            Some(ADMIN_TOKEN),
            None,
        )
        .await;
    assert_eq!(st, StatusCode::NO_CONTENT);
    let (_, l) = s.get(base, ADMIN_TOKEN).await;
    assert_eq!(l["items"].as_array().unwrap().len(), 9);
    let (_, l) = s.get(&format!("{base}?archived=true"), ADMIN_TOKEN).await;
    assert_eq!(l["items"].as_array().unwrap().len(), 10);
    audit_actions(
        &s,
        &["caseField.create", "caseField.update", "caseField.delete"],
    )
    .await;
}

#[tokio::test]
async fn case_sync_settings_are_typed_validated_and_audited() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("cs").await;
    let path = "/api/v1/projects/cs/settings";
    let (st, d) = s.get(path, ADMIN_TOKEN).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(d["caseSync"]["branches"], json!(["main", "master"]));
    assert_eq!(d["caseSync"]["fields"]["owner"], "code");
    assert_eq!(d["retention"]["manualDays"], 0);
    for bad in [
        json!({"caseSync": {"branches": ["a b"]}}),
        json!({"caseSync": {"branches": ["rel**"]}}),
        json!({"caseSync": {"folderFrom": "tree"}}),
        json!({"caseSync": {"fields": {"owner": "mine"}}}),
        json!({"caseSync": {"bogus": true}}),
        json!({"caseSync": {"staleAfterDays": 0}}),
        json!({"retention": {"manualDays": 99999}}),
    ] {
        let (st, b) = s
            .call(Method::PUT, path, Some(ADMIN_TOKEN), Some(bad.clone()))
            .await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "{bad}: {b}");
    }
    let (st, d) = s
        .call(
            Method::PUT,
            path,
            Some(ADMIN_TOKEN),
            Some(json!({"caseSync": {"branches": ["main", "release/**"], "fields": {"owner": "case"}},
                "retention": {"manualDays": 365}})),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{d}");
    assert_eq!(d["caseSync"]["branches"], json!(["main", "release/**"]));
    assert_eq!(d["caseSync"]["fields"]["owner"], "case");
    assert_eq!(d["caseSync"]["fields"]["name"], "code");
    assert_eq!(d["caseSync"]["rootFolder"], "Automated");
    assert_eq!(d["retention"]["manualDays"], 365);
    let items = audit_actions(&s, &["caseSync.settings"]).await;
    let a = items
        .iter()
        .find(|i| i["action"] == "caseSync.settings")
        .unwrap();
    assert_eq!(
        a["detail"]["caseSync"]["branches"],
        json!(["main", "release/**"])
    );
}

#[tokio::test]
async fn openapi_documents_the_tm_contract() {
    let (_, doc) = fastproof_server::app::api_router();
    let v = serde_json::to_value(&doc).unwrap();
    let schemas = v["components"]["schemas"].as_object().unwrap();
    for name in [
        "Case",
        "CaseSummary",
        "CaseList",
        "CaseWrite",
        "CasePatch",
        "CaseStep",
        "CaseBody",
        "CaseHtml",
        "CaseFolder",
        "CaseFolderWrite",
        "CaseFieldDef",
        "CaseFieldWrite",
        "FieldValue",
        "CaseVersion",
        "CaseSnapshot",
        "CaseHistoryEntry",
        "TestIdentity",
        "CaseBulk",
        "CaseBulkResult",
        "CaseFilter",
        "CaseSuggestion",
        "AttachmentRef",
        "TestPlan",
        "TestPlanWrite",
        "TestPlanItem",
        "TestPlanItemsChange",
        "PlanSyncDiff",
        "StartPlanLaunch",
        "PlanLaunch",
        "LaunchManual",
        "RunItemSummary",
        "RunStepResult",
        "RunItem",
        "RunStepWrite",
        "RunStepAck",
        "RunItemPatch",
        "RunItemsAdd",
        "RunItemsAdded",
        "RunDefect",
        "ManualClose",
        "TmJob",
        "ImportPreview",
        "ImportOptions",
        "TestOpsImport",
        "ImportReport",
        "CaseSyncSettings",
        "RunStatus",
        "CaseStatus",
    ] {
        assert!(schemas.contains_key(name), "{name} missing");
    }
    assert_eq!(
        schemas["RunStatus"]["enum"],
        json!([
            "untested",
            "in_progress",
            "passed",
            "failed",
            "blocked",
            "skipped"
        ])
    );
    assert_eq!(
        schemas["CaseKeyKind"]["enum"],
        json!(["allure_id", "testrail", "qase", "testops", "external"])
    );
    assert_eq!(
        schemas["ImportFormat"]["enum"],
        json!([
            "generic-csv",
            "testrail-csv",
            "qase-csv",
            "qase-json",
            "testops-csv",
            "fastproof-ndjson"
        ])
    );
    assert_eq!(
        serde_json::to_value(fastproof_store::tm::RunStatus::InProgress).unwrap(),
        json!("in_progress")
    );
    assert_eq!(
        serde_json::to_value(fastproof_store::tm::CaseBulkAction::SetStatus).unwrap(),
        json!("setStatus")
    );
    let launch = &schemas["Launch"]["properties"];
    assert!(launch.get("manual").is_some() && launch.get("testPlanId").is_some());
    let result = &schemas["ResultSummary"]["properties"];
    assert!(result.get("source").is_some() && result.get("caseNumber").is_some());
    let settings = &schemas["ProjectSettings"]["properties"];
    assert!(settings.get("caseSync").is_some(), "{settings}");
    let ops: Vec<&str> = v["paths"]
        .as_object()
        .unwrap()
        .values()
        .flat_map(|p| p.as_object().unwrap().values())
        .filter_map(|o| o["operationId"].as_str())
        .collect();
    for op in [
        "list_case_folders",
        "create_case_folder",
        "update_case_folder",
        "delete_case_folder",
        "list_case_fields",
        "create_case_field",
        "update_case_field",
        "delete_case_field",
    ] {
        assert!(ops.contains(&op), "{op} missing");
    }
    let tags: Vec<&str> = v["tags"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|t| t["name"].as_str())
        .collect();
    for t in ["cases", "plans", "runs", "imports"] {
        assert!(tags.contains(&t), "tag {t}");
    }
}
