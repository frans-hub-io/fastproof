//! Imports and exports of test cases over HTTP: preview, dry run → apply → repeated upsert
//! without duplicates, TestOps numbering and code identities, attachments from a zip, export
//! round trip, CSV formula guard, the one-time zip link, the Allure TestOps API client against
//! a local stub (fallback path, redirects, oversized bodies, the token never shown), the
//! permission matrix, `import_busy` and IDOR.

mod common;

use std::io::Write;
use std::time::{Duration, Instant};

use common::{ADMIN_TOKEN, Server, TestDb};
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};

const KEY_HEX: &str = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";

fn fixture(name: &str) -> Vec<u8> {
    let p = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../fastproof-tm-import/tests/fixtures")
        .join(name);
    std::fs::read(p).unwrap()
}

/// A multipart body of `(field, file name, bytes)` parts.
fn multipart(parts: &[(&str, Option<&str>, &[u8])]) -> (String, Vec<u8>) {
    let boundary = "fastproofboundary7MA4YWxkTrZu0gW";
    let mut body = Vec::new();
    for (name, file, bytes) in parts {
        write!(
            body,
            "--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\""
        )
        .unwrap();
        if let Some(f) = file {
            write!(body, "; filename=\"{f}\"").unwrap();
        }
        body.extend_from_slice(b"\r\n\r\n");
        body.extend_from_slice(bytes);
        body.extend_from_slice(b"\r\n");
    }
    write!(body, "--{boundary}--\r\n").unwrap();
    (format!("multipart/form-data; boundary={boundary}"), body)
}

async fn post_multipart(
    s: &Server,
    path: &str,
    token: &str,
    parts: &[(&str, Option<&str>, &[u8])],
) -> (StatusCode, Value) {
    let (ct, body) = multipart(parts);
    let r = s
        .req(Method::POST, path, Some(token))
        .header("content-type", ct)
        .body(body)
        .send()
        .await
        .unwrap();
    let st = r.status();
    let t = r.text().await.unwrap();
    (st, serde_json::from_str(&t).unwrap_or(Value::String(t)))
}

async fn start_import(
    s: &Server,
    key: &str,
    token: &str,
    file: &[u8],
    name: &str,
    options: Value,
    zip: Option<&[u8]>,
) -> (StatusCode, Value) {
    let o = options.to_string();
    let mut parts: Vec<(&str, Option<&str>, &[u8])> =
        vec![("options", None, o.as_bytes()), ("file", Some(name), file)];
    if let Some(z) = zip {
        parts.push(("attachments", Some("files.zip"), z));
    }
    post_multipart(s, &format!("/api/v1/projects/{key}/imports"), token, &parts).await
}

