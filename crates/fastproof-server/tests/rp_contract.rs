//! Contract tests of `/rp` on the recordings of real agents (`crates/fastproof-rp/tests/recordings`):
//! every recording is replayed in order against the real routes (direct and Kafka) and the
//! resulting launch, results, attempts, steps, logs and attachments are compared with a committed
//! snapshot (`crates/fastproof-rp/tests/snapshots`, rewritten with `FASTPROOF_UPDATE_SNAPSHOTS=1`).
//! Variations: duplicated requests, logs after the launch finish, no finish at all, files missing
//! from a multipart request, the fault recordings (requests the recorder answered with 503 never
//! reached a server and are skipped, as the agents saw them fail).

mod common;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use common::{ADMIN_TOKEN, Server, TestDb};
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};

const PROJECT: &str = "example-project";
const FAST: &[(&str, &str)] = &[
    ("FASTPROOF_RP_SETTLE_MS", "200"),
    ("FASTPROOF_RP_FINISH_GRACE_SECONDS", "3"),
];

fn recordings_dir() -> PathBuf {
    match std::env::var("FASTPROOF_TEST_RP_RECORDINGS") {
        Ok(d) => PathBuf::from(d),
        Err(_) => {
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../fastproof-rp/tests/recordings")
        }
    }
}

fn snapshots_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../fastproof-rp/tests/snapshots")
}

fn recordings() -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(recordings_dir())
        .map(|d| {
            d.filter_map(|e| e.ok())
                .filter_map(|e| {
                    let p = e.path();
                    (p.extension()? == "ndjson")
                        .then(|| p.file_stem()?.to_str().map(String::from))?
                })
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

/// The entries of a recording. Recorded uuids are numbered from 1 in every file; a random
/// prefix keeps replays of several recordings in one project apart, as real agents' uuids are.
fn load(name: &str) -> Vec<Value> {
    let salt = format!("{:08x}", rand::random::<u32>());
    std::fs::read_to_string(recordings_dir().join(format!("{name}.ndjson")))
        .unwrap()
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            serde_json::from_str(&l.replace(
                "00000000-0000-4000-8000-",
                &format!("{salt}-0000-4000-8000-"),
            ))
            .unwrap()
        })
        .collect()
}

#[derive(Clone, Default)]
struct Opts {
    /// Send item starts and finishes and launch finishes twice (a retry after a lost response).
    duplicate: bool,
    /// Move every log request behind the launch finish.
    logs_after_finish: bool,
    /// Leave out the launch finish.
    no_launch_finish: bool,
    /// Send multipart log requests without their files.
    drop_files: bool,
}

struct Replayed {
    launches: Vec<String>,
    observations: Vec<String>,
}

fn is_log(e: &Value) -> bool {
    let p = e["path"].as_str().unwrap();
    p.ends_with("/log") || p.ends_with("/log/entry")
}

fn is_launch_finish(e: &Value) -> bool {
    e["method"] == "PUT" && e["path"].as_str().unwrap().ends_with("/finish")
}

fn replace_ids(v: &Value, ids: &HashMap<String, String>) -> Value {
    match v {
        Value::String(s) => Value::String(ids.get(s).cloned().unwrap_or_else(|| s.clone())),
        Value::Array(a) => Value::Array(a.iter().map(|x| replace_ids(x, ids)).collect()),
        Value::Object(m) => Value::Object(
            m.iter()
                .map(|(k, x)| (k.clone(), replace_ids(x, ids)))
                .collect(),
        ),
        other => other.clone(),
    }
}

fn map_path(path: &str, ids: &HashMap<String, String>) -> String {
    path.split('/')
        .map(|seg| ids.get(seg).cloned().unwrap_or_else(|| seg.to_string()))
        .collect::<Vec<_>>()
        .join("/")
}

