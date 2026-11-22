//! Allure TestOps API client of the case import (beta). TestOps publishes no API reference
//! outside its instances (`/swagger-ui.html`); the paths follow the official utilities
//! (github.com/eroshenkoam/allure-testops-utils): the API token becomes a JWT at
//! `POST /api/uaa/oauth/token` (`grant_type=apitoken`), then `testcase/__search` (pages),
//! `testcase/{id}`, `…/step` (normalised scenario, or the older `…/scenario`), `…/cfv`,
//! `…/link`, `…/issue`, `testcase/attachment?testCaseId` and `…/attachment/{id}/content`.
//! Paths start with `/api/rs`; when the first search answers 404, `/api` is used instead
//! (newer versions, not confirmed).
//!
//! Every request goes through the egress client (`FASTPROOF_EGRESS_ALLOW`), only to the
//! origin of the base URL (`https`, `http` only with the explicit flag), redirects to another
//! origin are refused, each request has a 30 s timeout and a 16 MiB response cap, and at most
//! 10 000 search pages are read. The token and the JWT never appear in errors or logs.

use std::time::Duration;

use fastproof_tm_import::{
    ImportedAttachment, ImportedCase, ImportedKey, ImportedLink, ImportedStep, ImportedValue,
    map_status, rich,
};
use reqwest::Method;
use serde_json::Value;

use crate::integrations::egress::{Egress, OutResponse, OutboundError};

/// Largest response.
pub const MAX_RESPONSE_BYTES: usize = 16 * 1024 * 1024;
/// Search pages read, at most.
pub const MAX_PAGES: usize = 10_000;
/// Timeout of one request.
pub const TIMEOUT: Duration = Duration::from_secs(30);
/// Cases per search page.
pub const PAGE_SIZE: usize = 100;
/// Same-origin redirects followed.
const MAX_REDIRECTS: usize = 3;

/// A failed TestOps call (no URL, no token).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestOpsError(pub String);

impl std::fmt::Display for TestOpsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for TestOpsError {}

impl From<OutboundError> for TestOpsError {
    fn from(e: OutboundError) -> Self {
        TestOpsError(format!("Allure TestOps: {e}"))
    }
}

/// Checks a base URL: `https` (or `http` with `allow_http`), a host, no user info, query or
/// fragment. Returns it without a trailing slash.
pub fn check_base_url(u: &str, allow_http: bool) -> Result<url::Url, String> {
    let mut p = url::Url::parse(u.trim()).map_err(|_| "baseUrl is not a URL".to_string())?;
    match p.scheme() {
        "https" => {}
        "http" if allow_http => {}
        "http" => {
            return Err(
                "baseUrl must be https (http needs FASTPROOF_TESTOPS_ALLOW_HTTP=true)".into(),
            );
        }
        _ => return Err("baseUrl must be an https URL".into()),
    }
    if p.host_str().is_none() || !p.username().is_empty() || p.password().is_some() {
        return Err("baseUrl must have a host and no user info".into());
    }
    if p.query().is_some() || p.fragment().is_some() {
        return Err("baseUrl must not have a query or fragment".into());
    }
    let path = p.path().trim_end_matches('/').to_string();
    p.set_path(&path);
    Ok(p)
}

/// A connected client (holds the JWT).
pub struct TestOps<'a> {
    egress: &'a Egress,
    base: url::Url,
    jwt: String,
    prefix: &'static str,
    /// Bytes received so far (charged against the import budget by the caller).
    pub received: u64,
}

fn text(v: &Value, k: &str) -> String {
    match v.get(k) {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        _ => String::new(),
    }
}

fn name_of(v: &Value, k: &str) -> String {
    match v.get(k) {
        Some(Value::String(s)) => s.clone(),
        Some(o @ Value::Object(_)) => text(o, "name"),
        _ => String::new(),
    }
}

