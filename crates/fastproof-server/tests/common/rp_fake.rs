//! The fake ReportPortal read API of the `fastproof rp import` tests (moved from
//! `rp_import.rs` unchanged; included with `#[path]` by the tests that use it): launches, items
//! and logs with a page size of at most 2, files, the bearer key, injected 429 and 503 faults.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::extract::{Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

pub const RP_KEY: &str = "rp-secret-key";
#[derive(Default)]
pub struct Fake {
    pub launches: Vec<Value>,
    pub debug_launches: Vec<Value>,
    pub items: HashMap<i64, Vec<Value>>,
    pub logs: Vec<Value>,
    pub files: HashMap<String, (String, Vec<u8>, Option<String>)>,
    pub hits: Mutex<Vec<String>>,
    pub faults: Mutex<Vec<String>>,
}

pub fn page(all: Vec<Value>, q: &HashMap<String, String>) -> Value {
    let size = q
        .get("page.size")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(20)
        .min(2);
    let n = q
        .get("page.page")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(1);
    let total = all.len().div_ceil(size);
    let content: Vec<Value> = all.into_iter().skip((n - 1) * size).take(size).collect();
    json!({"content": content, "page": {"number": n, "size": size, "totalPages": total}})
}

pub async fn handle(State(f): State<Arc<Fake>>, req: Request) -> Response {
    let path = req.uri().path().to_string();
    let q: HashMap<String, String> =
        url::form_urlencoded::parse(req.uri().query().unwrap_or("").as_bytes())
            .into_owned()
            .collect();
    f.hits.lock().unwrap().push(path.clone());
    let auth = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if auth != format!("Bearer {RP_KEY}") {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"message": "bad key"})),
        )
            .into_response();
    }
    {
        let mut faults = f.faults.lock().unwrap();
        if let Some(at) = faults.iter().position(|p| path.contains(p.as_str())) {
            let which = faults.remove(at);
            return if which == "launch" {
                let mut h = HeaderMap::new();
                h.insert("retry-after", HeaderValue::from_static("0"));
                (StatusCode::TOO_MANY_REQUESTS, h, "slow down").into_response()
            } else {
                (StatusCode::SERVICE_UNAVAILABLE, "try later").into_response()
            };
        }
    }
    let ok = |v: Value| (StatusCode::OK, Json(v)).into_response();
    if path == "/api/info" {
        return ok(json!({"build": {"version": "5.14.1"}}));
    }
    if path.ends_with("/settings") {
        return ok(json!({"subTypes": {"PRODUCT_BUG": [
            {"locator": "pb001", "typeRef": "PRODUCT_BUG", "longName": "Product Bug"},
            {"locator": "pb_ab12", "typeRef": "PRODUCT_BUG", "longName": "Flaky infra"}]}}));
    }
    if path.ends_with("/launch/mode") {
        return ok(page(f.debug_launches.clone(), &q));
    }
    if path.ends_with("/launch") {
        let since = q
            .get("filter.gte.startTime")
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(0);
        let mut v: Vec<Value> = f
            .launches
            .iter()
            .filter(|l| l["startTime"].as_i64().unwrap() >= since)
            .filter(|l| q.get("filter.eq.name").is_none_or(|n| l["name"] == *n))
            .cloned()
            .collect();
        v.sort_by_key(|l| l["startTime"].as_i64().unwrap());
        return ok(page(v, &q));
    }
    if path.ends_with("/item") {
        let id: i64 = q["filter.eq.launchId"].parse().unwrap();
        return ok(page(f.items.get(&id).cloned().unwrap_or_default(), &q));
    }
    if path.ends_with("/log") {
        let ids: Vec<i64> = q["filter.in.item"]
            .split(',')
            .map(|x| x.parse().unwrap())
            .collect();
        let v: Vec<Value> = f
            .logs
            .iter()
            .filter(|l| ids.contains(&l["itemId"].as_i64().unwrap()))
            .cloned()
            .collect();
        return ok(page(v, &q));
    }
    if let Some(rest) = path.strip_prefix("/api/v1/data/") {
        let id = rest.rsplit('/').next().unwrap();
        return match f.files.get(id) {
            Some((ct, bytes, name)) => {
                let mut h = HeaderMap::new();
                h.insert(header::CONTENT_TYPE, ct.parse().unwrap());
                if let Some(n) = name {
                    h.insert(
                        header::CONTENT_DISPOSITION,
                        format!("attachment; filename=\"{n}\"").parse().unwrap(),
                    );
                }
                (StatusCode::OK, h, bytes.clone()).into_response()
            }
            None => StatusCode::NOT_FOUND.into_response(),
        };
    }
    StatusCode::NOT_FOUND.into_response()
}

use axum::Json;

pub async fn start_fake(f: Fake) -> (String, Arc<Fake>) {
    let f = Arc::new(f);
    let app = axum::Router::new().fallback(handle).with_state(f.clone());
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    (format!("http://{addr}"), f)
}