async fn wait_job(s: &Server, key: &str, id: &str, want: &[&str]) -> Value {
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        let (st, b) = s
            .get(&format!("/api/v1/projects/{key}/tm-jobs/{id}"), ADMIN_TOKEN)
            .await;
        assert_eq!(st, StatusCode::OK, "{b}");
        let status = b["status"].as_str().unwrap_or("").to_string();
        if want.contains(&status.as_str()) {
            return b;
        }
        assert!(
            !["failed", "cancelled", "done"].contains(&status.as_str())
                || want.contains(&status.as_str()),
            "job ended {status}: {b}"
        );
        assert!(Instant::now() < deadline, "job {id} stuck: {b}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Dry run, then apply; returns `(dry report, applied report)`.
async fn import(s: &Server, key: &str, file: &[u8], name: &str, options: Value) -> (Value, Value) {
    let (st, j) = start_import(s, key, ADMIN_TOKEN, file, name, options, None).await;
    assert_eq!(st, StatusCode::ACCEPTED, "{j}");
    let id = j["id"].as_str().unwrap().to_string();
    let ready = wait_job(s, key, &id, &["ready"]).await;
    let (st, b) = s
        .post(
            &format!("/api/v1/projects/{key}/imports/{id}/apply"),
            ADMIN_TOKEN,
            json!({}),
        )
        .await;
    assert_eq!(st, StatusCode::ACCEPTED, "{b}");
    let done = wait_job(s, key, &id, &["done"]).await;
    (ready["report"].clone(), done["report"].clone())
}

async fn cases(s: &Server, key: &str) -> Vec<Value> {
    let mut out = Vec::new();
    let mut before: Option<String> = None;
    loop {
        let q = before
            .as_ref()
            .map(|b| format!("&before={b}"))
            .unwrap_or_default();
        let (st, b) = s
            .get(
                &format!("/api/v1/projects/{key}/cases?limit=500{q}"),
                ADMIN_TOKEN,
            )
            .await;
        assert_eq!(st, StatusCode::OK, "{b}");
        out.extend(b["items"].as_array().unwrap().iter().cloned());
        match b["next"].as_str() {
            Some(n) => before = Some(n.to_string()),
            None => return out,
        }
    }
}

async fn case(s: &Server, key: &str, n: i64) -> Value {
    let (st, b) = s
        .get(&format!("/api/v1/projects/{key}/cases/{n}"), ADMIN_TOKEN)
        .await;
    assert_eq!(st, StatusCode::OK, "{b}");
    b
}

fn testrail_options(mode: &str) -> Value {
    json!({"format": "testrail-csv", "mode": mode, "unknownFieldValues": "create"})
}

#[tokio::test]
async fn testrail_dry_run_apply_and_repeated_upsert() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("imp").await;
    let file = fixture("testrail-rows.csv");

    let (st, p) = post_multipart(
        &s,
        "/api/v1/projects/imp/imports/preview",
        ADMIN_TOKEN,
        &[("file", Some("x.csv"), &file)],
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{p}");
    assert_eq!(p["format"], "testrail-csv");
    assert_eq!(p["stepsLayout"], "rows");
    assert_eq!(p["rows"].as_array().unwrap().len(), 5);

    let (dry, applied) = import(
        &s,
        "imp",
        &file,
        "cases.csv",
        testrail_options("createOnly"),
    )
    .await;
    assert_eq!(dry["create"], 3, "{dry}");
    assert_eq!(
        dry["folders"], 4,
        "Accounts, Login, Passwords, Reset: {dry}"
    );
    assert!(
        dry["unknownFields"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f == "Browser"),
        "{dry}"
    );
    assert!(
        dry["unknownValues"]["type"].is_null(),
        "type is not a field: {dry}"
    );
    assert_eq!(applied["create"], 3, "{applied}");
    assert_eq!(applied["folders"], 4);
    let list = cases(&s, "imp").await;
    assert_eq!(list.len(), 3);
    let first = case(&s, "imp", list[0]["number"].as_i64().unwrap()).await;
    assert_eq!(first["name"], "Login with a valid password");
    assert_eq!(first["origin"], "import");
    assert_eq!(
        first["fields"]["priority"], "high",
        "High matches the option high"
    );
    assert_eq!(first["body"]["steps"].as_array().unwrap().len(), 3);
    assert_eq!(first["folder"]["path"][0]["name"], "Accounts");
    assert!(
        first["keys"]
            .as_array()
            .unwrap()
            .iter()
            .any(|k| k["kind"] == "testrail" && k["value"] == "C3863")
    );
    let (_, v) = s
        .get(
            &format!("/api/v1/projects/imp/cases/{}/versions", first["number"]),
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(v["items"][0]["source"], "import");

    let (dry, applied) = import(&s, "imp", &file, "cases.csv", testrail_options("upsert")).await;
    assert_eq!(
        (
            dry["create"].as_i64(),
            dry["update"].as_i64(),
            dry["skip"].as_i64()
        ),
        (Some(0), Some(0), Some(3)),
        "{dry}"
    );
    assert_eq!(
        (applied["create"].as_i64(), applied["update"].as_i64()),
        (Some(0), Some(0)),
        "{applied}"
    );
    assert_eq!(cases(&s, "imp").await.len(), 3, "no duplicates");
    let (_, v) = s
        .get(
            &format!("/api/v1/projects/imp/cases/{}/versions", first["number"]),
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(
        v["items"].as_array().unwrap().len(),
        1,
        "an unchanged upsert writes no version"
    );

    let changed = String::from_utf8(file.clone())
        .unwrap()
        .replace("Logout,Login", "Log out now,Login");
    let (dry, applied) = import(
        &s,
        "imp",
        changed.as_bytes(),
        "cases.csv",
        testrail_options("upsert"),
    )
    .await;
    assert_eq!(dry["update"], 1, "{dry}");
    assert_eq!(applied["update"], 1, "{applied}");
    let renamed = cases(&s, "imp")
        .await
        .into_iter()
        .find(|c| c["name"] == "Log out now")
        .unwrap();
    let (_, v) = s
        .get(
            &format!("/api/v1/projects/imp/cases/{}/versions", renamed["number"]),
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(v["items"][0]["source"], "import");
    assert_eq!(v["items"][0]["version"], 2);

    let (dry, _) = import(
        &s,
        "imp",
        &file,
        "cases.csv",
        testrail_options("createOnly"),
    )
    .await;
    assert_eq!(dry["skip"], 3, "{dry}");

    let (st, j) = start_import(
        &s,
        "imp",
        ADMIN_TOKEN,
        &fixture("qase-v2.csv"),
        "q.csv",
        json!({"format": "qase-csv", "mode": "createOnly", "unknownFieldValues": "fail"}),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::ACCEPTED);
    let r = wait_job(&s, "imp", j["id"].as_str().unwrap(), &["ready"]).await;
    assert_eq!(r["report"]["create"], 2, "{r}");
    let (st, _) = s
        .call(
            Method::DELETE,
            &format!("/api/v1/projects/imp/imports/{}", j["id"].as_str().unwrap()),
            Some(ADMIN_TOKEN),
            None,
        )
        .await;
    assert_eq!(st, StatusCode::OK);
    let (st, b) = s
        .post(
            &format!(
                "/api/v1/projects/imp/imports/{}/apply",
                j["id"].as_str().unwrap()
            ),
            ADMIN_TOKEN,
            json!({}),
        )
        .await;
    assert_eq!(st, StatusCode::CONFLICT, "{b}");
    assert_eq!(b["code"], "job_not_ready");

    let (st, j) = start_import(
        &s,
        "imp",
        ADMIN_TOKEN,
        b"name\n\"broken",
        "b.csv",
        json!({"format": "generic-csv", "mode": "createOnly", "unknownFieldValues": "skip"}),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::ACCEPTED);
    let r = wait_job(&s, "imp", j["id"].as_str().unwrap(), &["ready"]).await;
    assert_eq!(r["report"]["errors"][0]["code"], "invalid_row", "{r}");
    assert_eq!(r["report"]["errors"][0]["row"], 2);

    let (_, a) = s.get("/api/v1/audit?limit=500", ADMIN_TOKEN).await;
    for want in ["import.start", "import.apply", "import.cancel"] {
        assert!(
            a["items"]
                .as_array()
                .unwrap()
                .iter()
                .any(|i| i["action"] == want),
            "audit lacks {want}"
        );
    }
}

#[tokio::test]
async fn testops_csv_numbers_identities_and_select_options() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("top").await;
    let (dry, applied) = import(
        &s,
        "top",
        &fixture("testops-tom.csv"),
        "t.csv",
        json!({"format": "testops-csv", "mode": "upsert", "unknownFieldValues": "create"}),
    )
    .await;
    assert_eq!(dry["create"], 3, "{dry}");
    assert_eq!(applied["create"], 3, "{applied}");
    let c = case(&s, "top", 1124).await;
    assert_eq!(c["name"], "API returns 200");
    assert_eq!(c["automated"], true);
    let code: Vec<&Value> = c["identities"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| i["kind"] == "code")
        .collect();
    assert_eq!(code.len(), 1, "{c}");
    assert_eq!(code[0]["fullName"], "com.example.HealthTest.ok");
    assert!(
        c["keys"]
            .as_array()
            .unwrap()
            .iter()
            .any(|k| k["kind"] == "testops" && k["value"] == "1124")
    );
    let p = case(&s, "top", 1123).await;
    assert_eq!(
        p["body"]["steps"][0]["steps"][0]["steps"][0]["action"],
        "Wait for load"
    );
    assert_eq!(p["fields"]["feature"], "Profile");
    let (st, n) = s
        .post(
            "/api/v1/projects/top/cases",
            ADMIN_TOKEN,
            json!({"name": "after import", "body": {}}),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED, "{n}");
    assert_eq!(n["number"], 1126, "case_seq = max + 1");

    let full = "com.example.HealthTest.ok";
    let hash = fastproof_model::identity::md5_hex(full);
    let n: i64 = sqlx::query_scalar("select count(*) from test_cases where test_case_hash = $1 and kind = 'code' and case_id is not null")
        .bind(&hash)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(
        n, 1,
        "the first upload of this test links to the imported case"
    );

    s.project("top-two").await;
    let (_, b) = s
        .post(
            "/api/v1/projects/top-two/cases",
            ADMIN_TOKEN,
            json!({"name": "existing", "body": {}}),
        )
        .await;
    assert_eq!(b["number"], 1);
    let csv = "ID,Title,Section,Priority\nC1,Select me,S,Urgent\nC2,Other,S,High\n";
    let (dry, applied) = import(
        &s,
        "top-two",
        csv.as_bytes(),
        "x.csv",
        json!({"format": "testrail-csv", "mode": "createOnly",
        "unknownFieldValues": "create"}),
    )
    .await;
    assert_eq!(dry["unknownValues"]["priority"], json!(["Urgent"]), "{dry}");
    assert_eq!(applied["create"], 2);
    let (_, f) = s
        .get("/api/v1/projects/top-two/case-fields", ADMIN_TOKEN)
        .await;
    let prio = f["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["key"] == "priority")
        .unwrap()
        .clone();
    assert!(
        prio["options"]
            .as_array()
            .unwrap()
            .iter()
            .any(|o| o["value"] == "Urgent"),
        "{prio}"
    );
    let numbers: Vec<i64> = cases(&s, "top-two")
        .await
        .iter()
        .map(|c| c["number"].as_i64().unwrap())
        .collect();
    assert_eq!(
        numbers,
        vec![1, 2, 3],
        "a project with cases gets new numbers"
    );
    let (dry, _) = import(
        &s,
        "top-two",
        "ID,Title,Section,Priority\nC9,Fail me,S,Nope\n".as_bytes(),
        "x.csv",
        json!({"format": "testrail-csv", "mode": "createOnly", "unknownFieldValues": "fail"}),
    )
    .await;
    assert_eq!(dry["errors"][0]["code"], "unknown_value", "{dry}");
    assert_eq!(dry["create"], 0);
}

fn zip_of(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut buf = std::io::Cursor::new(Vec::new());
    {
        let mut z = zip::ZipWriter::new(&mut buf);
        for (name, data) in entries {
            z.start_file(*name, zip::write::SimpleFileOptions::default())
                .unwrap();
            z.write_all(data).unwrap();
        }
        z.finish().unwrap();
    }
    buf.into_inner()
}

#[tokio::test]
async fn attachments_from_a_zip_and_traversal() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("att").await;
    let csv = "Key,Summary,Description,Files\nA-1,With file,\"See ![shot](shot.png)\",shot.png\nA-2,Missing,,nothere.png\n";
    let z = zip_of(&[("img/shot.png", b"\x89PNG fake image"), ("other.txt", b"x")]);
    let opts = json!({"format": "generic-csv", "mode": "createOnly", "unknownFieldValues": "skip",
        "mapping": {"Key": "key", "Summary": "name", "Description": "description", "Files": "attachments"}});
    let (st, j) = start_import(
        &s,
        "att",
        ADMIN_TOKEN,
        csv.as_bytes(),
        "a.csv",
        opts.clone(),
        Some(&z),
    )
    .await;
    assert_eq!(st, StatusCode::ACCEPTED, "{j}");
    let id = j["id"].as_str().unwrap();
    wait_job(&s, "att", id, &["ready"]).await;
    s.post(
        &format!("/api/v1/projects/att/imports/{id}/apply"),
        ADMIN_TOKEN,
        json!({}),
    )
    .await;
    let done = wait_job(&s, "att", id, &["done"]).await;
    assert_eq!(done["report"]["create"], 2, "{done}");
    let c = case(&s, "att", 1).await;
    let ids: Vec<uuid::Uuid> = sqlx::query_scalar(
        "select ca.attachment_id from case_attachments ca join cases c on c.id = ca.case_id where c.number = 1 \
         and c.project_id = (select id from projects where key = 'att')",
    )
    .fetch_all(&db.pool)
    .await
    .unwrap();
    assert_eq!(ids.len(), 1);
    let att_id = ids[0].to_string();
    let att_id = att_id.as_str();
    assert_eq!(
        c["body"]["description"],
        format!("See ![shot](attachment:{att_id})")
    );
    assert!(
        c["html"]["description"]
            .as_str()
            .unwrap()
            .contains(&format!("/attachments/{att_id}/content"))
    );

    let bad = zip_of(&[("../evil.png", b"x")]);
    let (_, j) = start_import(
        &s,
        "att",
        ADMIN_TOKEN,
        csv.as_bytes(),
        "a.csv",
        opts,
        Some(&bad),
    )
    .await;
    let id = j["id"].as_str().unwrap();
    wait_job(&s, "att", id, &["ready"]).await;
    s.post(
        &format!("/api/v1/projects/att/imports/{id}/apply"),
        ADMIN_TOKEN,
        json!({}),
    )
    .await;
    let failed = wait_job(&s, "att", id, &["failed"]).await;
    assert!(
        failed["report"]["errors"][0]["message"]
            .as_str()
            .unwrap()
            .contains("parent directory"),
        "{failed}"
    );
    assert_eq!(
        cases(&s, "att").await.len(),
        2,
        "nothing applied from the refused zip"
    );
}

#[tokio::test]
async fn export_round_trip_csv_guard_and_one_time_zip_link() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("exa").await;
    s.project("exb").await;
    let (_, f) = s
        .post(
            "/api/v1/projects/exa/case-folders",
            ADMIN_TOKEN,
            json!({"name": "Checkout"}),
        )
        .await;
    let (_, sub) = s
        .post(
            "/api/v1/projects/exa/case-folders",
            ADMIN_TOKEN,
            json!({"name": "Card", "parentId": f["id"]}),
        )
        .await;
    s.post("/api/v1/projects/exa/case-fields", ADMIN_TOKEN,
        json!({"key": "browser", "name": "Browser", "type": "select", "options": [{"value": "chrome"}, {"value": "firefox"}]})).await;
    let bodies = [
        json!({"name": "=HYPERLINK(\"https://evil.example.com\")", "folderId": sub["id"], "status": "draft",
            "body": {"description": "Pay with **card**", "preconditions": "Logged in",
                "steps": [{"action": "Open", "expected": "Shown", "steps": [{"action": "Nested", "data": "x"}]}, {"action": "Pay"}],
                "links": [{"name": "Spec", "url": "https://example.com/spec", "type": "link"}]},
            "fields": {"priority": "high", "browser": "firefox"}, "tags": ["pay", "smoke"]}),
        json!({"name": "Plain", "body": {"steps": []}}),
    ];
    for b in &bodies {
        let (st, r) = s
            .post("/api/v1/projects/exa/cases", ADMIN_TOKEN, b.clone())
            .await;
        assert_eq!(st, StatusCode::CREATED, "{r}");
    }
    let r = s
        .req(
            Method::GET,
            "/api/v1/projects/exa/cases/export?format=csv",
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
            .starts_with("text/csv")
    );
    let csv = r.text().await.unwrap();
    assert!(
        csv.contains("\"'=HYPERLINK(\"\"https://evil.example.com\"\")\""),
        "{csv}"
    );
    assert!(
        csv.lines()
            .next()
            .unwrap()
            .ends_with("browser,component,epic,feature,layer,owner,priority,severity,story,suite")
            || csv.contains("browser")
    );
    let r = s
        .req(
            Method::GET,
            "/api/v1/projects/exa/cases/export?format=ndjson&status=draft",
            Some(ADMIN_TOKEN),
        )
        .send()
        .await
        .unwrap();
    let nd = r.text().await.unwrap();
    assert_eq!(nd.lines().count(), 2, "header + the draft case: {nd}");
    let r = s
        .req(
            Method::GET,
            "/api/v1/projects/exa/cases/export?format=ndjson",
            Some(ADMIN_TOKEN),
        )
        .send()
        .await
        .unwrap();
    let nd = r.text().await.unwrap();

    let (_, applied) = import(
        &s,
        "exb",
        nd.as_bytes(),
        "cases.ndjson",
        json!({"format": "fastproof-ndjson", "mode": "createOnly", "unknownFieldValues": "create"}),
    )
    .await;
    assert_eq!(applied["create"], 2, "{applied}");
    for n in [1, 2] {
        let a = case(&s, "exa", n).await;
        let b = case(&s, "exb", n).await;
        for k in ["name", "status", "body", "fields", "tags"] {
            assert_eq!(a[k], b[k], "{k} of TC-{n}");
        }
        let path = |c: &Value| {
            c["folder"]["path"]
                .as_array()
                .map(|p| p.iter().map(|x| x["name"].clone()).collect::<Vec<_>>())
        };
        assert_eq!(path(&a), path(&b));
    }

    let (st, j) = s
        .post(
            "/api/v1/projects/exa/cases/exports",
            ADMIN_TOKEN,
            json!({"format": "zip", "attachments": true}),
        )
        .await;
    assert_eq!(st, StatusCode::ACCEPTED, "{j}");
    let url = j["downloadUrl"].as_str().unwrap().to_string();
    wait_job(&s, "exa", j["id"].as_str().unwrap(), &["done"]).await;
    let (_, again) = s
        .get(
            &format!("/api/v1/projects/exa/tm-jobs/{}", j["id"].as_str().unwrap()),
            ADMIN_TOKEN,
        )
        .await;
    assert!(
        again.get("downloadUrl").is_none(),
        "the link is shown once: {again}"
    );
    let other = s.token("other-admin", "admin", &["*"]).await;
    let r = s.req(Method::GET, &url, Some(&other)).send().await.unwrap();
    assert_eq!(r.status(), StatusCode::NOT_FOUND, "only for its creator");
    let bad = url
        .replace("token=", "token=0")
        .chars()
        .take(url.len())
        .collect::<String>();
    let r = s
        .req(Method::GET, &bad, Some(ADMIN_TOKEN))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::NOT_FOUND);
    let r = s
        .req(Method::GET, &url, Some(ADMIN_TOKEN))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let bytes = r.bytes().await.unwrap();
    let mut z = zip::ZipArchive::new(std::io::Cursor::new(bytes.to_vec())).unwrap();
    let names: Vec<String> = (0..z.len())
        .map(|i| z.by_index(i).unwrap().name().unwrap().to_string())
        .collect();
    assert!(
        names.contains(&"cases.ndjson".to_string()) && names.contains(&"cases.csv".to_string()),
        "{names:?}"
    );
    let r = s
        .req(Method::GET, &url, Some(ADMIN_TOKEN))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::NOT_FOUND, "one time");

    let (st, b) = s
        .post(
            "/api/v1/projects/exa/cases/exports",
            ADMIN_TOKEN,
            json!({"format": "pdf"}),
        )
        .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{b}");
    let (st, _) = s
        .get("/api/v1/projects/exa/cases/export?format=xml", ADMIN_TOKEN)
        .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn permissions_busy_limits_and_idor() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start_with(&db, &[("FASTPROOF_TM_MAX_IMPORT_BYTES", "2000")]).await;
    s.project("pa").await;
    s.project("pb").await;
    let viewer = s.token("v", "viewer", &["pa"]).await;
    let tester = s.token("t", "tester", &["pa"]).await;
    let uploader = s.token("u", "uploader", &["pa"]).await;
    let editor = s.token("e", "editor", &["pa"]).await;
    let editor_b = s.token("eb", "editor", &["pb"]).await;
    let file = fixture("testrail-cells.csv");
    let opts = testrail_options("createOnly");
    for t in [&viewer, &tester, &uploader, &editor_b] {
        let (st, _) = start_import(&s, "pa", t, &file, "c.csv", opts.clone(), None).await;
        assert_eq!(st, StatusCode::FORBIDDEN);
        let (st, _) = post_multipart(
            &s,
            "/api/v1/projects/pa/imports/preview",
            t,
            &[("file", Some("x.csv"), &file)],
        )
        .await;
        assert_eq!(st, StatusCode::FORBIDDEN);
    }
    let (st, _) = start_import(
        &s,
        "pa",
        "invalid-token-0123456789abcdef",
        &file,
        "c.csv",
        opts.clone(),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
    let r = s
        .req(
            Method::GET,
            "/api/v1/projects/pa/cases/export?format=csv",
            Some(&viewer),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK, "viewers may export");
    let (st, j) = start_import(&s, "pa", &editor, &file, "c.csv", opts.clone(), None).await;
    assert_eq!(st, StatusCode::ACCEPTED, "{j}");
    let id = j["id"].as_str().unwrap().to_string();
    let (st, _) = s
        .get(&format!("/api/v1/projects/pa/imports/{id}"), &viewer)
        .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (st, b) = s
        .get(&format!("/api/v1/projects/pa/tm-jobs/{id}"), &viewer)
        .await;
    assert_eq!(
        st,
        StatusCode::NOT_FOUND,
        "import jobs need cases:import, like /imports: {b}"
    );
    let (st, _) = s
        .get(&format!("/api/v1/projects/pa/tm-jobs/{id}"), &editor_b)
        .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (st, ex) = s
        .post(
            "/api/v1/projects/pa/cases/exports",
            &editor,
            json!({"format": "zip"}),
        )
        .await;
    assert_eq!(st, StatusCode::ACCEPTED, "{ex}");
    let ex_id = ex["id"].as_str().unwrap();
    let (st, _) = s
        .get(&format!("/api/v1/projects/pa/tm-jobs/{ex_id}"), &editor)
        .await;
    assert_eq!(st, StatusCode::OK);
    let (st, _) = s
        .get(&format!("/api/v1/projects/pa/tm-jobs/{ex_id}"), &viewer)
        .await;
    assert_eq!(
        st,
        StatusCode::NOT_FOUND,
        "export jobs only for their creator"
    );
    let (st, _) = s
        .get(
            &format!("/api/v1/projects/pa/tm-jobs/{}", uuid::Uuid::now_v7()),
            &viewer,
        )
        .await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    for path in [
        format!("/api/v1/projects/pb/imports/{id}"),
        format!("/api/v1/projects/pb/tm-jobs/{id}"),
    ] {
        let (st, _) = s.get(&path, ADMIN_TOKEN).await;
        assert_eq!(st, StatusCode::NOT_FOUND, "{path}");
    }
    let (st, _) = s
        .post(
            &format!("/api/v1/projects/pb/imports/{id}/apply"),
            ADMIN_TOKEN,
            json!({}),
        )
        .await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    let (st, _) = s
        .call(
            Method::DELETE,
            &format!("/api/v1/projects/pb/imports/{id}"),
            Some(ADMIN_TOKEN),
            None,
        )
        .await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    wait_job(&s, "pa", &id, &["ready"]).await;

    sqlx::query("update tm_jobs set status = 'running', lease_owner = 'elsewhere', lease_until = now() + interval '1 hour' where id = $1::uuid")
        .bind(&id)
        .execute(&db.pool)
        .await
        .unwrap();
    let (st, b) = start_import(&s, "pa", &editor, &file, "c.csv", opts.clone(), None).await;
    assert_eq!(st, StatusCode::CONFLICT, "{b}");
    assert_eq!(b["code"], "import_busy");
    let (st, b) = start_import(&s, "pb", ADMIN_TOKEN, &file, "c.csv", opts.clone(), None).await;
    assert_eq!(st, StatusCode::ACCEPTED, "another project is not busy: {b}");
    wait_job(&s, "pb", b["id"].as_str().unwrap(), &["ready"]).await;
    let (st, _) = s
        .call(
            Method::DELETE,
            &format!("/api/v1/projects/pa/imports/{id}"),
            Some(&editor),
            None,
        )
        .await;
    assert_eq!(st, StatusCode::OK);

    let (_, f) = s
        .post(
            "/api/v1/projects/pb/case-folders",
            ADMIN_TOKEN,
            json!({"name": "Other"}),
        )
        .await;
    let mut o = opts.clone();
    o["targetFolderId"] = f["id"].clone();
    let (st, _) = start_import(&s, "pa", &editor, &file, "c.csv", o, None).await;
    assert_eq!(st, StatusCode::NOT_FOUND, "a folder of another project");
    let big = vec![b'a'; 3000];
    let (st, b) = start_import(&s, "pa", &editor, &big, "big.csv", opts.clone(), None).await;
    assert_eq!(st, StatusCode::PAYLOAD_TOO_LARGE, "{b}");
    let (st, _) = start_import(&s, "pa", &editor, b"", "e.csv", opts.clone(), None).await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    let (st, _) = start_import(
        &s,
        "pa",
        &editor,
        &file,
        "c.csv",
        json!({"format": "testrail-csv", "mode": "x"}),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    let (st, _) = start_import(&s, "pa", &editor, &file, "c.csv",
        json!({"format": "generic-csv", "mode": "createOnly", "unknownFieldValues": "skip", "mapping": {"a": "evil"}}), None).await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    let (st, _) = s
        .post(
            "/api/v1/projects/pa/imports",
            &editor,
            json!({"source": "allure-testops", "baseUrl": "https://testops.example.com",
            "token": "t", "remoteProjectId": 1, "mode": "createOnly"}),
        )
        .await;
    assert_eq!(
        st,
        StatusCode::BAD_REQUEST,
        "the host is not in FASTPROOF_EGRESS_ALLOW"
    );
}

mod stub {
    use std::sync::{Arc, Mutex};

    use axum::Router;
    use axum::extract::{Path, Query, State};
    use axum::http::{HeaderMap, StatusCode};
    use axum::response::{IntoResponse, Response};
    use axum::routing::{get, post};
    use serde_json::{Value, json};

    pub const TOKEN: &str = "testops-api-token-SECRET-1234";
    pub const JWT: &str = "jwt-value-SECRET-5678";

    #[derive(Clone, Default)]
    pub struct Seen(pub Arc<Mutex<Vec<String>>>, pub bool);

    fn authed(h: &HeaderMap) -> bool {
        h.get("authorization").and_then(|v| v.to_str().ok()) == Some(&format!("Bearer {JWT}"))
    }

    async fn token(State(seen): State<Seen>, body: String) -> Response {
        seen.0
            .lock()
            .unwrap()
            .push(format!("token {}", body.contains(TOKEN)));
        if !body.contains("grant_type=apitoken") || !body.contains(TOKEN) {
            return StatusCode::UNAUTHORIZED.into_response();
        }
        axum::Json(json!({"access_token": JWT, "token_type": "bearer"})).into_response()
    }

    async fn search(
        State(seen): State<Seen>,
        h: HeaderMap,
        Query(q): Query<std::collections::HashMap<String, String>>,
    ) -> Response {
        if !authed(&h) {
            return StatusCode::UNAUTHORIZED.into_response();
        }
        let page: usize = q.get("page").and_then(|p| p.parse().ok()).unwrap_or(0);
        let ids: Vec<Value> = match page {
            0 => vec![json!({"id": 501}), json!({"id": 502})],
            1 if seen.1 => vec![json!({"id": 503})],
            _ => vec![],
        };
        axum::Json(json!({"content": ids, "totalPages": 2, "number": page, "last": page >= 1}))
            .into_response()
    }

    async fn case(h: HeaderMap, Path(id): Path<i64>) -> Response {
        if !authed(&h) {
            return StatusCode::UNAUTHORIZED.into_response();
        }
        match id {
            501 => axum::Json(json!({"id": 501, "name": "Login", "fullName": "com.example.LoginTest.login", "automated": true,
                "description": "<p>Logs <b>in</b></p>", "precondition": "User exists", "expectedResult": "Dashboard",
                "layer": {"name": "UI"}, "status": {"name": "Active"}, "tags": [{"name": "smoke"}]})).into_response(),
            502 => axum::Json(json!({"id": 502, "name": "Manual check", "automated": false, "status": {"name": "Draft"}})).into_response(),
            503 => axum::Json(json!({"id": 503, "name": "x".repeat(17 * 1024 * 1024)})).into_response(),
            _ => StatusCode::NOT_FOUND.into_response(),
        }
    }

    async fn step(Path(id): Path<i64>) -> Response {
        if id != 501 {
            return axum::Json(json!({"root": {"children": []}, "scenarioSteps": {}}))
                .into_response();
        }
        axum::Json(json!({"root": {"children": [1, 2]}, "scenarioSteps": {
            "1": {"body": "Open the page", "children": [3]}, "2": {"body": "Submit", "children": [4]},
            "3": {"body": "Wait"}, "4": {"attachmentId": 77}}, "attachments": {"77": {"name": "form.png"}}}))
        .into_response()
    }

    async fn cfv(Path(id): Path<i64>) -> Response {
        let _ = id;
        axum::Json(json!([{"customField": {"name": "Component"}, "name": "auth"}])).into_response()
    }

    async fn links(Path(id): Path<i64>) -> Response {
        let _ = id;
        axum::Json(
            json!([{"name": "Doc", "url": "https://docs.example.com/login", "type": "link"}]),
        )
        .into_response()
    }

    async fn attachments(Query(q): Query<std::collections::HashMap<String, String>>) -> Response {
        let id = q.get("testCaseId").cloned().unwrap_or_default();
        if id == "501" {
            return axum::Json(json!({"content": [{"id": 77, "name": "form.png"}]}))
                .into_response();
        }
        axum::Json(json!({"content": []})).into_response()
    }

    async fn content(Path(id): Path<i64>) -> Response {
        if id == 77 {
            return ([("content-type", "image/png")], b"\x89PNG form".to_vec()).into_response();
        }
        StatusCode::NOT_FOUND.into_response()
    }

    async fn moved() -> Response {
        (
            StatusCode::FOUND,
            [(
                "location",
                "https://elsewhere.example.net/api/uaa/oauth/token",
            )],
        )
            .into_response()
    }

    /// TestOps without `/api/rs` (only `/api`), or one that redirects the token call.
    pub async fn start(redirect: bool, big: bool) -> (String, Seen) {
        let seen = Seen(Default::default(), big);
        let mut app = Router::new();
        if redirect {
            app = app.route("/api/uaa/oauth/token", post(moved));
        } else {
            app = app.route("/api/uaa/oauth/token", post(token));
        }
        let app = app
            .route("/api/testcase/__search", get(search))
            .route("/api/testcase/{id}", get(case))
            .route("/api/testcase/{id}/step", get(step))
            .route("/api/testcase/{id}/cfv", get(cfv))
            .route("/api/testcase/{id}/link", get(links))
            .route(
                "/api/testcase/{id}/issue",
                get(|| async { axum::Json(json!([])) }),
            )
            .route("/api/testcase/attachment", get(attachments))
            .route("/api/testcase/attachment/{id}/content", get(content))
            .with_state(seen.clone());
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(l, app).await.unwrap();
        });
        (format!("http://{addr}"), seen)
    }
}

#[tokio::test]
async fn testops_api_import_through_the_egress_client() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let (base, seen) = stub::start(false, true).await;
    let (redirect_base, _) = stub::start(true, false).await;
    let dir = tempfile::tempdir().unwrap();
    let key = dir.path().join("secrets.key");
    std::fs::write(&key, format!("{KEY_HEX}\n")).unwrap();
    let port = base.rsplit(':').next().unwrap().to_string();
    let rport = redirect_base.rsplit(':').next().unwrap().to_string();
    let allow = format!("127.0.0.1:{port}, 127.0.0.1:{rport}");
    let s = Server::start_with(
        &db,
        &[
            ("FASTPROOF_EGRESS_ALLOW", &allow),
            ("FASTPROOF_SECRETS_KEY_FILE", key.to_str().unwrap()),
            ("FASTPROOF_TESTOPS_ALLOW_HTTP", "true"),
            ("FASTPROOF_TM_MAX_IMPORT_BYTES", "4194304"),
        ],
    )
    .await;
    s.project("to").await;

    let body = json!({"source": "allure-testops", "baseUrl": base, "token": stub::TOKEN, "remoteProjectId": 7, "mode": "upsert"});
    let (st, j) = s
        .post("/api/v1/projects/to/imports", ADMIN_TOKEN, body.clone())
        .await;
    assert_eq!(st, StatusCode::ACCEPTED, "{j}");
    let id = j["id"].as_str().unwrap().to_string();
    let failed = wait_job(&s, "to", &id, &["failed", "ready"]).await;
    assert_eq!(
        failed["status"], "failed",
        "case 503 is larger than 16 MiB: {failed}"
    );
    let msg = failed["report"]["errors"][0]["message"].as_str().unwrap();
    assert!(msg.contains("too large"), "{msg}");
    assert!(!failed.to_string().contains("SECRET"), "{failed}");
    assert!(seen.0.lock().unwrap().iter().any(|x| x == "token true"));

    let (st, j) = s
        .post(
            "/api/v1/projects/to/imports",
            ADMIN_TOKEN,
            json!({"source": "allure-testops", "baseUrl": redirect_base,
            "token": stub::TOKEN, "remoteProjectId": 7, "mode": "upsert"}),
        )
        .await;
    assert_eq!(st, StatusCode::ACCEPTED, "{j}");
    let r = wait_job(&s, "to", j["id"].as_str().unwrap(), &["failed"]).await;
    assert!(
        r["report"]["errors"][0]["message"]
            .as_str()
            .unwrap()
            .contains("another host"),
        "{r}"
    );

    let n: i64 =
        sqlx::query_scalar("select count(*) from project_secrets where name like 'tm-import-%'")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert!(n <= 2, "secrets are bound to their jobs");
    let (_, a) = s.get("/api/v1/audit?limit=500", ADMIN_TOKEN).await;
    assert!(
        !a.to_string().contains("SECRET"),
        "the token is never audited"
    );
    let (_, j) = s
        .get(&format!("/api/v1/projects/to/imports/{id}"), ADMIN_TOKEN)
        .await;
    assert!(!j.to_string().contains("SECRET"));
}

