//! End-to-end throughput (ignored by default): 5 000 Allure results with steps and one
//! attachment each, uploaded through HTTP into PostgreSQL and the filesystem blob store.
//!
//! `FASTPROOF_TEST_DATABASE_URL=... cargo test --release -p fastproof-server --test bench -- --ignored --nocapture`

mod common;

use std::time::Instant;

use common::{Server, TestDb, tar_gz};

fn peak_rss_kib() -> Option<u64> {
    let s = std::fs::read_to_string("/proc/self/status").ok()?;
    s.lines()
        .find(|l| l.starts_with("VmHWM:"))
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|v| v.parse().ok())
}

fn step(depth: u32, i: u32) -> String {
    let children = if depth < 2 {
        (0..3)
            .map(|j| step(depth + 1, j))
            .collect::<Vec<_>>()
            .join(",")
    } else {
        String::new()
    };
    format!(
        r#"{{"name":"step {depth}.{i} with a realistic name","status":"passed","stage":"finished","start":1795249075967,"stop":1795249075999,"parameters":[{{"name":"k","value":"v{i}"}}],"steps":[{children}]}}"#
    )
}

#[tokio::test]
#[ignore]
async fn ingest_5000_allure_results_end_to_end() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("bench").await;
    let launch = s.launch("bench", None).await;
    let lid = launch["id"].as_str().unwrap();
    let n = 5000;
    let dir = tempfile::tempdir().unwrap();
    let steps = (0..2).map(|i| step(0, i)).collect::<Vec<_>>().join(",");
    for i in 0..n {
        let status = if i % 20 == 0 { "failed" } else { "passed" };
        let doc = format!(
            r#"{{"uuid":"r{i}","historyId":"h{i}","testCaseId":"tc{i}","name":"test {i}","fullName":"com.example.suite{m}.Test{i}#run","status":"{status}","statusDetails":{{"message":"expected: <1> but was: <2>","trace":"java.lang.AssertionError\n\tat com.example.Test.run(Test.java:{i})"}},"stage":"finished","start":1795249075967,"stop":1795249076967,"labels":[{{"name":"suite","value":"suite{m}"}},{{"name":"feature","value":"f{m}"}},{{"name":"severity","value":"normal"}}],"parameters":[{{"name":"browser","value":"chromium"}},{{"name":"user","value":"u{i}","mode":"masked"}}],"links":[{{"type":"issue","url":"https://issues.example.com/T-{i}"}}],"attachments":[{{"name":"log","source":"r{i}-attachment.txt","type":"text/plain"}}],"steps":[{steps}]}}"#,
            m = i % 50
        );
        std::fs::write(dir.path().join(format!("r{i}-result.json")), doc).unwrap();
        std::fs::write(
            dir.path().join(format!("r{i}-attachment.txt")),
            format!("log line for {i}\n").repeat(20),
        )
        .unwrap();
        if i % 10 == 0 {
            let children: Vec<String> = (i..i + 10).map(|c| format!("\"r{c}\"")).collect();
            std::fs::write(
                dir.path().join(format!("c{i}-container.json")),
                format!(r#"{{"uuid":"c{i}","children":[{}],"befores":[{{"name":"setUp","status":"passed","start":1,"stop":2}}],"afters":[{{"name":"tearDown","status":"passed","start":3,"stop":4}}]}}"#, children.join(",")),
            )
            .unwrap();
        }
    }
    let bytes = tar_gz(dir.path(), &|_| true);
    let size = bytes.len();
    let rss_before = peak_rss_kib();
    let started = Instant::now();
    let u = s.ingest("bench", lid, bytes, "").await;
    let elapsed = started.elapsed();
    assert_eq!(u["status"], "done", "{u}");
    assert_eq!(u["counts"]["results"], n);
    let l = s.launch_get("bench", lid).await;
    assert_eq!(l["counts"]["total"], n);
    eprintln!(
        "BENCH: {n} allure results ({} steps each, {} attachments, {size} bytes tar.gz) through HTTP into PostgreSQL + fs in {:.2} s; peak RSS before {:?} KiB, after {:?} KiB",
        1 + 2 * 13,
        u["counts"]["attachments"],
        elapsed.as_secs_f64(),
        rss_before,
        peak_rss_kib()
    );
    assert!(
        elapsed.as_secs_f64() < 10.0,
        "target is under 10 s, took {elapsed:?}"
    );
}
