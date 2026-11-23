//! Launch export: the self-contained HTML report, the XLSX workbook, filters, the size limit of
//! the inline export and the background job. Skipped without `FASTPROOF_TEST_DATABASE_URL`.

mod common;

use std::io::Read;

use common::{ADMIN_TOKEN, Server, TestDb};
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};

async fn post_results(s: &Server, project: &str, launch: &str, results: &[Value]) {
    let body: String = results
        .iter()
        .map(|r| r.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    let r = s
        .req(
            Method::POST,
            &format!("/api/v1/projects/{project}/launches/{launch}/results"),
            Some(ADMIN_TOKEN),
        )
        .header("content-type", "application/x-ndjson")
        .body(body)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::ACCEPTED);
    let out: Value = r.json().await.unwrap();
    assert_eq!(out["accepted"], results.len(), "{out}");
}

fn result(id: &str, name: &str, status: &str, message: Option<&str>) -> Value {
    let mut r = json!({"id": id, "name": name, "fullName": format!("shop.{name}"), "status": status,
        "start": 1795249075000i64, "stop": 1795249075000i64 + 250});
    if let Some(m) = message {
        r["error"] = json!({"message": m});
    }
    r
}

async fn hostile_launch(s: &Server) -> String {
    s.project("exp").await;
    let l = s.launch("exp", None).await;
    let id = l["id"].as_str().unwrap().to_string();
    post_results(
        s,
        "exp",
        &id,
        &[
            result(
                "a",
                "<script>alert(1)</script>",
                "failed",
                Some("<img src=x onerror=alert(2)> boom"),
            ),
            result(
                "b",
                "=HYPERLINK(\"http://198.51.100.9\",\"x\")",
                "passed",
                None,
            ),
            result("c", "plain", "broken", Some("line1\nline2 \u{1}")),
            result("d", "other", "passed", None),
        ],
    )
    .await;
    id
}

async fn get_export(s: &Server, token: &str, id: &str, query: &str) -> reqwest::Response {
    s.req(
        Method::GET,
        &format!("/api/v1/projects/exp/launches/{id}/export?{query}"),
        Some(token),
    )
    .send()
    .await
    .unwrap()
}

#[tokio::test]
async fn html_report_is_self_contained_and_escaped() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    let id = hostile_launch(&s).await;

    let r = get_export(&s, ADMIN_TOKEN, &id, "format=html").await;
    assert_eq!(r.status(), StatusCode::OK);
    let h = r.headers().clone();
    assert!(h["content-type"].to_str().unwrap().starts_with("text/html"));
    assert!(
        h["content-disposition"]
            .to_str()
            .unwrap()
            .starts_with("attachment")
    );
    assert!(
        h["content-security-policy"]
            .to_str()
            .unwrap()
            .contains("sandbox")
    );
    assert_eq!(h["x-content-type-options"], "nosniff");
    let body = r.text().await.unwrap();
    if let Ok(out) = std::env::var("FASTPROOF_TEST_HTML_OUT") {
        std::fs::write(out, &body).unwrap();
    }
    assert!(!body.contains("<script"), "no script element");
    assert!(!body.contains("<img"), "markup of a test is escaped");
    assert!(body.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
    assert!(body.contains("&lt;img src=x onerror=alert(2)&gt; boom"));
    for needle in [
        "src=\"http",
        "href=\"http",
        "url(http",
        "@import",
        "<link",
        "<iframe",
    ] {
        assert!(!body.contains(needle), "{needle}");
    }
    assert!(body.contains("4 results"));
    assert!(body.contains("@media print"));
    assert!(body.contains("default-src 'none'"));

    let r = get_export(&s, ADMIN_TOKEN, &id, "format=html&status=failed,broken").await;
    let body = r.text().await.unwrap();
    assert!(body.contains("2 results"), "{body}");
    assert!(!body.contains("shop.other"));

    let r = get_export(&s, ADMIN_TOKEN, &id, "format=html&inline=true").await;
    assert!(
        r.headers()["content-disposition"]
            .to_str()
            .unwrap()
            .starts_with("inline")
    );

    assert_eq!(
        get_export(&s, ADMIN_TOKEN, &id, "format=pdf")
            .await
            .status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        get_export(&s, ADMIN_TOKEN, &id, "format=html&status=nope")
            .await
            .status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        get_export(
            &s,
            ADMIN_TOKEN,
            "00000000-0000-0000-0000-000000000000",
            "format=html"
        )
        .await
        .status(),
        StatusCode::NOT_FOUND
    );
    let r = s
        .req(
            Method::GET,
            &format!("/api/v1/projects/exp/launches/{id}/export?format=html"),
            None,
        )
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn permissions_follow_reading_the_launch() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    let id = hostile_launch(&s).await;
    s.project("other").await;
    let viewer = s.token("viewer", "viewer", &["exp"]).await;
    let elsewhere = s.token("elsewhere", "viewer", &["other"]).await;
    assert_eq!(
        get_export(&s, &viewer, &id, "format=xlsx").await.status(),
        StatusCode::OK
    );
    assert_eq!(
        get_export(&s, &elsewhere, &id, "format=xlsx")
            .await
            .status(),
        StatusCode::FORBIDDEN
    );
    let (st, _) = s
        .call(
            Method::POST,
            &format!("/api/v1/projects/exp/launches/{id}/exports?format=xlsx"),
            Some(&elsewhere),
            None,
        )
        .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
}