#[tokio::test]
async fn testops_api_import_creates_numbered_cases() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let (base, _) = stub::start(false, false).await;
    let dir = tempfile::tempdir().unwrap();
    let key = dir.path().join("secrets.key");
    std::fs::write(&key, format!("{KEY_HEX}\n")).unwrap();
    let port = base.rsplit(':').next().unwrap().to_string();
    let allow = format!("127.0.0.1:{port}");
    let s = Server::start_with(
        &db,
        &[
            ("FASTPROOF_EGRESS_ALLOW", &allow),
            ("FASTPROOF_SECRETS_KEY_FILE", key.to_str().unwrap()),
            ("FASTPROOF_TESTOPS_ALLOW_HTTP", "true"),
        ],
    )
    .await;
    s.project("tn").await;
    let (st, b) = s
        .post(
            "/api/v1/projects/tn/imports",
            ADMIN_TOKEN,
            json!({"source": "allure-testops", "baseUrl": base.replace("127.0.0.1", "localhost"),
            "token": stub::TOKEN, "remoteProjectId": 7, "mode": "upsert"}),
        )
        .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "localhost is not listed: {b}");
    let mut rejected = s
        .post(
            "/api/v1/projects/tn/imports",
            ADMIN_TOKEN,
            json!({"source": "allure-testops", "baseUrl": base,
        "token": stub::TOKEN, "remoteProjectId": 7, "mode": "upsert", "extra": stub::TOKEN}),
        )
        .await;
    assert_eq!(rejected.0, StatusCode::BAD_REQUEST);
    assert!(!rejected.1.to_string().contains("SECRET"), "{}", rejected.1);
    rejected.1 = Value::Null;
    let (st, j) = s
        .post(
            "/api/v1/projects/tn/imports",
            ADMIN_TOKEN,
            json!({"source": "allure-testops", "baseUrl": base, "token": stub::TOKEN,
            "remoteProjectId": 8, "mode": "upsert"}),
        )
        .await;
    assert_eq!(st, StatusCode::ACCEPTED, "{j}");
    let id = j["id"].as_str().unwrap().to_string();
    let r = wait_job(&s, "tn", &id, &["ready"]).await;
    assert_eq!(r["report"]["create"], 2, "{r}");
    assert!(!r.to_string().contains("SECRET"));
    s.post(
        &format!("/api/v1/projects/tn/imports/{id}/apply"),
        ADMIN_TOKEN,
        json!({}),
    )
    .await;
    let done = wait_job(&s, "tn", &id, &["done"]).await;
    assert_eq!(done["report"]["create"], 2, "{done}");
    let c = case(&s, "tn", 501).await;
    assert_eq!(c["name"], "Login");
    assert_eq!(c["status"], "active");
    assert_eq!(c["automated"], true);
    assert_eq!(c["body"]["description"], "Logs **in**");
    assert_eq!(c["body"]["preconditions"], "User exists");
    assert_eq!(c["body"]["steps"][0]["steps"][0]["action"], "Wait");
    assert_eq!(c["body"]["steps"][1]["expected"], "Dashboard");
    assert_eq!(
        c["body"]["steps"][1]["attachments"]
            .as_array()
            .unwrap()
            .len(),
        1,
        "{c}"
    );
    assert_eq!(c["fields"]["layer"], "UI");
    assert_eq!(c["tags"], json!(["smoke"]));
    assert!(
        c["identities"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["kind"] == "code" && i["fullName"] == "com.example.LoginTest.login")
    );
    assert!(
        c["keys"]
            .as_array()
            .unwrap()
            .iter()
            .any(|k| k["kind"] == "testops" && k["value"] == "501")
    );
    assert_eq!(
        c["body"]["links"][0]["url"],
        "https://docs.example.com/login"
    );
    let m = case(&s, "tn", 502).await;
    assert_eq!(m["status"], "draft");
    assert_eq!(m["automated"], false);
    let n: i64 =
        sqlx::query_scalar("select count(*) from project_secrets where name like 'tm-import-%'")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(n, 0, "the token is removed once the import is done");
}