impl<'a> TestOps<'a> {
    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base.as_str().trim_end_matches('/'), path)
    }

    async fn send(
        &mut self,
        method: Method,
        path: &str,
        body: Option<(String, &'static str)>,
        auth: bool,
        max: usize,
    ) -> Result<OutResponse, TestOpsError> {
        let mut url = self.url(path);
        for _ in 0..=MAX_REDIRECTS {
            let mut rb = self.egress.request(method.clone(), &url)?.timeout(TIMEOUT);
            if auth {
                rb = rb.bearer_auth(&self.jwt);
            }
            if let Some((b, ct)) = &body {
                rb = rb.header("content-type", *ct).body(b.clone());
            }
            rb = rb.header("accept", "application/json");
            let r = self.egress.send_no_redirect(rb, max).await?;
            self.received += r.body.len() as u64;
            if (300..400).contains(&r.status) {
                let loc = r
                    .headers
                    .get("location")
                    .and_then(|l| l.to_str().ok())
                    .ok_or_else(|| {
                        TestOpsError("Allure TestOps: a redirect without a location".into())
                    })?;
                let cur = url::Url::parse(&url)
                    .map_err(|_| TestOpsError("Allure TestOps: bad URL".into()))?;
                let next = cur.join(loc).map_err(|_| {
                    TestOpsError("Allure TestOps: the redirect location is not a URL".into())
                })?;
                if next.origin() != self.base.origin() {
                    return Err(TestOpsError(
                        "Allure TestOps: a redirect to another host was refused".into(),
                    ));
                }
                url = next.to_string();
                continue;
            }
            return Ok(r);
        }
        Err(TestOpsError("Allure TestOps: too many redirects".into()))
    }

    /// Exchanges the API token for a JWT.
    pub async fn connect(
        egress: &'a Egress,
        base: url::Url,
        token: &str,
    ) -> Result<Self, TestOpsError> {
        let mut c = Self {
            egress,
            base,
            jwt: String::new(),
            prefix: "/api/rs",
            received: 0,
        };
        let form = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("grant_type", "apitoken")
            .append_pair("scope", "openid")
            .append_pair("token", token)
            .finish();
        let r = c
            .send(
                Method::POST,
                "/api/uaa/oauth/token",
                Some((form, "application/x-www-form-urlencoded")),
                false,
                1024 * 1024,
            )
            .await?;
        if r.status != 200 {
            return Err(TestOpsError(format!(
                "Allure TestOps refused the token (HTTP {})",
                r.status
            )));
        }
        let v: Value = r.json()?;
        c.jwt = v
            .get("access_token")
            .and_then(Value::as_str)
            .filter(|t| !t.is_empty() && t.len() < 16 * 1024)
            .ok_or_else(|| TestOpsError("Allure TestOps: no access_token in the answer".into()))?
            .to_string();
        Ok(c)
    }

    async fn get(&mut self, path: &str) -> Result<Option<Value>, TestOpsError> {
        let full = format!("{}{}", self.prefix, path);
        let r = self
            .send(Method::GET, &full, None, true, MAX_RESPONSE_BYTES)
            .await?;
        match r.status {
            200 => Ok(Some(r.json()?)),
            404 => Ok(None),
            401 | 403 => Err(TestOpsError(format!(
                "Allure TestOps denied access (HTTP {})",
                r.status
            ))),
            s => Err(TestOpsError(format!("Allure TestOps answered HTTP {s}"))),
        }
    }

    /// One search page: case ids and whether more pages follow. The first call falls back to
    /// `/api` when `/api/rs` is not there.
    pub async fn search(
        &mut self,
        project: i64,
        page: usize,
    ) -> Result<(Vec<i64>, bool), TestOpsError> {
        if page >= MAX_PAGES {
            return Err(TestOpsError(format!(
                "Allure TestOps: more than {MAX_PAGES} pages"
            )));
        }
        let q = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("projectId", &project.to_string())
            .append_pair("rql", "id > 0")
            .append_pair("page", &page.to_string())
            .append_pair("size", &PAGE_SIZE.to_string())
            .finish();
        let path = format!("/testcase/__search?{q}");
        let mut v = self.get(&path).await?;
        if v.is_none() && page == 0 && self.prefix == "/api/rs" {
            self.prefix = "/api";
            v = self.get(&path).await?;
        }
        let v = v.ok_or_else(|| {
            TestOpsError("Allure TestOps: the test case search is not there (HTTP 404)".into())
        })?;
        let content = v
            .get("content")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let ids: Vec<i64> = content
            .iter()
            .filter_map(|c| c.get("id").and_then(Value::as_i64))
            .collect();
        let last = v.get("last").and_then(Value::as_bool).unwrap_or_else(|| {
            let total = v.get("totalPages").and_then(Value::as_u64).unwrap_or(0) as usize;
            page + 1 >= total
        });
        let more = !last && !ids.is_empty();
        Ok((ids, more))
    }

    /// One case with its scenario, custom field values, links, issues and attachment list.
    pub async fn case(&mut self, id: i64) -> Result<Option<ImportedCase>, TestOpsError> {
        let Some(v) = self.get(&format!("/testcase/{id}")).await? else {
            return Ok(None);
        };
        let mut c = ImportedCase {
            keys: vec![ImportedKey {
                kind: "testops".into(),
                value: id.to_string(),
            }],
            number: Some(id).filter(|n| *n > 0),
            name: text(&v, "name"),
            description: rich(&text(&v, "description")),
            preconditions: rich(&text(&v, "precondition")),
            automated: v.get("automated").and_then(Value::as_bool).unwrap_or(false),
            status: map_status(&name_of(&v, "status")),
            ..Default::default()
        };
        let full = text(&v, "fullName");
        if c.automated && !full.is_empty() {
            c.full_names.push(full);
        }
        let layer = name_of(&v, "layer");
        if !layer.is_empty() {
            c.fields.insert("layer".into(), ImportedValue::Text(layer));
        }
        if let Some(tags) = v.get("tags").and_then(Value::as_array) {
            c.tags = tags
                .iter()
                .take(1000)
                .map(|t| match t {
                    Value::String(s) => s.clone(),
                    o => text(o, "name"),
                })
                .filter(|t| !t.trim().is_empty())
                .collect();
        }
        let scenario = match self.get(&format!("/testcase/{id}/step")).await? {
            Some(s) => Some(s),
            None => self.get(&format!("/testcase/{id}/scenario")).await?,
        };
        if let Some(s) = scenario {
            c.steps = scenario_steps(&s, &mut c.attachments);
        }
        let expected = rich(&text(&v, "expectedResult"));
        if !expected.is_empty() {
            match c.steps.last_mut() {
                Some(s) if s.expected.is_empty() => s.expected = expected,
                _ => {
                    c.description = [
                        c.description.clone(),
                        format!("**Expected result**\n\n{expected}"),
                    ]
                    .into_iter()
                    .filter(|s| !s.is_empty())
                    .collect::<Vec<_>>()
                    .join("\n\n")
                }
            }
        }
        if let Some(Value::Array(cfv)) = self.get(&format!("/testcase/{id}/cfv")).await? {
            for x in cfv.iter().take(1000) {
                let field = x
                    .get("customField")
                    .map(|f| text(f, "name"))
                    .unwrap_or_default();
                let value = text(x, "name");
                if field.is_empty() || value.is_empty() {
                    continue;
                }
                match c.fields.remove(&field) {
                    Some(ImportedValue::Text(prev)) => {
                        c.fields
                            .insert(field, ImportedValue::Many(vec![prev, value]));
                    }
                    Some(ImportedValue::Many(mut m)) => {
                        m.push(value);
                        c.fields.insert(field, ImportedValue::Many(m));
                    }
                    _ => {
                        c.fields.insert(field, ImportedValue::Text(value));
                    }
                }
            }
        }
        for (path, kind) in [("link", "link"), ("issue", "issue")] {
            if let Some(Value::Array(links)) = self.get(&format!("/testcase/{id}/{path}")).await? {
                for l in links.iter().take(100) {
                    let url = text(l, "url");
                    if url.starts_with("https://") || url.starts_with("http://") {
                        c.links.push(ImportedLink {
                            name: text(l, "name"),
                            url,
                            kind: kind.into(),
                        });
                    }
                }
            }
        }
        let q = format!("/testcase/attachment?testCaseId={id}&page=0&size=100");
        if let Some(page) = self.get(&q).await? {
            let list = page
                .get("content")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            for a in list.iter().take(100) {
                let aid = text(a, "id");
                if aid.is_empty() {
                    continue;
                }
                let source = format!("testops:{aid}");
                if !c.attachments.iter().any(|x| x.source == source) {
                    c.attachments.push(ImportedAttachment {
                        name: text(a, "name"),
                        source,
                        old_id: None,
                    });
                }
            }
        }
        Ok(Some(c))
    }

    /// The content of an attachment, at most `max` bytes.
    pub async fn attachment(
        &mut self,
        id: &str,
        max: usize,
    ) -> Result<Option<bytes::Bytes>, TestOpsError> {
        if !id.bytes().all(|b| b.is_ascii_digit()) || id.is_empty() || id.len() > 19 {
            return Ok(None);
        }
        let mut full = format!("{}/testcase/attachment/{id}/content", self.prefix);
        let mut r = self.send(Method::GET, &full, None, true, max).await?;
        if r.status == 404 && self.prefix == "/api/rs" {
            full = format!("/api/testcase/attachment/{id}/content");
            r = self.send(Method::GET, &full, None, true, max).await?;
            if r.status == 200 {
                self.prefix = "/api";
            }
        }
        match r.status {
            200 => Ok(Some(r.body)),
            404 => Ok(None),
            s => Err(TestOpsError(format!(
                "Allure TestOps answered HTTP {s} for an attachment"
            ))),
        }
    }
}

