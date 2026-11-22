//! M5: `trace_actions` reads trace lines with a bounded buffer. A trace whose `.trace` entry is
//! one 64 MiB line (about 64 KiB compressed) must not make the call allocate the line. Its own
//! test binary: the global allocator counts the bytes of every thread.

mod common;

use std::alloc::{GlobalAlloc, Layout, System};
use std::io::Write;
use std::sync::atomic::{AtomicUsize, Ordering};

use common::{ADMIN_TOKEN, Server, TestDb};
use reqwest::StatusCode;
use serde_json::{Value, json};

struct Counting;

static CURRENT: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        let p = unsafe { System.alloc(l) };
        if !p.is_null() {
            let now = CURRENT.fetch_add(l.size(), Ordering::Relaxed) + l.size();
            PEAK.fetch_max(now, Ordering::Relaxed);
        }
        p
    }

    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        unsafe { System.dealloc(p, l) };
        CURRENT.fetch_sub(l.size(), Ordering::Relaxed);
    }

    unsafe fn realloc(&self, p: *mut u8, l: Layout, new: usize) -> *mut u8 {
        let q = unsafe { System.realloc(p, l, new) };
        if !q.is_null() {
            if new >= l.size() {
                let now = CURRENT.fetch_add(new - l.size(), Ordering::Relaxed) + new - l.size();
                PEAK.fetch_max(now, Ordering::Relaxed);
            } else {
                CURRENT.fetch_sub(l.size() - new, Ordering::Relaxed);
            }
        }
        q
    }
}

#[global_allocator]
static ALLOC: Counting = Counting;

fn zip_of(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut buf = std::io::Cursor::new(Vec::new());
    {
        let mut z = zip::ZipWriter::new(&mut buf);
        let o = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        for (name, bytes) in entries {
            z.start_file(*name, o).unwrap();
            z.write_all(bytes).unwrap();
        }
        z.finish().unwrap();
    }
    buf.into_inner()
}

#[tokio::test]
async fn trace_actions_read_long_lines_in_bounded_memory() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("mem").await;
    let trace = {
        let mut t = Vec::with_capacity(64 * 1024 * 1024 + 200);
        t.extend_from_slice(b"{\"type\":\"before\",\"callId\":\"c@1\",\"class\":\"Frame\",\"method\":\"goto\",\"x\":\"");
        t.resize(64 * 1024 * 1024, b'a');
        t.extend_from_slice(b"\"}\n{\"type\":\"before\",\"callId\":\"c@2\",\"class\":\"Frame\",\"method\":\"click\",\"params\":{\"selector\":\"#buy\"}}\n");
        zip_of(&[("0-trace.trace", &t)])
    };
    let result = json!({"uuid": "r1", "historyId": "h1", "name": "buys", "fullName": "shop.Buy#buys",
        "status": "passed", "start": 1795249075967i64, "stop": 1795249076967i64,
        "attachments": [{"name": "trace", "source": "r1-trace.zip", "type": "application/vnd.allure.playwright-trace"}]});
    let results = zip_of(&[
        ("r1-result.json", result.to_string().as_bytes()),
        ("r1-trace.zip", &trace),
    ]);
    drop(trace);
    let (st, l) = s
        .post(
            "/api/v1/projects/mem/launches",
            ADMIN_TOKEN,
            json!({"name": "m"}),
        )
        .await;
    assert!(st.is_success(), "{l}");
    let lid = l["id"].as_str().unwrap().to_string();
    let u = s.ingest("mem", &lid, results, "").await;
    assert_eq!(u["status"], "done", "{u}");
    let rid = s.results("mem", &lid, "").await[0]["id"]
        .as_str()
        .unwrap()
        .to_string();

    let body = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": "trace_actions",
        "arguments": {"project": "mem", "resultId": rid}}});
    let base = CURRENT.load(Ordering::Relaxed);
    PEAK.store(base, Ordering::Relaxed);
    let r = s
        .http
        .post(format!("{}/mcp", s.base))
        .header("accept", "application/json, text/event-stream")
        .header("content-type", "application/json")
        .header("mcp-protocol-version", "2025-06-18")
        .bearer_auth(ADMIN_TOKEN)
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let text = r.text().await.unwrap();
    let grown = PEAK.load(Ordering::Relaxed).saturating_sub(base);
    let data = text
        .lines()
        .find_map(|l| l.strip_prefix("data:"))
        .unwrap_or(&text)
        .trim()
        .to_string();
    let v: Value = serde_json::from_str(&data).unwrap();
    let out: Value =
        serde_json::from_str(v["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(out["actions"][0]["apiName"], "Frame.click", "{out}");
    assert!(
        grown < 24 * 1024 * 1024,
        "the call allocated {} MiB at its peak",
        grown / (1024 * 1024)
    );
}