/// 20 000 TestRail cases (12 steps each, 300 folders up to depth 3): dry run, apply, NDJSON
/// export. Run with `--ignored --nocapture`; prints the timings.
#[tokio::test]
#[ignore]
async fn timing_20k_testrail_import_and_ndjson_export() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start_with(&db, &[("FASTPROOF_DATABASE_MAX_CONNECTIONS", "16")]).await;
    s.project("bench").await;
    let mut csv = String::from(
        "ID,Title,Section Hierarchy,Priority,Type,Preconditions,Steps (Step),Steps (Expected Result)\n",
    );
    for i in 0..20_000 {
        let steps: Vec<String> = (1..=12)
            .map(|k| format!("{k}. Open page {k} of case {i} and press the button"))
            .collect();
        let exp: Vec<String> = (1..=12)
            .map(|k| format!("{k}. Page {k} shows the result"))
            .collect();
        csv.push_str(&format!(
            "C{i},Case number {i},Area {} > Module {} > Part {},High,Functional,User exists,\"{}\",\"{}\"\n",
            i % 10,
            i % 30,
            i % 300,
            steps.join("\n"),
            exp.join("\n")
        ));
    }
    let t0 = Instant::now();
    let (st, j) = start_import(
        &s,
        "bench",
        ADMIN_TOKEN,
        csv.as_bytes(),
        "bench.csv",
        testrail_options("createOnly"),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::ACCEPTED, "{j}");
    let id = j["id"].as_str().unwrap().to_string();
    let ready = wait_job(&s, "bench", &id, &["ready"]).await;
    let dry = t0.elapsed();
    assert_eq!(ready["report"]["create"], 20_000, "{}", ready["report"]);
    let t1 = Instant::now();
    s.post(
        &format!("/api/v1/projects/bench/imports/{id}/apply"),
        ADMIN_TOKEN,
        json!({}),
    )
    .await;
    let done = wait_job(&s, "bench", &id, &["done"]).await;
    let applied = t1.elapsed();
    assert_eq!(done["report"]["create"], 20_000, "{}", done["report"]);
    let t2 = Instant::now();
    let r = s
        .req(
            Method::GET,
            "/api/v1/projects/bench/cases/export?format=ndjson",
            Some(ADMIN_TOKEN),
        )
        .send()
        .await
        .unwrap();
    let body = r.text().await.unwrap();
    let export = t2.elapsed();
    assert_eq!(body.lines().count(), 20_001);
    println!(
        "TIMING 20k TestRail CSV ({} MB): upload+dry run {:.1}s, apply {:.1}s, total {:.1}s; NDJSON export {:.1}s ({} MB)",
        csv.len() / 1_000_000,
        dry.as_secs_f64(),
        applied.as_secs_f64(),
        (dry + applied).as_secs_f64(),
        export.as_secs_f64(),
        body.len() / 1_000_000
    );
}