/// Steps of a scenario: the normalised form `{root: {children}, scenarioSteps: {id: {body,
/// children, expectedResult?, expectedResultId?, attachmentId?}}, attachments: {id: {name}}}`
/// or the older tree `{steps: [{name, steps, expectedResult, attachments}]}`.
pub fn scenario_steps(s: &Value, attachments: &mut Vec<ImportedAttachment>) -> Vec<ImportedStep> {
    if let (Some(root), Some(all)) = (
        s.get("root"),
        s.get("scenarioSteps").and_then(Value::as_object),
    ) {
        let names = s.get("attachments").and_then(Value::as_object);
        let mut budget = 10_000usize;
        let mut loose = Vec::new();
        return normalised(root, all, names, 1, &mut budget, attachments, &mut loose);
    }
    tree(s.get("steps").unwrap_or(&Value::Null), 1, attachments)
}

fn child_ids(v: &Value) -> Vec<String> {
    v.get("children")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .map(|x| match x {
                    Value::Number(n) => n.to_string(),
                    Value::String(s) => s.clone(),
                    _ => String::new(),
                })
                .collect()
        })
        .unwrap_or_default()
}

fn body_text(all: &serde_json::Map<String, Value>, id: &str, depth: usize) -> String {
    let Some(v) = all.get(id) else {
        return String::new();
    };
    let mut parts = vec![text(v, "body")];
    if depth < 8 {
        for c in child_ids(v) {
            parts.push(body_text(all, &c, depth + 1));
        }
    }
    parts.retain(|p| !p.is_empty());
    parts.join("\n")
}