async fn send(
    s: &Server,
    token: &str,
    e: &Value,
    ids: &HashMap<String, String>,
    opts: &Opts,
) -> (StatusCode, Value) {
    let method = Method::from_bytes(e["method"].as_str().unwrap().as_bytes()).unwrap();
    let path = map_path(e["path"].as_str().unwrap(), ids);
    let mut query = String::new();
    if let Some(q) = e["query"].as_object() {
        for (k, v) in q {
            for x in v.as_array().into_iter().flatten() {
                let x = x.as_str().unwrap();
                query.push(if query.is_empty() { '?' } else { '&' });
                query.push_str(&format!("{k}={}", ids.get(x).map_or(x, |v| v.as_str())));
            }
        }
    }
    let rb = s.req(method, &format!("{path}{query}"), Some(token));
    let resp = if let Some(parts) = e["multipart"].as_array() {
        let boundary = "replay-boundary-0042";
        let mut b = Vec::new();
        for p in parts {
            if opts.drop_files && p.get("json").is_none() {
                continue;
            }
            let name = p["name"].as_str().unwrap_or("file");
            let disp = match p["filename"].as_str() {
                Some(f) => format!("form-data; name=\"{name}\"; filename=\"{f}\""),
                None => format!("form-data; name=\"{name}\""),
            };
            b.extend_from_slice(
                format!(
                    "--{boundary}\r\nContent-Disposition: {disp}\r\nContent-Type: {}\r\n\r\n",
                    p["contentType"]
                        .as_str()
                        .unwrap_or("application/octet-stream")
                )
                .as_bytes(),
            );
            if p.get("json").is_some() {
                b.extend_from_slice(replace_ids(&p["json"], ids).to_string().as_bytes());
            } else {
                let n = p["size"].as_u64().unwrap_or(0) as usize;
                b.extend((0..n).map(|i| (i % 251) as u8));
            }
            b.extend_from_slice(b"\r\n");
        }
        b.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
        rb.header(
            "content-type",
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(b)
        .send()
        .await
        .unwrap()
    } else if e.get("body").is_some() {
        rb.json(&replace_ids(&e["body"], ids)).send().await.unwrap()
    } else {
        rb.send().await.unwrap()
    };
    let st = resp.status();
    (st, resp.json().await.unwrap_or(Value::Null))
}

async fn fp_launch_of(s: &Server, number: i64) -> String {
    let (_, b) = s
        .get(
            &format!("/api/v1/projects/{PROJECT}/launches?limit=100"),
            ADMIN_TOKEN,
        )
        .await;
    b["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|l| l["number"] == number)
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string()
}

/// Replays a recording; requests the recorder answered with an injected fault are skipped.
async fn replay(s: &Server, token: &str, name: &str, opts: &Opts) -> Replayed {
    let mut entries: Vec<Value> = load(name)
        .into_iter()
        .filter(|e| e["injected"] != true)
        .collect();
    if opts.no_launch_finish {
        entries.retain(|e| !is_launch_finish(e));
    }
    if opts.logs_after_finish
        && let Some(at) = entries.iter().position(is_launch_finish)
    {
        let (logs, mut moved): (Vec<Value>, Vec<Value>) =
            entries[..at].iter().cloned().partition(is_log);
        moved.push(entries[at].clone());
        moved.extend(logs);
        moved.extend(entries[at + 1..].iter().cloned());
        entries = moved;
    }
    let mut ids: HashMap<String, String> = HashMap::new();
    let mut launches = Vec::new();
    let mut observations = Vec::new();
    for e in &entries {
        let lu = e["body"]["launchUuid"]
            .as_str()
            .map(str::to_string)
            .or_else(|| {
                e["multipart"].as_array().and_then(|p| {
                    p.iter()
                        .find_map(|x| x["json"][0]["launchUuid"].as_str().map(str::to_string))
                })
            });
        if let Some(lu) = lu
            && !ids.contains_key(&lu)
        {
            let l = s.launch(PROJECT, None).await;
            let id = l["id"].as_str().unwrap().to_string();
            ids.insert(lu, id.clone());
            launches.push(id);
        }
        let (st, ours) = send(s, token, e, &ids, opts).await;
        let at = format!("{} {} #{}", e["method"], e["path"], e["seq"]);
        assert_eq!(
            st.as_u16() as u64,
            e["response"]["status"].as_u64().unwrap(),
            "{name}: {at}: {ours}"
        );
        let recorded_id = e["response"]["body"]["id"].as_str();
        if let (Some(r), Some(o)) = (recorded_id, ours["id"].as_str()) {
            ids.insert(r.to_string(), o.to_string());
            if e["path"].as_str().unwrap().ends_with("/launch") && e["method"] == "POST" {
                launches.push(fp_launch_of(s, ours["number"].as_i64().unwrap()).await);
            }
        }
        if opts.duplicate && !is_log(e) {
            let post_item = e["method"] == "POST" && e["path"].as_str().unwrap().contains("/item");
            let put_item = e["method"] == "PUT" && e["path"].as_str().unwrap().contains("/item/");
            let client_uuid = e["body"]["uuid"].is_string();
            if (post_item && client_uuid) || put_item || is_launch_finish(e) {
                let (st2, again) = send(s, token, e, &ids, opts).await;
                if post_item {
                    if st2 != st || again["id"] != ours["id"] {
                        observations.push(format!(
                            "duplicate start {at}: {st2} {again}, first {st} {ours}"
                        ));
                    }
                } else if !st2.is_success() {
                    observations.push(format!("duplicate finish {at}: {st2} {again}"));
                }
            }
        }
    }
    launches.sort();
    launches.dedup();
    Replayed {
        launches,
        observations,
    }
}

async fn finish_leftovers(s: &Server, launches: &[String]) {
    for l in launches {
        if s.launch_get(PROJECT, l).await["status"] == "running" {
            let (st, _) = s
                .call(
                    Method::POST,
                    &format!("/api/v1/projects/{PROJECT}/launches/{l}/finish"),
                    Some(ADMIN_TOKEN),
                    None,
                )
                .await;
            assert_eq!(st, StatusCode::OK);
        }
    }
}

/// Waits until the launch is not running and its results stopped changing.
async fn settled_results(s: &Server, launch: &str) -> Vec<Value> {
    let deadline = Instant::now() + Duration::from_secs(90);
    let mut last = 0;
    let mut stable = 0;
    loop {
        let l = s.launch_get(PROJECT, launch).await;
        let r = s.results(PROJECT, launch, "").await;
        let calm = l["status"] != "running" && r.iter().all(|x| x["status"] != "running");
        if calm && r.len() == last {
            stable += 1;
            if stable >= 4 {
                return r;
            }
        } else {
            stable = 0;
        }
        last = r.len();
        assert!(
            Instant::now() < deadline,
            "launch {launch} did not settle: {l}"
        );
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
}

fn first_line(v: &Value, cut: usize) -> Value {
    v.as_str().map_or(Value::Null, |m| {
        json!(
            m.lines()
                .next()
                .unwrap_or("")
                .chars()
                .take(cut)
                .collect::<String>()
        )
    })
}

fn flatten_steps(steps: &Value, depth: usize, out: &mut Vec<String>) {
    for st in steps.as_array().into_iter().flatten() {
        out.push(format!(
            "{}{} [{}] {}{}",
            "  ".repeat(depth),
            st["name"].as_str().unwrap(),
            st["status"].as_str().unwrap(),
            st["kind"].as_str().unwrap(),
            st["attachments"]
                .as_array()
                .filter(|a| !a.is_empty())
                .map(|a| format!(" files={}", a.len()))
                .unwrap_or_default()
        ));
        flatten_steps(&st["steps"], depth + 1, out);
    }
}

/// What a launch looks like to a user, without ids and times.
async fn snapshot(s: &Server, launch: &str, results: &[Value]) -> Value {
    let l = s.launch_get(PROJECT, launch).await;
    let mut out = Vec::new();
    for r in results {
        let id = r["id"].as_str().unwrap();
        let (_, d) = s
            .get(
                &format!("/api/v1/projects/{PROJECT}/results/{id}"),
                ADMIN_TOKEN,
            )
            .await;
        if let Ok(n) = std::env::var("FASTPROOF_DEBUG_RESULT")
            && d["fullName"].as_str().is_some_and(|f| f.contains(&n))
        {
            eprintln!("{}", serde_json::to_string_pretty(&d).unwrap());
        }
        let (_, logs) = s
            .get(
                &format!(
                    "/api/v1/projects/{PROJECT}/launches/{launch}/logs?resultId={id}&limit=5000"
                ),
                ADMIN_TOKEN,
            )
            .await;
        let levels = {
            let mut m: std::collections::BTreeMap<String, u64> = Default::default();
            for line in logs["items"].as_array().into_iter().flatten() {
                *m.entry(line["level"].as_str().unwrap_or("?").to_string())
                    .or_default() += 1;
            }
            m
        };
        let mut attempts = Vec::new();
        for a in d["attempts"].as_array().unwrap() {
            let mut steps = Vec::new();
            flatten_steps(&a["steps"], 0, &mut steps);
            let mut files: Vec<String> = a["attachments"]
                .as_array()
                .unwrap()
                .iter()
                .map(|f| {
                    format!(
                        "{} {} {}",
                        f["name"].as_str().unwrap(),
                        f["kind"].as_str().unwrap(),
                        f["size"]
                    )
                })
                .collect();
            files.sort();
            attempts.push(json!({
                "no": a["no"],
                "status": a["status"],
                "error": first_line(&a["error"]["message"], 120),
                "steps": steps,
                "attachments": files,
            }));
        }
        let mut labels: Vec<String> = d["labels"]
            .as_array()
            .unwrap()
            .iter()
            .map(|l| {
                format!(
                    "{}={}",
                    l["name"].as_str().unwrap(),
                    l["value"].as_str().unwrap_or("")
                )
            })
            .collect();
        labels.sort();
        let mut links: Vec<String> = d["links"]
            .as_array()
            .unwrap()
            .iter()
            .map(|l| {
                format!(
                    "{} {}",
                    l["type"].as_str().unwrap_or(""),
                    l["url"].as_str().unwrap_or("")
                )
            })
            .collect();
        links.sort();
        out.push(json!({
            "name": d["name"],
            "fullName": d["fullName"],
            "status": d["status"],
            "message": first_line(&d["message"], 120),
            "flakyInRun": d["flakyInRun"],
            "suitePath": d["suitePath"],
            "labels": labels,
            "links": links,
            "parameters": d["parameters"].as_array().unwrap().iter().map(|p| format!("{}={}", p["name"].as_str().unwrap_or(""), p["value"].as_str().unwrap_or(""))).collect::<Vec<_>>(),
            "attempts": attempts,
            "logLevels": levels,
        }));
    }
    out.sort_by_key(|v| {
        (
            v["fullName"].as_str().unwrap_or("").to_string(),
            v["name"].as_str().unwrap_or("").to_string(),
            v["status"].as_str().unwrap_or("").to_string(),
            v["parameters"].to_string(),
            v["attempts"].to_string(),
        )
    });
    let mut lines = 0;
    let mut before: Option<String> = None;
    loop {
        let q = before
            .as_ref()
            .map(|b| format!("&before={b}"))
            .unwrap_or_default();
        let (_, page) = s
            .get(
                &format!("/api/v1/projects/{PROJECT}/launches/{launch}/logs?limit=5000{q}"),
                ADMIN_TOKEN,
            )
            .await;
        lines += page["items"].as_array().map_or(0, |a| a.len());
        match page["next"].as_str() {
            Some(n) => before = Some(n.to_string()),
            None => break,
        }
    }
    unsalt(&json!({
        "launch": {"status": l["status"], "counts": l["counts"], "logLines": lines},
        "results": out,
    }))
}

/// Puts the replay prefix of recorded uuids back to the recorded one.
fn unsalt(v: &Value) -> Value {
    let text = v.to_string();
    let b = text.as_bytes();
    let tail = b"-0000-4000-8000-";
    let mut out: Vec<u8> = Vec::with_capacity(text.len());
    let mut i = 0;
    while i < b.len() {
        if i + 8 + tail.len() <= b.len()
            && &b[i + 8..i + 8 + tail.len()] == tail
            && b[i..i + 8].iter().all(u8::is_ascii_hexdigit)
        {
            out.extend_from_slice(b"00000000");
            i += 8;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    serde_json::from_slice(&out).unwrap()
}

fn check_snapshot(name: &str, got: &Value) {
    let path = snapshots_dir().join(format!("{name}.json"));
    let text = serde_json::to_string_pretty(got).unwrap() + "\n";
    if std::env::var("FASTPROOF_UPDATE_SNAPSHOTS").is_ok() {
        std::fs::create_dir_all(snapshots_dir()).unwrap();
        std::fs::write(&path, text).unwrap();
        return;
    }
    let want = std::fs::read_to_string(&path).unwrap_or_else(|_| {
        panic!(
            "no snapshot {}: run with FASTPROOF_UPDATE_SNAPSHOTS=1",
            path.display()
        )
    });
    if want != text {
        let actual = std::env::temp_dir().join(format!("rp-snapshot-{name}.actual.json"));
        std::fs::write(&actual, &text).unwrap();
        panic!(
            "snapshot {name} differs; compare {} with {}",
            path.display(),
            actual.display()
        );
    }
}

async fn run_snapshot(s: &Server, token: &str, name: &str, opts: &Opts) -> (Value, Replayed) {
    let r = replay(s, token, name, opts).await;
    assert_eq!(r.launches.len(), 1, "{name}: launches {:?}", r.launches);
    finish_leftovers(s, &r.launches).await;
    let results = settled_results(s, &r.launches[0]).await;
    let snap = snapshot(s, &r.launches[0], &results).await;
    (snap, r)
}

async fn setup(extra: &[(&str, &str)]) -> Option<(TestDb, Server, String)> {
    let db = TestDb::new().await?;
    let mut vars: Vec<(&str, &str)> = FAST.to_vec();
    vars.extend_from_slice(extra);
    let s = Server::start_with(&db, &vars).await;
    s.project(PROJECT).await;
    let token = s.token("ci", "uploader", &[PROJECT]).await;
    Some((db, s, token))
}

fn recorded_log_lines(name: &str) -> u64 {
    load(name)
        .iter()
        .filter(|e| e["injected"] != true && is_log(e))
        .map(|e| match e["multipart"].as_array() {
            Some(parts) => parts
                .iter()
                .filter_map(|p| p["json"].as_array().map(|a| a.len() as u64))
                .sum(),
            None => 1,
        })
        .sum()
}

async fn snapshots_for(extra: &[(&str, &str)], names: &[String]) {
    let Some((_db, s, token)) = setup(extra).await else {
        return;
    };
    for name in names {
        let (snap, _) = run_snapshot(&s, &token, name, &Opts::default()).await;
        let total = snap["launch"]["counts"]["total"].as_i64().unwrap();
        assert!(total > 0, "{name}: no results");
        assert_eq!(
            snap["launch"]["logLines"].as_u64().unwrap(),
            recorded_log_lines(name),
            "{name}: log lines lost or invented"
        );
        check_snapshot(name, &snap);
    }
}

#[tokio::test]
async fn recordings_match_their_snapshots_direct() {
    snapshots_for(&[], &recordings()).await;
}

#[tokio::test]
async fn recordings_match_their_snapshots_kafka() {
    let Ok(brokers) = std::env::var("FASTPROOF_TEST_KAFKA_BROKERS") else {
        eprintln!("FASTPROOF_TEST_KAFKA_BROKERS is not set: skipped");
        return;
    };
    let prefix = format!("fptest{:08x}.", rand::random::<u32>());
    snapshots_for(
        &[
            ("FASTPROOF_INGEST_MODE", "kafka"),
            ("FASTPROOF_KAFKA_BROKERS", &brokers),
            ("FASTPROOF_KAFKA_TOPIC_PREFIX", &prefix),
            ("FASTPROOF_KAFKA_CREATE_TOPICS", "true"),
            ("FASTPROOF_KAFKA_PARTITIONS", "4"),
        ],
        &recordings(),
    )
    .await;
}

/// A retry after a lost response must not change the outcome.
#[tokio::test]
async fn duplicated_requests_change_nothing() {
    let Some((_db, s, token)) = setup(&[]).await else {
        return;
    };
    let mut seen = Vec::new();
    for name in [
        "playwright-v2-steps",
        "playwright-v1-steps",
        "testng-v2-async",
        "pytest-plain",
        "robot-deep",
    ] {
        let (snap, r) = run_snapshot(
            &s,
            &token,
            name,
            &Opts {
                duplicate: true,
                ..Opts::default()
            },
        )
        .await;
        seen.extend(r.observations.iter().map(|o| format!("{name}: {o}")));
        check_snapshot(name, &snap);
    }
    assert!(
        seen.is_empty(),
        "duplicates handled differently:\n{}",
        seen.join("\n")
    );
}

/// Every log request arrives after the launch finish (TestNG async sends its last batch so):
/// all lines are kept and every failure still gets its message, because a failed result
/// without an error waits `FASTPROOF_RP_ERROR_WAIT_MS` for its ERROR line.
#[tokio::test]
async fn logs_after_the_launch_finish_are_kept() {
    let Some((_db, s, token)) = setup(&[]).await else {
        return;
    };
    let mut problems = Vec::new();
    for name in [
        "testng-v2-async",
        "testng-sync",
        "pytest-plain",
        "robot-deep",
        "playwright-v2-steps",
    ] {
        let (snap, _) = run_snapshot(
            &s,
            &token,
            name,
            &Opts {
                logs_after_finish: true,
                ..Opts::default()
            },
        )
        .await;
        let want: Value = serde_json::from_str(
            &std::fs::read_to_string(snapshots_dir().join(format!("{name}.json"))).unwrap(),
        )
        .unwrap();
        if snap["launch"]["counts"] != want["launch"]["counts"] {
            problems.push(format!("{name}: counts differ"));
        }
        let lost = snap["results"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|r| r["status"] == "failed" && r["message"].is_null())
            .count();
        if lost > 0 {
            problems.push(format!("{name}: {lost} failures lost their message"));
        }
        if snap["launch"]["logLines"] != want["launch"]["logLines"] {
            problems.push(format!(
                "{name}: log lines {} instead of {}",
                snap["launch"]["logLines"], want["launch"]["logLines"]
            ));
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

/// An agent that died: no launch finish, the idle timeout interrupts the launch.
#[tokio::test]
async fn a_launch_without_finish_is_interrupted_by_the_idle_timeout() {
    let Some((db, s, token)) = setup(&[]).await else {
        return;
    };
    let r = replay(
        &s,
        &token,
        "pytest-plain",
        &Opts {
            no_launch_finish: true,
            ..Opts::default()
        },
    )
    .await;
    let l = &r.launches[0];
    assert_eq!(s.launch_get(PROJECT, l).await["status"], "running");
    sqlx::query("update rp_launches set last_call_at = now() - interval '2 hours' where launch_id = $1::uuid")
        .bind(l)
        .execute(&db.pool)
        .await
        .unwrap();
    // Through the server's own `rp.maintain` job (with the default idle timeout of an hour).
    db.run_scheduled("rp.maintain").await;
    let results = settled_results(&s, l).await;
    assert_eq!(results.len(), 19);
    assert_eq!(s.launch_get(PROJECT, l).await["status"], "interrupted");
}

/// A file named in the JSON part but missing from the request fails that entry only.
#[tokio::test]
async fn missing_files_in_multipart_do_not_lose_the_rest() {
    let Some((_db, s, token)) = setup(&[]).await else {
        return;
    };
    for name in ["playwright-v2-steps", "testng-v2-async", "pytest-plain"] {
        let r = replay(
            &s,
            &token,
            name,
            &Opts {
                drop_files: true,
                ..Opts::default()
            },
        )
        .await;
        finish_leftovers(&s, &r.launches).await;
        let results = settled_results(&s, &r.launches[0]).await;
        let want: Value = serde_json::from_str(
            &std::fs::read_to_string(snapshots_dir().join(format!("{name}.json"))).unwrap(),
        )
        .unwrap();
        assert_eq!(
            results.len(),
            want["results"].as_array().unwrap().len(),
            "{name}"
        );
        let snap = snapshot(&s, &r.launches[0], &results).await;
        let files: usize = snap["results"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| {
                r["attempts"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|a| a["attachments"].as_array().unwrap().len())
                    .sum::<usize>()
            })
            .sum();
        assert_eq!(files, 0, "{name}: files without bytes were stored");
        assert_eq!(snap["launch"]["counts"], want["launch"]["counts"], "{name}");
    }
}

/// The recordings stay free of secrets and local details.
#[test]
fn recordings_are_sanitized() {
    for name in recordings() {
        let text =
            std::fs::read_to_string(recordings_dir().join(format!("{name}.ndjson"))).unwrap();
        for bad in [
            "Bearer ",
            "fp_example",
            "/home/node",
            "claude-1000",
            "scratchpad",
        ] {
            assert!(!text.contains(bad), "{name} contains {bad}");
        }
        for line in text.lines() {
            let e: Value = serde_json::from_str(line).unwrap();
            assert!(
                e.get("headers")
                    .is_some_and(|h| h.get("authorization").is_none())
            );
        }
    }
    assert!(Path::new(&snapshots_dir()).parent().is_some());
}

async fn rp_call(
    s: &Server,
    method: Method,
    path: &str,
    token: &str,
    body: Value,
) -> (StatusCode, Value) {
    s.call(method, &format!("/rp/api/{path}"), Some(token), Some(body))
        .await
}

/// An item uuid seen in another launch of the project must not touch the first launch's result.
#[tokio::test]
async fn an_item_uuid_reused_in_another_launch_does_not_merge_results() {
    let Some((_db, s, token)) = setup(&[]).await else {
        return;
    };
    let mut fp = Vec::new();
    for n in 0..2 {
        let (st, l) = rp_call(
            &s,
            Method::POST,
            &format!("v2/{PROJECT}/launch"),
            &token,
            json!({"name": format!("l{n}")}),
        )
        .await;
        assert_eq!(st, StatusCode::CREATED, "{l}");
        let lu = l["id"].as_str().unwrap().to_string();
        let (st, b) = rp_call(
            &s,
            Method::POST,
            &format!("v2/{PROJECT}/item"),
            &token,
            json!({"uuid": "reused-item-uuid", "launchUuid": lu, "name": "t", "type": "STEP", "codeRef": "a.py:t"}),
        )
        .await;
        if n == 0 {
            assert_eq!(st, StatusCode::CREATED, "{b}");
            let (st, b) = rp_call(
                &s,
                Method::PUT,
                &format!("v2/{PROJECT}/item/reused-item-uuid"),
                &token,
                json!({"launchUuid": lu, "status": "PASSED"}),
            )
            .await;
            assert_eq!(st, StatusCode::OK, "{b}");
        } else {
            assert_eq!(
                st,
                StatusCode::CONFLICT,
                "the second launch may not take over the uuid: {st} {b}"
            );
        }
        let (st, _) = rp_call(
            &s,
            Method::PUT,
            &format!("v2/{PROJECT}/launch/{lu}/finish"),
            &token,
            json!({}),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        fp.push(fp_launch_of(&s, l["number"].as_i64().unwrap()).await);
    }
    let results = settled_results(&s, &fp[0]).await;
    assert_eq!(results.len(), 1);
    assert_eq!(results[0]["attemptCount"], 1);
    assert_eq!(s.results(PROJECT, &fp[1], "").await.len(), 0);
}