fn entry(zip: &mut zip::ZipArchive<std::io::Cursor<Vec<u8>>>, name: &str) -> String {
    let mut s = String::new();
    zip.by_name(name).unwrap().read_to_string(&mut s).unwrap();
    s
}

#[tokio::test]
async fn xlsx_has_summary_and_filtered_results() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    let id = hostile_launch(&s).await;
    let r = get_export(&s, ADMIN_TOKEN, &id, "format=xlsx&status=passed").await;
    assert_eq!(r.status(), StatusCode::OK);
    assert!(
        r.headers()["content-type"]
            .to_str()
            .unwrap()
            .contains("spreadsheetml")
    );
    let bytes = r.bytes().await.unwrap().to_vec();
    if let Ok(out) = std::env::var("FASTPROOF_TEST_XLSX_OUT") {
        std::fs::write(out, &bytes).unwrap();
    }
    let mut z = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
    let summary = entry(&mut z, "xl/worksheets/sheet1.xml");
    assert!(summary.contains("status=passed"));
    assert!(summary.contains("<v>4</v>"), "the launch counts: {summary}");
    let sheet = entry(&mut z, "xl/worksheets/sheet2.xml");
    assert_eq!(
        sheet.matches("<row ").count(),
        3,
        "header and two passed results"
    );
    assert!(sheet.contains("&quot;http://198.51.100.9&quot;"));
    assert!(sheet.contains("t=\"inlineStr\""));
    assert!(!sheet.contains("<f>"), "no formulas");
    assert!(sheet.contains("<autoFilter ref=\"A1:P3\"/>"));
    assert!(!sheet.contains("&lt;script"), "filtered out");
    let wb = entry(&mut z, "xl/workbook.xml");
    assert!(wb.contains("name=\"Summary\"") && wb.contains("name=\"Results\""));
}

