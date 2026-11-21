//! HTTP API on a real PostgreSQL: auth, projects, launches, streaming results, attachments,
//! limits, live events, readiness. Skipped without `FASTPROOF_TEST_DATABASE_URL`.

mod common;

use std::io::Write;
use std::time::Duration;

use common::{ADMIN_TOKEN, Server, TestDb};
use futures::StreamExt;
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};

#[tokio::test]
async fn auth_is_uniform_and_scoped() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("team-a").await;
    s.project("team-b").await;
    let launch = s.launch("team-a", None).await;
    let lid = launch["id"].as_str().unwrap();

    let (st, body) = s.call(Method::GET, "/api/v1/projects", None, None).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
    assert_eq!(
        body,
        json!({"code": "unauthorized", "message": "authentication required"})
    );
    let (st2, body2) = s
        .call(Method::GET, "/api/v1/projects", Some("fp_wrong"), None)
        .await;
    assert_eq!((st2, body2), (st, body.clone()));
    let r = s
        .req(Method::GET, "/api/v1/projects", None)
        .send()
        .await
        .unwrap();
    assert_eq!(r.headers()["www-authenticate"], "Bearer");
    for path in [
        "/api/openapi.json",
        "/api/v1/me",
        "/api/v1/projects/team-a/events",
        "/api/v1/audit",
        "/api/v1/admin/jobs",
    ] {
        let (st, b) = s.call(Method::GET, path, None, None).await;
        assert_eq!(st, StatusCode::UNAUTHORIZED, "{path}: {b}");
    }
    let (st, _) = s.call(Method::GET, "/healthz", None, None).await;
    assert_eq!(st, StatusCode::OK);
    let r = s.req(Method::GET, "/api/docs/", None).send().await.unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    assert!(
        r.headers()["content-security-policy"]
            .to_str()
            .unwrap()
            .contains("script-src 'self'")
    );
    let r = s.req(Method::GET, "/trace/", None).send().await.unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    assert!(
        r.headers()["content-security-policy"]
            .to_str()
            .unwrap()
            .contains("worker-src 'self'")
    );
    assert_eq!(r.headers()["x-frame-options"], "SAMEORIGIN");
    let r = s
        .req(Method::GET, "/trace/sw.bundle.js", None)
        .send()
        .await
        .unwrap();
    assert!(
        r.headers()["content-type"]
            .to_str()
            .unwrap()
            .starts_with("text/javascript")
    );
    let r = s
        .req(Method::GET, "/launches/x", None)
        .send()
        .await
        .unwrap();
    let csp = r.headers()["content-security-policy"]
        .to_str()
        .unwrap()
        .to_string();
    assert!(
        csp.contains("img-src 'self' data: blob:") && csp.contains("media-src 'self' blob:"),
        "{csp}"
    );
    let (st, doc) = s.get("/api/openapi.json", ADMIN_TOKEN).await;
    assert_eq!(st, StatusCode::OK);
    assert!(doc["paths"]["/api/v1/projects/{key}/launches/{id}/uploads"].is_object());

    let viewer = s.token("viewer-a", "viewer", &["team-a"]).await;
    let uploader = s.token("uploader-a", "uploader", &["team-a"]).await;
    let (st, b) = s.get("/api/v1/projects", &viewer).await;
    assert_eq!(st, StatusCode::OK);
    let keys: Vec<&str> = b["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["key"].as_str().unwrap())
        .collect();
    assert_eq!(keys, vec!["team-a"]);
    let (st, _) = s
        .get(&format!("/api/v1/projects/team-a/launches/{lid}"), &viewer)
        .await;
    assert_eq!(st, StatusCode::OK);

    let (st, b) = s
        .post(
            "/api/v1/projects/team-a/launches",
            &viewer,
            json!({"name": "x"}),
        )
        .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    assert_eq!(b["code"], "forbidden");
    assert_eq!(b["details"]["required"], "launches:write");
    let (st, _) = s
        .upload_as("team-a", lid, &viewer, b"<testsuite/>".to_vec())
        .await;
    assert_eq!(st, StatusCode::FORBIDDEN, "viewer cannot upload");

    let (st, _) = s
        .post(
            "/api/v1/projects/team-a/launches",
            &uploader,
            json!({"name": "ci"}),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED);
    let (st, b) = s
        .call(
            Method::DELETE,
            &format!("/api/v1/projects/team-a/launches/{lid}"),
            Some(&uploader),
            None,
        )
        .await;
    assert_eq!(
        st,
        StatusCode::FORBIDDEN,
        "uploader cannot delete launches: {b}"
    );
    let (st, _) = s
        .call(
            Method::PATCH,
            "/api/v1/projects/team-a",
            Some(&uploader),
            Some(json!({"name": "n"})),
        )
        .await;
    assert_eq!(st, StatusCode::FORBIDDEN, "uploader cannot change settings");
    let (st, b) = s.get("/api/v1/admin/jobs", &uploader).await;
    assert_eq!(st, StatusCode::FORBIDDEN, "{b}");

    let (st_b, body_b) = s.get("/api/v1/projects/team-b", &viewer).await;
    let (st_c, body_c) = s.get("/api/v1/projects/team-c", &viewer).await;
    assert_eq!(st_b, StatusCode::FORBIDDEN);
    assert_eq!(st_c, StatusCode::FORBIDDEN);
    assert_eq!(body_b["code"], body_c["code"]);
    assert_eq!(
        body_b.to_string().replace("team-b", "KEY"),
        body_c.to_string().replace("team-c", "KEY"),
        "an existing and a missing project look the same"
    );
    let (st, _) = s
        .get(&format!("/api/v1/projects/team-b/launches/{lid}"), &viewer)
        .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (st, _) = s
        .get(
            &format!("/api/v1/projects/team-a/launches/{}", uuid::Uuid::now_v7()),
            &viewer,
        )
        .await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    let (st, _) = s
        .get("/api/v1/projects/team-a/launches/not-a-uuid", &viewer)
        .await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    let (st, b) = s.get("/api/v1/admin/jobs", ADMIN_TOKEN).await;
    assert_eq!(st, StatusCode::OK);
    let names: Vec<&str> = b["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|j| j["name"].as_str().unwrap())
        .collect();
    let registered = fastproof_server::scheduler::standard_jobs()
        .unwrap()
        .names();
    assert!(registered.len() >= 4, "{registered:?}");
    for j in &registered {
        assert!(
            names.contains(&j.as_str()),
            "{j} is listed as soon as the server is up: {names:?}"
        );
    }
}

#[tokio::test]
async fn projects_and_launches() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    let (st, b) = s
        .post(
            "/api/v1/projects",
            ADMIN_TOKEN,
            json!({"key": "Bad_Key", "name": "x"}),
        )
        .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{b}");
    s.project("shop").await;
    let (st, b) = s
        .post(
            "/api/v1/projects",
            ADMIN_TOKEN,
            json!({"key": "shop", "name": "again"}),
        )
        .await;
    assert_eq!(st, StatusCode::CONFLICT, "{b}");
    let (st, b) = s
        .call(
            Method::PATCH,
            "/api/v1/projects/shop",
            Some(ADMIN_TOKEN),
            Some(json!({"description": "Web shop"})),
        )
        .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(b["description"], "Web shop");

    let body = json!({"externalId": "pipeline-7", "name": "main #7",
        "ci": {"provider": "gitlab", "pipelineId": "7"},
        "vcs": {"branch": "main", "repository": "https://gitlab.example.com/web/shop.git", "commit": "0123abcd"},
        "environment": {"key": "staging", "values": {"os": "linux"}},
        "attributes": [{"key": "team", "value": "web"}]});
    let (st, l1) = s
        .post("/api/v1/projects/shop/launches", ADMIN_TOKEN, body.clone())
        .await;
    assert_eq!(st, StatusCode::CREATED);
    let (st, l2) = s
        .post("/api/v1/projects/shop/launches", ADMIN_TOKEN, body)
        .await;
    assert_eq!(st, StatusCode::OK, "same externalId returns the launch");
    assert_eq!(l1["id"], l2["id"]);
    assert_eq!(l1["number"], 1);
    assert_eq!(l1["status"], "running");
    assert_eq!(
        l1["vcs"]["commitUrl"],
        "https://gitlab.example.com/web/shop/-/commit/0123abcd"
    );
    let (_, dbg) = s
        .post(
            "/api/v1/projects/shop/launches",
            ADMIN_TOKEN,
            json!({"name": "local", "debug": true}),
        )
        .await;
    let (_, only_debug) = s
        .get("/api/v1/projects/shop/launches?debug=true", ADMIN_TOKEN)
        .await;
    assert_eq!(only_debug["items"].as_array().unwrap().len(), 1);
    assert_eq!(only_debug["items"][0]["id"], dbg["id"]);
    let l3 = s.launch("shop", None).await;
    assert_eq!(l3["number"], 3);
    let (_, p) = s.get("/api/v1/projects/shop", ADMIN_TOKEN).await;
    assert_eq!(p["lastLaunch"]["id"], l3["id"], "{p}");
    assert_eq!(p["lastLaunch"]["counts"]["total"], 0);
    let (st, list) = s
        .get("/api/v1/projects/shop/launches?branch=main", ADMIN_TOKEN)
        .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(list["items"].as_array().unwrap().len(), 1);
    let (_, list) = s
        .get(
            "/api/v1/projects/shop/launches?attr=team:web&environment=staging",
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(list["items"][0]["id"], l1["id"]);
    let (_, page) = s
        .get("/api/v1/projects/shop/launches?limit=1", ADMIN_TOKEN)
        .await;
    assert_eq!(page["items"][0]["number"], 3);
    let next = page["next"].as_str().unwrap();
    let (_, page2) = s
        .get(
            &format!("/api/v1/projects/shop/launches?limit=1&before={next}"),
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(
        page2["items"][0]["number"], 1,
        "debug launch 2 is not listed"
    );
    assert!(page2["next"].is_null());
    let (st, _) = s
        .get("/api/v1/projects/shop/launches?limit=501", ADMIN_TOKEN)
        .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);

    let id = l1["id"].as_str().unwrap();
    let (st, f) = s
        .call(
            Method::POST,
            &format!("/api/v1/projects/shop/launches/{id}/finish"),
            Some(ADMIN_TOKEN),
            None,
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{f}");
    assert_eq!(f["status"], "finished");
    assert!(f["finishedAt"].is_string());
    let (st, _) = s
        .post(
            &format!("/api/v1/projects/shop/launches/{id}/finish"),
            ADMIN_TOKEN,
            json!({"status": "weird"}),
        )
        .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    let (st, _) = s
        .call(
            Method::DELETE,
            &format!("/api/v1/projects/shop/launches/{id}"),
            Some(ADMIN_TOKEN),
            None,
        )
        .await;
    assert_eq!(st, StatusCode::NO_CONTENT);
    let (st, _) = s
        .get(&format!("/api/v1/projects/shop/launches/{id}"), ADMIN_TOKEN)
        .await;
    assert_eq!(st, StatusCode::NOT_FOUND);

    let (st, _) = s
        .call(
            Method::DELETE,
            "/api/v1/projects/shop",
            Some(ADMIN_TOKEN),
            None,
        )
        .await;
    assert_eq!(st, StatusCode::NO_CONTENT);
    let (st, _) = s.get("/api/v1/projects/shop", ADMIN_TOKEN).await;
    assert_eq!(st, StatusCode::NOT_FOUND);

    let (st, audit) = s.get("/api/v1/audit?limit=100", ADMIN_TOKEN).await;
    assert_eq!(st, StatusCode::OK);
    let actions: Vec<&str> = audit["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["action"].as_str().unwrap())
        .collect();
    for a in [
        "project.create",
        "project.update",
        "launch.finish",
        "launch.delete",
        "project.delete",
    ] {
        assert!(actions.contains(&a), "{a} not audited: {actions:?}");
    }
    let (st, _) = s.get("/api/v1/audit?action=project.*", ADMIN_TOKEN).await;
    assert_eq!(st, StatusCode::OK);
}

fn result_json(id: &str, name: &str, status: &str, attachment: Option<&str>) -> Value {
    let mut r = json!({"id": id, "name": name, "fullName": format!("api.Suite#{name}"), "status": status,
        "start": 1795249075967i64, "stop": 1795249076967i64,
        "labels": [{"name": "suite", "value": "api"}],
        "parameters": [{"name": "user", "value": "alice"}, {"name": "password", "value": "hunter2", "mode": "masked"}],
        "steps": [{"name": "call", "status": status, "steps": [{"name": "inner", "status": "passed"}]}]});
    if let Some(a) = attachment {
        r["attachments"] = json!([{"id": a, "name": "response.json"}]);
    }
    r
}

#[tokio::test]
async fn streaming_results_and_attachments() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("api").await;
    let launch = s.launch("api", None).await;
    let lid = launch["id"].as_str().unwrap();
    let base = format!("/api/v1/projects/api/launches/{lid}");

    let att = |bytes: &'static [u8], ct: &'static str, name: &'static str| {
        s.req(
            Method::POST,
            &format!("{base}/attachments"),
            Some(ADMIN_TOKEN),
        )
        .header("content-type", ct)
        .header("x-fastproof-attachment-name", name)
        .body(bytes)
        .send()
    };
    let r = att(b"{\"ok\":true}", "application/json", "response.json")
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::CREATED);
    let a1: Value = r.json().await.unwrap();
    let r = att(b"{\"ok\":true}", "application/json", "response.json")
        .await
        .unwrap();
    let a2: Value = r.json().await.unwrap();
    assert_eq!(a1["id"], a2["id"], "same bytes, same attachment");
    let r = att(b"{\"ok\":true}", "application/json", "again.json")
        .await
        .unwrap();
    let a3: Value = r.json().await.unwrap();
    assert_ne!(a1["id"], a3["id"], "another name is a reference of its own");
    assert_eq!(a1["sha256"], a3["sha256"], "to the same bytes");
    assert_eq!(a1["size"], 11);
    let aid = a1["id"].as_str().unwrap();

    let body = json!([
        result_json("t1", "first", "passed", Some(aid)),
        result_json("t2", "second", "failed", None),
        {"id": "t3", "status": "passed"},
        result_json("t4", "third", "passed", Some(&uuid::Uuid::now_v7().to_string())),
    ]);
    let (st, out) = s
        .post(&format!("{base}/results"), ADMIN_TOKEN, body.clone())
        .await;
    assert_eq!(st, StatusCode::ACCEPTED, "{out}");
    assert_eq!(out["accepted"], 2);
    assert_eq!(out["duplicates"], 0);
    let rejected: Vec<u64> = out["rejected"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["index"].as_u64().unwrap())
        .collect();
    assert_eq!(rejected, vec![2, 3]);
    let (_, again) = s.post(&format!("{base}/results"), ADMIN_TOKEN, body).await;
    assert_eq!(again["accepted"], 0);
    assert_eq!(again["duplicates"], 2);

    let mut nd = String::new();
    nd.push_str(&result_json("t2", "second", "failed", None).to_string());
    nd.push('\n');
    nd.push_str(&json!({"id": "t2-retry", "name": "second", "fullName": "api.Suite#second", "status": "passed",
        "start": 1795249079967i64, "parameters": [{"name": "user", "value": "alice"}, {"name": "password", "value": "hunter2", "mode": "masked"}]}).to_string());
    nd.push_str("\n\nnot json\n");
    nd.push_str(&result_json("t5", "fifth", "broken", None).to_string());
    let r = s
        .req(Method::POST, &format!("{base}/results"), Some(ADMIN_TOKEN))
        .header("content-type", "application/x-ndjson")
        .body(nd)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::ACCEPTED);
    let out: Value = r.json().await.unwrap();
    assert_eq!(out["accepted"], 2, "{out}");
    assert_eq!(out["duplicates"], 1);
    assert_eq!(out["rejected"][0]["index"], 3);

    let r = s
        .req(Method::POST, &format!("{base}/results"), Some(ADMIN_TOKEN))
        .header("content-type", "text/plain")
        .body("x")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);

    let l = s.launch_get("api", lid).await;
    assert_eq!(l["counts"]["total"], 3, "{l}");
    assert_eq!(l["counts"]["flaky"], 1);
    let results = s.results("api", lid, "").await;
    let second = results.iter().find(|r| r["name"] == "second").unwrap();
    assert_eq!(
        second["attemptCount"], 2,
        "the same series in two requests becomes two attempts"
    );
    assert_eq!(second["status"], "passed");
    let failed = s.results("api", lid, "&status=failed,broken").await;
    assert_eq!(failed.len(), 1);
    let q = s.results("api", lid, "&q=fift").await;
    assert_eq!(q.len(), 1);
    let by_label = s.results("api", lid, "&label=suite:api").await;
    assert_eq!(
        by_label.len(),
        2,
        "the latest attempt of `second` has no labels"
    );

    let first = results.iter().find(|r| r["name"] == "first").unwrap();
    let (st, d) = s
        .get(
            &format!(
                "/api/v1/projects/api/results/{}",
                first["id"].as_str().unwrap()
            ),
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(d["attempts"].as_array().unwrap().len(), 1);
    assert_eq!(d["attempts"][0]["steps"][0]["steps"][0]["name"], "inner");
    assert_eq!(d["attempts"][0]["attachments"][0]["id"], aid);
    assert_eq!(d["attempts"][0]["attachments"][0]["kind"], "json");
    let params = d["parameters"].as_array().unwrap();
    assert_eq!(
        params.len(),
        1,
        "masked parameters are never returned: {params:?}"
    );
    assert!(!d.to_string().contains("hunter2"));
    assert_eq!(d["history"].as_array().unwrap().len(), 1);
    assert_eq!(d["history"][0]["resultId"], first["id"]);
    assert_eq!(d["launchId"], lid);
    assert_eq!(d["launchNumber"], 1);
    let tc = first["testCaseId"].as_str().unwrap();
    let (st, h) = s
        .get(
            &format!("/api/v1/projects/api/test-cases/{tc}/history"),
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(h["items"][0]["launchId"], lid);
    assert_eq!(h["testCase"]["fullName"], "api.Suite#first");
    let (st, tree) = s
        .get(&format!("{base}/tree?groupBy=suite"), ADMIN_TOKEN)
        .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(tree["counts"]["total"], 3);
    let groups: Vec<(&str, usize)> = tree["children"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| {
            (
                c["name"].as_str().unwrap(),
                c["resultIds"].as_array().unwrap().len(),
            )
        })
        .collect();
    assert_eq!(groups, vec![("api", 2), ("api.Suite", 1)]);

    let r = s
        .req(
            Method::GET,
            &format!("/api/v1/projects/api/attachments/{aid}/content"),
            Some(ADMIN_TOKEN),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let h = r.headers();
    assert_eq!(h["x-content-type-options"], "nosniff");
    assert_eq!(h["content-security-policy"], "sandbox; default-src 'none'");
    assert_eq!(h["cache-control"], "private, max-age=31536000, immutable");
    assert!(
        h["content-disposition"]
            .to_str()
            .unwrap()
            .starts_with("inline")
    );
    assert_eq!(r.bytes().await.unwrap().as_ref(), b"{\"ok\":true}");
    let r = att(
        b"<html><script>alert(1)</script></html>",
        "text/html",
        "page.html",
    )
    .await
    .unwrap();
    let html: Value = r.json().await.unwrap();
    let r = s
        .req(
            Method::GET,
            &format!(
                "/api/v1/projects/api/attachments/{}/content",
                html["id"].as_str().unwrap()
            ),
            Some(ADMIN_TOKEN),
        )
        .send()
        .await
        .unwrap();
    assert!(
        r.headers()["content-disposition"]
            .to_str()
            .unwrap()
            .starts_with("attachment")
    );
    assert_eq!(
        r.headers()["content-security-policy"],
        "sandbox; default-src 'none'"
    );
    let r = s
        .req(
            Method::GET,
            &format!("/api/v1/projects/api/attachments/{aid}/content?download=1"),
            Some(ADMIN_TOKEN),
        )
        .send()
        .await
        .unwrap();
    assert!(
        r.headers()["content-disposition"]
            .to_str()
            .unwrap()
            .starts_with("attachment")
    );

    s.project("other").await;
    let (st, _) = s
        .get(
            &format!("/api/v1/projects/other/attachments/{aid}/content"),
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(st, StatusCode::NOT_FOUND, "attachments are per project");
    let other = s.token("viewer-other", "viewer", &["other"]).await;
    let (st, _) = s
        .get(
            &format!("/api/v1/projects/api/attachments/{aid}/content"),
            &other,
        )
        .await;
    assert_eq!(st, StatusCode::FORBIDDEN);

    let events = fastproof_store::Store::new(db.pool.clone())
        .outbox_after(0, 10_000)
        .await
        .unwrap();
    assert!(events.iter().filter(|e| e.kind == "result.updated").count() >= 4);
}

#[tokio::test]
async fn limits_name_the_limit() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start_cfg(&db, &[("FASTPROOF_MAX_UPLOAD_BYTES", "200000")], |c| {
        c.limits.max_total_uncompressed_bytes = 4 * 1024 * 1024;
    })
    .await;
    s.project("lim").await;
    let launch = s.launch("lim", None).await;
    let lid = launch["id"].as_str().unwrap();

    let junit = common::read("gotestsum/junit.xml");
    let mut body = Vec::new();
    body.extend_from_slice(
        b"--XBOUNDARY\r\nContent-Disposition: form-data; name=\"note\"\r\n\r\nignored\r\n",
    );
    body.extend_from_slice(b"--XBOUNDARY\r\nContent-Disposition: form-data; name=\"file\"; filename=\"../../junit.xml\"\r\nContent-Type: application/xml\r\n\r\n");
    body.extend_from_slice(&junit);
    body.extend_from_slice(b"\r\n--XBOUNDARY--\r\n");
    let r = s
        .req(
            Method::POST,
            &format!("/api/v1/projects/lim/launches/{lid}/uploads"),
            Some(ADMIN_TOKEN),
        )
        .header("content-type", "multipart/form-data; boundary=XBOUNDARY")
        .body(body)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::ACCEPTED);
    let u: Value = r.json().await.unwrap();
    assert_eq!(u["sizeBytes"], junit.len());
    let done = s.wait_upload("lim", lid, u["id"].as_str().unwrap()).await;
    assert_eq!(done["status"], "done", "{done}");
    assert!(done["counts"]["results"].as_u64().unwrap() > 0);

    let (st, b) = s.upload("lim", lid, vec![b'x'; 300_000], "").await;
    assert_eq!(st, StatusCode::PAYLOAD_TOO_LARGE, "{b}");
    assert_eq!(b["code"], "payload_too_large");
    assert_eq!(b["details"]["limit"], "max_upload_bytes");

    let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let opts = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    zip.start_file("bomb-attachment.txt", opts).unwrap();
    let zeros = vec![0u8; 1024 * 1024];
    for _ in 0..16 {
        zip.write_all(&zeros).unwrap();
    }
    zip.start_file("r-result.json", opts).unwrap();
    zip.write_all(br#"{"uuid":"r","name":"t","fullName":"a.T#t","status":"passed","attachments":[{"name":"b","source":"bomb-attachment.txt","type":"text/plain"}]}"#)
        .unwrap();
    let bytes = zip.finish().unwrap().into_inner();
    assert!(bytes.len() < 200_000, "{}", bytes.len());
    let u = s.ingest("lim", lid, bytes, "").await;
    assert_eq!(u["status"], "failed", "{u}");
    assert_eq!(u["error"]["code"], "limit_exceeded");
    assert_eq!(u["error"]["limit"], "max_total_uncompressed_bytes");

    let (st, b) = s
        .post(
            &format!("/api/v1/projects/lim/launches/{lid}/results"),
            ADMIN_TOKEN,
            json!({"id": "x"}),
        )
        .await;
    assert_eq!(st, StatusCode::ACCEPTED);
    assert_eq!(b["rejected"].as_array().unwrap().len(), 1);
    let r = s
        .req(
            Method::POST,
            &format!("/api/v1/projects/lim/launches/{lid}/results"),
            Some(ADMIN_TOKEN),
        )
        .header("content-type", "application/x-ndjson")
        .body(vec![b' '; 300_000])
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::PAYLOAD_TOO_LARGE);
    let b: Value = r.json().await.unwrap();
    assert_eq!(b["details"]["limit"], "max_upload_bytes");
}

#[tokio::test]
async fn events_after_upload_and_readiness() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("live").await;
    let launch = s.launch("live", None).await;
    let lid = launch["id"].as_str().unwrap().to_string();
    let ready = reqwest::get(format!("{}/readyz", s.ops)).await.unwrap();
    assert_eq!(ready.status(), StatusCode::OK);

    let r = s
        .req(
            Method::GET,
            "/api/v1/projects/live/events",
            Some(ADMIN_TOKEN),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    assert!(
        r.headers()["content-type"]
            .to_str()
            .unwrap()
            .starts_with("text/event-stream")
    );
    let mut stream = r.bytes_stream();
    let mut seen = String::new();
    read_until(&mut stream, &mut seen, "event: ready").await;
    let u = s
        .ingest(
            "live",
            &lid,
            common::read("gotestsum/junit.xml"),
            "?fileName=junit.xml",
        )
        .await;
    assert_eq!(u["status"], "done", "{u}");
    read_until(&mut stream, &mut seen, "event: upload.updated").await;
    read_until(&mut stream, &mut seen, "event: launch.updated").await;
    assert!(seen.contains(&lid));

    let m = reqwest::get(format!("{}/metrics", s.ops))
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(m.contains("fastproof_ingest_jobs_total"), "{m}");
    assert!(m.contains("fastproof_results_ingested_total"));

    db.drop_now().await;
    let ready = reqwest::get(format!("{}/readyz", s.ops)).await.unwrap();
    assert_eq!(ready.status(), StatusCode::SERVICE_UNAVAILABLE);
    let b: Value = ready.json().await.unwrap();
    assert_eq!(b["checks"]["database"], false);
    let health = reqwest::get(format!("{}/healthz", s.ops)).await.unwrap();
    assert_eq!(health.status(), StatusCode::OK);
}

async fn read_until<S, B>(stream: &mut S, seen: &mut String, needle: &str)
where
    S: futures::Stream<Item = reqwest::Result<B>> + Unpin,
    B: AsRef<[u8]>,
{
    tokio::time::timeout(Duration::from_secs(30), async {
        while !seen.contains(needle) {
            let chunk = stream.next().await.unwrap().unwrap();
            seen.push_str(&String::from_utf8_lossy(chunk.as_ref()));
        }
    })
    .await
    .unwrap_or_else(|_| panic!("no {needle} event in {seen}"));
}

impl Server {
    async fn upload_as(
        &self,
        project: &str,
        launch: &str,
        token: &str,
        bytes: Vec<u8>,
    ) -> (StatusCode, Value) {
        let r = self
            .req(
                Method::POST,
                &format!("/api/v1/projects/{project}/launches/{launch}/uploads"),
                Some(token),
            )
            .body(bytes)
            .send()
            .await
            .unwrap();
        let s = r.status();
        (s, r.json().await.unwrap_or(Value::Null))
    }
}

#[tokio::test]
async fn token_sign_in_without_sso() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    let r = s
        .req(Method::GET, "/auth/config", None)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let b: Value = r.json().await.unwrap();
    assert_eq!(b, json!({"oidc": false}), "public, and says SSO is off");

    let plain = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    for (ret, want) in [
        (
            "%2Fp%2Fshop%3Fbranch%3Dmain",
            "/sign-in?return=%2Fp%2Fshop%3Fbranch%3Dmain",
        ),
        ("%2F%2Fevil.example.com", "/sign-in"),
        ("https%3A%2F%2Fevil.example.com%2F", "/sign-in"),
    ] {
        let r = plain
            .get(format!("{}/auth/login?return={ret}", s.base))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::SEE_OTHER, "{ret}");
        assert_eq!(r.headers()["location"], want, "{ret}");
    }
    let r = plain
        .get(format!("{}/auth/login", s.base))
        .send()
        .await
        .unwrap();
    assert_eq!(r.headers()["location"], "/sign-in");
    let r = s
        .req(Method::POST, "/auth/logout", None)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::NOT_FOUND);

    let (st, me) = s.get("/api/v1/me", ADMIN_TOKEN).await;
    assert_eq!(st, StatusCode::OK, "{me}");
    let (st, _) = s
        .call(
            Method::POST,
            "/api/v1/projects",
            Some(ADMIN_TOKEN),
            Some(json!({"key": "tok", "name": "Token"})),
        )
        .await;
    assert_eq!(
        st,
        StatusCode::CREATED,
        "bearer requests need no CSRF header"
    );
}