fn normalised(
    node: &Value,
    all: &serde_json::Map<String, Value>,
    names: Option<&serde_json::Map<String, Value>>,
    depth: usize,
    budget: &mut usize,
    attachments: &mut Vec<ImportedAttachment>,
    loose: &mut Vec<String>,
) -> Vec<ImportedStep> {
    let mut out: Vec<ImportedStep> = Vec::new();
    for id in child_ids(node) {
        if *budget == 0 || depth > 16 {
            break;
        }
        *budget -= 1;
        let Some(v) = all.get(&id) else { continue };
        if let Some(a) = v.get("attachmentId").filter(|a| !a.is_null()) {
            let aid = match a {
                Value::Number(n) => n.to_string(),
                Value::String(s) => s.clone(),
                _ => continue,
            };
            let name = names
                .and_then(|n| n.get(&aid))
                .map(|x| text(x, "name"))
                .unwrap_or_default();
            let source = format!("testops:{aid}");
            match out.last_mut() {
                Some(last) => last.attachments.push(source.clone()),
                None => loose.push(source.clone()),
            }
            if !attachments.iter().any(|x| x.source == source) {
                attachments.push(ImportedAttachment {
                    name,
                    source,
                    old_id: None,
                });
            }
            continue;
        }
        let mut expected = text(v, "expectedResult");
        if let Some(e) = v.get("expectedResultId").filter(|e| !e.is_null()) {
            let eid = match e {
                Value::Number(n) => n.to_string(),
                Value::String(s) => s.clone(),
                _ => String::new(),
            };
            expected = body_text(all, &eid, 0);
        }
        let mut step = ImportedStep {
            action: rich(&text(v, "body")),
            expected: rich(&expected),
            ..Default::default()
        };
        let mut own = Vec::new();
        step.steps = normalised(v, all, names, depth + 1, budget, attachments, &mut own);
        step.attachments.extend(own);
        out.push(step);
    }
    out
}