async fn big_launch(s: &Server, n: usize) -> String {
    s.project("exp").await;
    let l = s.launch("exp", None).await;
    let id = l["id"].as_str().unwrap().to_string();
    for chunk in (0..n).collect::<Vec<_>>().chunks(2000) {
        let rs: Vec<Value> = chunk
            .iter()
            .map(|i| {
                result(
                    &format!("r{i}"),
                    &format!("t{i:06}"),
                    if i % 7 == 0 { "failed" } else { "passed" },
                    None,
                )
            })
            .collect();
        post_results(s, "exp", &id, &rs).await;
    }
    for _ in 0..100 {
        let l = s.launch_get("exp", &id).await;
        if l["counts"]["total"] == n {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    id
}

#[tokio::test]
async fn large_launch_needs_a_job_which_exports_everything() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    let id = big_launch(&s, 10_300).await;

    let r = get_export(&s, ADMIN_TOKEN, &id, "format=xlsx").await;
    assert_eq!(r.status(), StatusCode::CONFLICT);
    let body: Value = r.json().await.unwrap();
    assert_eq!(body["code"], "export_too_large");

    let (st, job) = s
        .call(
            Method::POST,
            &format!("/api/v1/projects/exp/launches/{id}/exports?format=xlsx&status=failed"),
            Some(ADMIN_TOKEN),
            None,
        )
        .await;
    assert_eq!(st, StatusCode::ACCEPTED, "{job}");
    let jid = job["id"].as_str().unwrap().to_string();
    let base = format!("/api/v1/projects/exp/launches/{id}/exports/{jid}");

    let viewer = s.token("viewer", "viewer", &["exp"]).await;
    let (st, _) = s.get(&base, &viewer).await;
    assert_eq!(
        st,
        StatusCode::NOT_FOUND,
        "another caller does not see the job"
    );

    let mut last = Value::Null;
    for _ in 0..300 {
        let (st, j) = s.get(&base, ADMIN_TOKEN).await;
        assert_eq!(st, StatusCode::OK);
        if j["status"] == "done" || j["status"] == "failed" {
            last = j;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
    assert_eq!(last["status"], "done", "{last}");
    let want = (0..10_300).filter(|i| i % 7 == 0).count();
    assert_eq!(last["done"], want);
    assert!(last["downloadUrl"].as_str().unwrap().ends_with("/download"));

    let (st, _) = s.get(&format!("{base}/download"), &viewer).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    let r = s
        .req(Method::GET, &format!("{base}/download"), Some(ADMIN_TOKEN))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let bytes = r.bytes().await.unwrap().to_vec();
    let mut z = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
    let sheet = entry(&mut z, "xl/worksheets/sheet2.xml");
    assert_eq!(sheet.matches("<row ").count(), want + 1);
}

#[tokio::test]
async fn active_exports_are_bounded_per_caller_and_stale_ones_fail() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    let id = hostile_launch(&s).await;
    let path = format!("/api/v1/projects/exp/launches/{id}/exports?format=html");
    let (st, first) = s.call(Method::POST, &path, Some(ADMIN_TOKEN), None).await;
    assert_eq!(st, StatusCode::ACCEPTED, "{first}");
    let me = first["createdBy"].as_str().unwrap().to_string();
    let (project,): (uuid::Uuid,) = sqlx::query_as("select id from projects where key = 'exp'")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    for _ in 0..2 {
        sqlx::query(
            "insert into launch_exports (id, project_id, launch_id, format, created_by, status) \
             values (gen_random_uuid(), $1, gen_random_uuid(), 'html', $2, 'running')",
        )
        .bind(project)
        .bind(&me)
        .execute(&db.pool)
        .await
        .unwrap();
    }
    let (st, b) = s.call(Method::POST, &path, Some(ADMIN_TOKEN), None).await;
    assert_eq!(st, StatusCode::CONFLICT, "{b}");
    assert_eq!(b["code"], "export_busy");

    sqlx::query("update launch_exports set updated_at = now() - interval '10 minutes' where status = 'running'")
        .execute(&db.pool)
        .await
        .unwrap();
    let store = &s.running.as_ref().unwrap().state.store;
    assert_eq!(store.fail_stale_launch_exports().await.unwrap(), 2);
    let (st, _) = s.call(Method::POST, &path, Some(ADMIN_TOKEN), None).await;
    assert_eq!(st, StatusCode::ACCEPTED);
}

#[tokio::test]
async fn cli_exports_html_xlsx_and_prints_pdf_with_a_browser() {
    use fastproof_server::cli::Conn;
    use fastproof_server::cli::export::{ExportArgs, run};
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    let id = hostile_launch(&s).await;
    let dir = tempfile::tempdir().unwrap();
    let args = |format: &str, out: &std::path::Path| ExportArgs {
        conn: Conn {
            url: s.base.clone(),
            token_file: None,
            project: "exp".into(),
        },
        launch: id.clone(),
        format: format.into(),
        out: Some(out.to_path_buf()),
        status: Some("failed".into()),
        filter: None,
        background: format == "xlsx",
        timeout: 60,
        chromium: None,
        no_sandbox: false,
    };
    unsafe { std::env::set_var("FASTPROOF_TOKEN", ADMIN_TOKEN) };

    let html = dir.path().join("r.html");
    assert_eq!(run(args("html", &html)).await.unwrap(), 0);
    let text = std::fs::read_to_string(&html).unwrap();
    assert!(text.contains("1 results") && text.contains("&lt;script&gt;"));

    let xlsx = dir.path().join("r.xlsx");
    assert_eq!(run(args("xlsx", &xlsx)).await.unwrap(), 0);
    let mut z = zip::ZipArchive::new(std::io::Cursor::new(std::fs::read(&xlsx).unwrap())).unwrap();
    assert_eq!(
        entry(&mut z, "xl/worksheets/sheet2.xml")
            .matches("<row ")
            .count(),
        2
    );

    let fake = dir.path().join("fake-chromium");
    std::fs::write(
        &fake,
        "#!/bin/sh\nfor a in \"$@\"; do case \"$a\" in --print-to-pdf=*) f=\"${a#--print-to-pdf=}\";; esac; done\nprintf '%%PDF-1.4 fake' > \"$f\"\n",
    )
    .unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let pdf = dir.path().join("r.pdf");
    let mut a = args("pdf", &pdf);
    a.chromium = Some(fake);
    assert_eq!(run(a).await.unwrap(), 0);
    assert!(std::fs::read(&pdf).unwrap().starts_with(b"%PDF"));

    let mut a = args("pdf", &pdf);
    a.chromium = None;
    let old_path = std::env::var_os("PATH");
    unsafe { std::env::set_var("PATH", dir.path()) };
    let err = run(a).await.unwrap_err().to_string();
    if let Some(p) = old_path {
        unsafe { std::env::set_var("PATH", p) };
    }
    assert!(err.contains("no Chromium found"), "{err}");
}