async fn cli(args: &[&str]) -> i32 {
    let cli = <fastproof_server::cli::Cli as clap::Parser>::try_parse_from(args).unwrap();
    fastproof_server::cli::run(cli).await.unwrap()
}

#[tokio::test]
async fn cli_cases_export_and_import() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("cli-a").await;
    s.project("cli-b").await;
    for i in 0..3 {
        let (st, b) = s
            .post("/api/v1/projects/cli-a/cases", ADMIN_TOKEN, json!({"name": format!("Case {i}"),
                "body": {"steps": [{"action": format!("Do {i}"), "expected": "Done"}]}, "tags": ["cli"]}))
            .await;
        assert_eq!(st, StatusCode::CREATED, "{b}");
    }
    let dir = tempfile::tempdir().unwrap();
    let token = dir.path().join("token");
    std::fs::write(&token, ADMIN_TOKEN).unwrap();
    let out = dir.path().join("cases.ndjson");
    let (url, tok, out_s) = (
        s.base.clone(),
        token.to_str().unwrap().to_string(),
        out.to_str().unwrap().to_string(),
    );
    let common = ["--url", url.as_str(), "--token-file", tok.as_str()];
    let mut a = vec!["fastproof", "cases", "export"];
    a.extend(common);
    a.extend([
        "--project",
        "cli-a",
        "--format",
        "ndjson",
        "--out",
        out_s.as_str(),
    ]);
    assert_eq!(cli(&a).await, 0);
    assert_eq!(std::fs::read_to_string(&out).unwrap().lines().count(), 4);
    let mut a = vec!["fastproof", "cases", "import"];
    a.extend(common);
    a.extend([
        "--project",
        "cli-b",
        out_s.as_str(),
        "--format",
        "fastproof-ndjson",
    ]);
    assert_eq!(cli(&a).await, 0);
    assert!(cases(&s, "cli-b").await.is_empty(), "a dry run by default");
    a.push("--apply");
    assert_eq!(cli(&a).await, 0);
    let b = cases(&s, "cli-b").await;
    assert_eq!(b.len(), 3);
    assert_eq!(
        case(&s, "cli-b", 2).await["body"],
        case(&s, "cli-a", 2).await["body"]
    );
}

