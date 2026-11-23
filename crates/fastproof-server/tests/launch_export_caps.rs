//! Byte cap of a launch export file (its own process: the cap is an environment variable).
//! Skipped without `FASTPROOF_TEST_DATABASE_URL`.

mod common;

use common::{ADMIN_TOKEN, Server, TestDb};
use reqwest::{Method, StatusCode};
use serde_json::json;

#[tokio::test]
async fn a_job_over_the_byte_cap_fails_and_an_inline_export_answers_409() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    unsafe { std::env::set_var("FASTPROOF_LAUNCH_EXPORT_MAX_BYTES", "4000") };
    let s = Server::start(&db).await;
    s.project("cap").await;
    let l = s.launch("cap", None).await;
    let id = l["id"].as_str().unwrap().to_string();
    let rs: Vec<String> = (0..60)
        .map(|i| {
            json!({"id": format!("r{i}"), "name": format!("test number {i} with a longish name"),
                "fullName": format!("suite.test_number_{i}_with_a_longish_name"), "status": "passed",
                "start": 1795249075000i64, "stop": 1795249075100i64})
            .to_string()
        })
        .collect();
    let r = s
        .req(
            Method::POST,
            &format!("/api/v1/projects/cap/launches/{id}/results"),
            Some(ADMIN_TOKEN),
        )
        .header("content-type", "application/x-ndjson")
        .body(rs.join("\n"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::ACCEPTED);

    let r = s
        .req(
            Method::GET,
            &format!("/api/v1/projects/cap/launches/{id}/export?format=html"),
            Some(ADMIN_TOKEN),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::CONFLICT);
    assert_eq!(
        r.json::<serde_json::Value>().await.unwrap()["code"],
        "export_too_large"
    );

    let (st, job) = s
        .call(
            Method::POST,
            &format!("/api/v1/projects/cap/launches/{id}/exports?format=xlsx"),
            Some(ADMIN_TOKEN),
            None,
        )
        .await;
    assert_eq!(st, StatusCode::ACCEPTED, "{job}");
    let base = format!(
        "/api/v1/projects/cap/launches/{id}/exports/{}",
        job["id"].as_str().unwrap()
    );
    let mut last = serde_json::Value::Null;
    for _ in 0..100 {
        let (_, j) = s.get(&base, ADMIN_TOKEN).await;
        if j["status"] == "failed" || j["status"] == "done" {
            last = j;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert_eq!(last["status"], "failed", "{last}");
    assert!(last["error"].as_str().unwrap().contains("exceed"), "{last}");
    unsafe { std::env::remove_var("FASTPROOF_LAUNCH_EXPORT_MAX_BYTES") };
}