fn tree(v: &Value, depth: usize, attachments: &mut Vec<ImportedAttachment>) -> Vec<ImportedStep> {
    let Some(arr) = v.as_array() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for s in arr.iter().take(10_000) {
        let mut step = ImportedStep {
            action: rich([text(s, "keyword"), text(s, "name")].join(" ").trim()),
            expected: rich(&text(s, "expectedResult")),
            ..Default::default()
        };
        if let Some(a) = s.get("attachments").and_then(Value::as_array) {
            for x in a.iter().take(100) {
                let aid = text(x, "id");
                if aid.is_empty() {
                    continue;
                }
                let source = format!("testops:{aid}");
                step.attachments.push(source.clone());
                if !attachments.iter().any(|y| y.source == source) {
                    attachments.push(ImportedAttachment {
                        name: text(x, "name"),
                        source,
                        old_id: None,
                    });
                }
            }
        }
        if depth < 16 {
            step.steps = tree(
                s.get("steps").unwrap_or(&Value::Null),
                depth + 1,
                attachments,
            );
        }
        out.push(step);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_urls() {
        assert_eq!(
            check_base_url("https://testops.example.com/", false)
                .unwrap()
                .as_str(),
            "https://testops.example.com/"
        );
        assert!(check_base_url("http://testops.example.com", false).is_err());
        assert!(check_base_url("http://testops.example.com", true).is_ok());
        assert!(check_base_url("https://u:p@testops.example.com", false).is_err());
        assert!(check_base_url("https://testops.example.com/?x=1", false).is_err());
        assert!(check_base_url("ftp://testops.example.com", true).is_err());
    }

    #[test]
    fn scenarios() {
        let n: Value = serde_json::json!({"root": {"children": [1, 2]}, "scenarioSteps": {
            "1": {"body": "Open", "children": [3], "expectedResultId": 5},
            "2": {"body": "Check"},
            "3": {"body": "Sub"},
            "4": {"attachmentId": 9},
            "5": {"body": "Shown", "children": []}
        }, "attachments": {"9": {"name": "shot.png"}}});
        let mut a = Vec::new();
        let s = scenario_steps(&n, &mut a);
        assert_eq!(s.len(), 2);
        assert_eq!(s[0].expected, "Shown");
        assert_eq!(s[0].steps[0].action, "Sub");
        let n: Value = serde_json::json!({"root": {"children": [1]}, "scenarioSteps": {
            "1": {"body": "Submit", "children": [2]}, "2": {"attachmentId": 9}}});
        let s = scenario_steps(&n, &mut a);
        assert_eq!(s[0].attachments, vec!["testops:9"]);
        assert!(s[0].steps.is_empty());
        let old: Value = serde_json::json!({"steps": [{"name": "A", "steps": [{"name": "B", "expectedResult": "ok"}]}]});
        let s = scenario_steps(&old, &mut a);
        assert_eq!(s[0].steps[0].expected, "ok");
    }
}