fn tm_job_files(s: &Server) -> Vec<std::path::PathBuf> {
    let root = s.dir.path().join("blobs").join("tm-jobs");
    let mut out = Vec::new();
    for d in std::fs::read_dir(&root).into_iter().flatten().flatten() {
        for f in std::fs::read_dir(d.path()).into_iter().flatten().flatten() {
            out.push(f.path());
        }
    }
    out
}

#[tokio::test]
async fn deleting_a_project_removes_its_job_files() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("gone").await;
    s.project("kept").await;
    for key in ["gone", "kept"] {
        let (st, j) = start_import(
            &s,
            key,
            ADMIN_TOKEN,
            &fixture("testrail-cells.csv"),
            "c.csv",
            testrail_options("createOnly"),
            None,
        )
        .await;
        assert_eq!(st, StatusCode::ACCEPTED, "{j}");
        wait_job(&s, key, j["id"].as_str().unwrap(), &["ready"]).await;
    }
    s.post(
        "/api/v1/projects/gone/cases",
        ADMIN_TOKEN,
        json!({"name": "x", "body": {}}),
    )
    .await;
    let (_, e) = s
        .post(
            "/api/v1/projects/gone/cases/exports",
            ADMIN_TOKEN,
            json!({"format": "zip"}),
        )
        .await;
    wait_job(&s, "gone", e["id"].as_str().unwrap(), &["done"]).await;
    let before = tm_job_files(&s);
    assert_eq!(
        before.len(),
        3,
        "two import sources and one export: {before:?}"
    );
    let (st, _) = s
        .call(
            Method::DELETE,
            "/api/v1/projects/gone",
            Some(ADMIN_TOKEN),
            None,
        )
        .await;
    assert!(st.is_success(), "{st}");
    sqlx::query("update scheduled_jobs set next_run_at = now() where name = 'projects.purge'")
        .execute(&db.pool)
        .await
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let left = tm_job_files(&s);
        if left.len() == 1 {
            break;
        }
        assert!(Instant::now() < deadline, "job files left: {left:?}");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let (st, _) = s.get("/api/v1/projects/kept/cases", ADMIN_TOKEN).await;
    assert_eq!(st, StatusCode::OK);
}
