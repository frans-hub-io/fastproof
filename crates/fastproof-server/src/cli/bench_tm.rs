//! `fastproof bench tm`: loads test management through the public HTTP API only (the same
//! token and URL as `fastproof bench`), so it runs from a laptop or a pod against any
//! installation. It builds a project with fields, a folder tree and cases with steps and
//! versions, links automated tests by `ALLURE_ID` through an allure-results upload, reads the
//! list and the tree, starts a launch from a managed plan, runs it with concurrent virtual
//! testers, adds the automated part, exports the cases and imports a TestRail CSV into a
//! second project; then prints p50/p95/max per operation against the lab targets.
//!
//! Data is generated from `--seed` case by case while it is sent, so memory stays small
//! whatever the counts.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, anyhow, bail};
use bytes::Bytes;
use clap::Args;
use futures::{StreamExt, stream};
use reqwest::{Method, RequestBuilder};
use serde::Serialize;
use serde_json::{Value, json};
use uuid::Uuid;

use super::client::Client;

/// Options of `fastproof bench tm` (the defaults are the lab scenario).
#[derive(Args, Debug, Clone)]
pub struct TmBenchArgs {
    /// Server URL.
    #[arg(long, env = "FASTPROOF_URL")]
    pub url: String,
    /// File holding the API token (otherwise FASTPROOF_TOKEN). The token needs
    /// `projects:write` and the test management permissions (an admin token).
    #[arg(long, env = "FASTPROOF_TOKEN_FILE")]
    pub token_file: Option<PathBuf>,
    /// Key of the project to create (default: a random `bench-tm-…`). It must not exist;
    /// the TestRail import goes into `<key>-imp`.
    #[arg(long)]
    pub project: Option<String>,
    /// Keep both projects after the run (otherwise they are deleted at the end).
    #[arg(long)]
    pub keep: bool,
    /// Seed of the generated data.
    #[arg(long, default_value_t = 42)]
    pub seed: u64,
    /// Test cases.
    #[arg(long, default_value_t = 20_000)]
    pub cases: usize,
    /// Case folders.
    #[arg(long, default_value_t = 300)]
    pub folders: usize,
    /// Depth of the folder tree, at most.
    #[arg(long, default_value_t = 5)]
    pub folder_depth: usize,
    /// Custom fields (select, multiselect, text, number and date in turn).
    #[arg(long, default_value_t = 30)]
    pub fields: usize,
    /// Steps per case on average (some nested).
    #[arg(long, default_value_t = 12)]
    pub steps: usize,
    /// Versions of each case of the version sample (edits = versions - 1).
    #[arg(long, default_value_t = 5)]
    pub versions: usize,
    /// Cases that get `versions - 1` edits.
    #[arg(long, default_value_t = 2_000)]
    pub version_sample: usize,
    /// Steps of the large case saves (the "case save" target).
    #[arg(long, default_value_t = 100)]
    pub save_steps: usize,
    /// Large case saves.
    #[arg(long, default_value_t = 200)]
    pub save_sample: usize,
    /// Cases that automated tests link to by `ALLURE_ID`.
    #[arg(long, default_value_t = 2_000)]
    pub automated: usize,
    /// Cases of the managed plan (the run cases by a filter plus automated cases by number).
    #[arg(long, default_value_t = 5_000)]
    pub plan_cases: usize,
    /// Manual cases executed in the launch started from the plan.
    #[arg(long, default_value_t = 2_000)]
    pub run_cases: usize,
    /// Steps of each executed case.
    #[arg(long, default_value_t = 25)]
    pub run_steps: usize,
    /// Virtual testers executing at the same time.
    #[arg(long, default_value_t = 50)]
    pub testers: usize,
    /// A tester's pause before each step write and final status, in milliseconds on average
    /// (uniform 0..2x); 0 runs the testers flat out (a stress test, not a human pace).
    #[arg(long, default_value_t = 0)]
    pub think_ms: u64,
    /// Tests in each allure-results upload (`automated` of them carry `ALLURE_ID`).
    #[arg(long, default_value_t = 5_000)]
    pub upload_results: usize,
    /// Cases of the generated TestRail CSV import (0: no import).
    #[arg(long, default_value_t = 20_000)]
    pub import_cases: usize,
    /// Samples of each read (list page, tree level, case).
    #[arg(long, default_value_t = 200)]
    pub samples: usize,
    /// Requests sent at the same time while creating and editing cases.
    #[arg(long, default_value_t = 16)]
    pub parallel: usize,
    /// Requests sent at the same time by the read phase.
    #[arg(long, default_value_t = 4)]
    pub read_parallel: usize,
    /// Seconds to wait for uploads, code sync and imports.
    #[arg(long, default_value_t = 900)]
    pub wait_timeout: u64,
}

// ---------------------------------------------------------------------------------------------
// Deterministic generators.

fn mix(mut z: u64) -> u64 {
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// A small deterministic generator (splitmix64), one stream per seed and purpose.
#[derive(Clone, Debug)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64, stream: u64) -> Self {
        Self(mix(seed ^ mix(stream.wrapping_add(0x9E37_79B9_7F4A_7C15))))
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        mix(self.0)
    }

    /// `0..n` (0 when `n` is 0).
    pub fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            0
        } else {
            (self.next_u64() % n as u64) as usize
        }
    }

    pub fn chance(&mut self, percent: usize) -> bool {
        self.below(100) < percent
    }
}

const STREAM_FIELDS: u64 = 1 << 56;
const STREAM_FOLDERS: u64 = 2 << 56;
const STREAM_CASE: u64 = 3 << 56;
const STREAM_RESULT: u64 = 4 << 56;
const STREAM_IMPORT: u64 = 5 << 56;
const STREAM_RUN: u64 = 6 << 56;
const STREAM_READ: u64 = 7 << 56;

const WORDS: [&str; 48] = [
    "checkout",
    "cart",
    "payment",
    "user",
    "profile",
    "order",
    "invoice",
    "search",
    "filter",
    "login",
    "session",
    "token",
    "address",
    "delivery",
    "discount",
    "coupon",
    "product",
    "catalog",
    "review",
    "rating",
    "account",
    "password",
    "email",
    "notification",
    "report",
    "export",
    "import",
    "settings",
    "language",
    "currency",
    "refund",
    "wishlist",
    "banner",
    "page",
    "button",
    "form",
    "dialog",
    "table",
    "column",
    "row",
    "upload",
    "download",
    "status",
    "history",
    "message",
    "widget",
    "dashboard",
    "summary",
];
const VERBS: [&str; 12] = [
    "Open",
    "Click",
    "Enter",
    "Select",
    "Submit",
    "Check",
    "Verify",
    "Scroll to",
    "Type into",
    "Clear",
    "Drag",
    "Confirm",
];

fn words(rng: &mut Rng, n: usize) -> String {
    let mut s = String::new();
    for i in 0..n {
        if i > 0 {
            s.push(' ');
        }
        s.push_str(WORDS[rng.below(WORDS.len())]);
    }
    s
}

/// A custom field of the scenario.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FieldSpec {
    pub key: String,
    pub kind: &'static str,
    pub options: Vec<String>,
}

const FIELD_KINDS: [&str; 5] = ["select", "multiselect", "text", "number", "date"];

/// `n` fields of mixed types; the first is a select (used by the list filter).
pub fn field_specs(seed: u64, n: usize) -> Vec<FieldSpec> {
    (0..n)
        .map(|i| {
            let mut rng = Rng::new(seed, STREAM_FIELDS | i as u64);
            let kind = FIELD_KINDS[i % FIELD_KINDS.len()];
            let options = if matches!(kind, "select" | "multiselect") {
                (0..3 + rng.below(10))
                    .map(|k| format!("{} {k}", WORDS[(i + k) % WORDS.len()]))
                    .collect()
            } else {
                Vec::new()
            };
            FieldSpec {
                key: format!("bf_{i:02}"),
                kind,
                options,
            }
        })
        .collect()
}

impl FieldSpec {
    /// The create body (`CaseFieldWrite`).
    pub fn write(&self) -> Value {
        let mut v =
            json!({"key": self.key, "name": format!("Bench {}", self.key), "type": self.kind});
        if !self.options.is_empty() {
            v["options"] = Value::Array(
                self.options
                    .iter()
                    .map(|o| json!({"value": o, "archived": false}))
                    .collect(),
            );
        }
        v
    }

    fn value(&self, rng: &mut Rng) -> Value {
        match self.kind {
            "select" => json!(self.options[rng.below(self.options.len())]),
            "multiselect" => {
                let a = rng.below(self.options.len());
                let b = rng.below(self.options.len());
                if a == b {
                    json!([self.options[a]])
                } else {
                    json!([self.options[a.min(b)], self.options[a.max(b)]])
                }
            }
            "number" => json!(rng.below(1000)),
            "date" => json!(format!(
                "2026-{:02}-{:02}",
                1 + rng.below(12),
                1 + rng.below(28)
            )),
            _ => json!(words(rng, 3)),
        }
    }
}

/// Parent index of each folder (`None` for roots): parents come before their children and
/// no folder is deeper than `max_depth` levels.
pub fn folder_tree(seed: u64, n: usize, max_depth: usize) -> Vec<Option<usize>> {
    let max_depth = max_depth.max(1);
    let roots = (n / 20).clamp(1, n.max(1));
    let mut rng = Rng::new(seed, STREAM_FOLDERS);
    let mut parents: Vec<Option<usize>> = Vec::with_capacity(n);
    let mut depth: Vec<usize> = Vec::with_capacity(n);
    for i in 0..n {
        if i < roots {
            parents.push(None);
            depth.push(1);
            continue;
        }
        let mut p = rng.below(i);
        while depth[p] >= max_depth {
            p = parents[p].unwrap_or(p);
            if depth[p] < max_depth {
                break;
            }
        }
        if depth[p] >= max_depth {
            parents.push(None);
            depth.push(1);
        } else {
            parents.push(Some(p));
            depth.push(depth[p] + 1);
        }
    }
    parents
}

/// Depth of each folder of `folder_tree` (roots are 1).
pub fn folder_depths(parents: &[Option<usize>]) -> Vec<usize> {
    let mut d = vec![0; parents.len()];
    for i in 0..parents.len() {
        d[i] = parents[i].map_or(1, |p| d[p] + 1);
    }
    d
}

fn step(rng: &mut Rng, children: Vec<Value>) -> Value {
    json!({
        "action": format!("{} the {} {}", VERBS[rng.below(VERBS.len())], words(rng, 2), words(rng, 1)),
        "expected": format!("The {} shows the {}", words(rng, 2), words(rng, 2)),
        "data": if rng.chance(20) { format!("`{}-{}`", words(rng, 1), rng.below(10_000)) } else { String::new() },
        "steps": children,
    })
}

/// `n` steps in all (counting nested ones), about one top-level step in four with two
/// nested steps.
pub fn steps(rng: &mut Rng, n: usize) -> Vec<Value> {
    let mut out = Vec::new();
    let mut left = n;
    while left > 0 {
        if left >= 3 && rng.chance(25) {
            let kids = (0..2).map(|_| step(rng, Vec::new())).collect();
            out.push(step(rng, kids));
            left -= 3;
        } else {
            out.push(step(rng, Vec::new()));
            left -= 1;
        }
    }
    out
}

/// Steps of a step tree, nested ones included.
pub fn count_steps(steps: &[Value]) -> usize {
    steps
        .iter()
        .map(|s| 1 + s["steps"].as_array().map_or(0, |c| count_steps(c)))
        .sum()
}

/// Step paths (`1`, `1.2`) of a step tree, parents before their children.
pub fn step_paths(steps: &[Value]) -> Vec<String> {
    fn walk(steps: &[Value], prefix: &str, out: &mut Vec<String>) {
        for (i, s) in steps.iter().enumerate() {
            let p = if prefix.is_empty() {
                (i + 1).to_string()
            } else {
                format!("{prefix}.{}", i + 1)
            };
            out.push(p.clone());
            if let Some(c) = s["steps"].as_array() {
                walk(c, &p, out);
            }
        }
    }
    let mut out = Vec::new();
    walk(steps, "", &mut out);
    out
}

/// What a case is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    /// Linked by `ALLURE_ID` from the automated tests.
    Automated,
    /// A manual case of the plan, executed by the testers (tag `bench-run`).
    Run,
    Plain,
}

/// Tag of the cases the plan's filter selects.
pub const RUN_TAG: &str = "bench-run";

/// The case population of a scenario.
#[derive(Clone, Debug)]
pub struct Gen {
    pub seed: u64,
    pub cases: usize,
    pub automated: usize,
    pub run_cases: usize,
    pub run_steps: usize,
    pub steps: usize,
    pub version_sample: usize,
    pub save_sample: usize,
    pub fields: Vec<FieldSpec>,
}

impl Gen {
    pub fn of(a: &TmBenchArgs) -> Self {
        Self {
            seed: a.seed,
            cases: a.cases,
            automated: a.automated,
            run_cases: a.run_cases,
            run_steps: a.run_steps,
            steps: a.steps,
            version_sample: a.version_sample,
            save_sample: a.save_sample,
            fields: field_specs(a.seed, a.fields),
        }
    }

    pub fn role(&self, i: usize) -> Role {
        if i < self.automated {
            Role::Automated
        } else if i < self.automated + self.run_cases {
            Role::Run
        } else {
            Role::Plain
        }
    }

    /// Indices of the cases that get version edits (after the run cases).
    pub fn version_range(&self) -> std::ops::Range<usize> {
        let from = (self.automated + self.run_cases).min(self.cases);
        from..(from + self.version_sample).min(self.cases)
    }

    /// Indices of the cases saved with `save_steps` steps (the last ones, never run cases).
    pub fn save_range(&self) -> std::ops::Range<usize> {
        let first = (self.automated + self.run_cases).min(self.cases);
        self.cases.saturating_sub(self.save_sample).max(first)..self.cases
    }

    /// Steps of the other cases so the average over all cases is `steps`.
    fn plain_mean(&self) -> f64 {
        let others = self.cases.saturating_sub(self.run_cases).max(1) as f64;
        let total = (self.cases * self.steps) as f64 - (self.run_cases * self.run_steps) as f64;
        (total / others).max(1.0)
    }

    pub fn step_count(&self, i: usize) -> usize {
        if self.role(i) == Role::Run {
            return self.run_steps.max(1);
        }
        let mean = self.plain_mean();
        let lo = (mean * 0.5).floor().max(1.0) as usize;
        let hi = (mean * 1.5).ceil().max(lo as f64) as usize;
        let mut rng = Rng::new(self.seed, STREAM_CASE | (i as u64) << 8 | 0xFF);
        let mut n = lo + rng.below(hi - lo + 1);
        // Pull towards the mean so the population average lands on it.
        if (n as f64) < mean && rng.chance(50) {
            n = mean.round() as usize;
        }
        n.max(1)
    }

    /// The `CaseWrite` of case `i` at edit `edit` (0 = created), with `steps` steps when given.
    pub fn case(&self, i: usize, edit: usize, steps_n: Option<usize>, folders: &[Uuid]) -> Value {
        let mut rng = Rng::new(self.seed, STREAM_CASE | (i as u64) << 8);
        let role = self.role(i);
        let n = steps_n.unwrap_or_else(|| self.step_count(i));
        let folder = folders.get(rng.below(folders.len())).copied();
        let status = match (role, rng.below(10)) {
            (Role::Run, _) => "active",
            (_, 0) => "draft",
            (_, 1) => "outdated",
            _ => "active",
        };
        let mut tags: Vec<String> = (0..rng.below(4))
            .map(|_| format!("tag-{}", rng.below(20)))
            .collect();
        if role == Role::Run {
            tags.push(RUN_TAG.into());
        }
        tags.sort();
        tags.dedup();
        let mut fields = serde_json::Map::new();
        for f in &self.fields {
            if rng.chance(60) {
                fields.insert(f.key.clone(), f.value(&mut rng));
            }
        }
        let title = words(&mut rng, 3);
        let description = format!(
            "Checks that the **{}** works for a {} with {}.\n\nSee the {} guide for the {} rules.",
            words(&mut rng, 2),
            words(&mut rng, 1),
            words(&mut rng, 3),
            words(&mut rng, 1),
            words(&mut rng, 2)
        );
        let preconditions = format!(
            "- A {} exists\n- The {} is signed in",
            words(&mut rng, 2),
            words(&mut rng, 1)
        );
        let mut st = steps(&mut rng, n);
        let mut name = format!("TC bench {i}: {title}");
        if edit > 0 {
            name.push_str(&format!(" (rev {edit})"));
            let mut erng = Rng::new(self.seed, STREAM_CASE | (i as u64) << 8 | edit as u64);
            if let Some(s) = st.first_mut() {
                s["action"] = json!(format!("{} (edit {edit})", words(&mut erng, 4)));
            }
            if let Some(f) = self.fields.first() {
                fields.insert(f.key.clone(), f.value(&mut erng));
            }
        }
        json!({
            "name": name,
            "folderId": folder,
            "status": status,
            "body": {"description": description, "preconditions": preconditions, "steps": st, "links": []},
            "fields": fields,
            "tags": tags,
        })
    }
}

const SEVERITIES: [&str; 3] = ["normal", "critical", "minor"];

/// One Allure result of test `j` (`ALLURE_ID` when given).
pub fn allure_result(seed: u64, j: usize, allure_id: Option<i64>, now: i64) -> Value {
    let mut rng = Rng::new(seed, STREAM_RESULT | j as u64);
    let failed = rng.chance(8);
    let suite = format!("Suite {:02}", j % 40);
    let mut labels = vec![
        json!({"name": "suite", "value": suite}),
        json!({"name": "feature", "value": format!("Feature {}", j % 25)}),
        json!({"name": "severity", "value": SEVERITIES[j % 3]}),
    ];
    if let Some(a) = allure_id {
        labels.push(json!({"name": "ALLURE_ID", "value": a.to_string()}));
    }
    let start = now + (j as i64) * 10;
    let mut r = json!({
        "uuid": Uuid::from_u64_pair(rng.next_u64(), rng.next_u64()).to_string(),
        "name": format!("test {j} {}", words(&mut rng, 2)),
        "fullName": format!("com.example.bench.suite{:02}.Feature{}Test#case{j}", j % 40, j % 25),
        "status": if failed { "failed" } else { "passed" },
        "stage": "finished",
        "start": start,
        "stop": start + 50 + rng.below(500) as i64,
        "labels": labels,
        "steps": [
            {"name": "open the page", "status": "passed", "stage": "finished", "start": start, "stop": start + 10},
            {"name": "fill the form", "status": "passed", "stage": "finished", "start": start + 10, "stop": start + 30},
            {"name": "submit", "status": if failed { "failed" } else { "passed" }, "stage": "finished",
             "start": start + 30, "stop": start + 50},
        ],
    });
    if failed {
        r["statusDetails"] = json!({"message": "AssertionError: expected 200 but was 500",
            "trace": format!("java.lang.AssertionError\n\tat com.example.bench.Test.case{j}(Test.java:42)")});
    }
    r
}

/// An allure-results tar.gz of `n` tests, compressed while generated (only the compressed
/// bytes are held).
pub fn allure_tar_gz(
    seed: u64,
    n: usize,
    allure_id: &dyn Fn(usize) -> Option<i64>,
    now: i64,
) -> anyhow::Result<Vec<u8>> {
    let enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    let mut tar = tar::Builder::new(enc);
    for j in 0..n {
        let r = allure_result(seed, j, allure_id(j), now);
        let bytes = serde_json::to_vec(&r)?;
        let mut h = tar::Header::new_gnu();
        h.set_size(bytes.len() as u64);
        h.set_mode(0o644);
        h.set_mtime((now / 1000).max(0) as u64);
        h.set_cksum();
        tar.append_data(
            &mut h,
            format!("{}-result.json", r["uuid"].as_str().unwrap_or("r")),
            bytes.as_slice(),
        )?;
    }
    Ok(tar.into_inner()?.finish()?)
}

const TESTRAIL_HEADER: [&str; 11] = [
    "ID",
    "Title",
    "Section Hierarchy",
    "Template",
    "Type",
    "Priority",
    "Estimate",
    "References",
    "Preconditions",
    "Steps (Step)",
    "Steps (Expected Result)",
];

fn csv_line(out: &mut String, cells: &[String]) {
    for (i, c) in cells.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push('"');
        out.push_str(&c.replace('"', "\"\""));
        out.push('"');
    }
    out.push_str("\r\n");
}

/// The header line of the TestRail CSV.
pub fn testrail_header() -> String {
    let mut s = String::new();
    csv_line(
        &mut s,
        &TESTRAIL_HEADER
            .iter()
            .map(|h| h.to_string())
            .collect::<Vec<_>>(),
    );
    s
}

/// TestRail CSV rows of cases `from..to` (one row per case, steps as numbered lists, the
/// layout TestRail exports with "steps in one cell").
pub fn testrail_rows(seed: u64, from: usize, to: usize, steps_mean: usize) -> String {
    let mut s = String::new();
    for i in from..to {
        let mut rng = Rng::new(seed, STREAM_IMPORT | i as u64);
        let n = (steps_mean / 2 + rng.below(steps_mean.max(1) + 1)).max(1);
        let mut actions = String::new();
        let mut expected = String::new();
        for k in 1..=n {
            actions.push_str(&format!(
                "{k}. {} the <b>{}</b>\n",
                VERBS[rng.below(VERBS.len())],
                words(&mut rng, 2)
            ));
            expected.push_str(&format!("{k}. The {} is shown\n", words(&mut rng, 2)));
        }
        let section = format!(
            "Area {} > Module {} > Feature {}",
            rng.below(10),
            rng.below(5),
            rng.below(6)
        );
        csv_line(
            &mut s,
            &[
                format!("C{}", i + 1),
                format!("Imported {i}: {}", words(&mut rng, 3)),
                section,
                "Test Case (Steps)".into(),
                ["Functional", "Regression", "Smoke & Sanity", "Acceptance"][rng.below(4)].into(),
                ["Low", "Medium", "High", "Critical"][rng.below(4)].into(),
                format!("{}m", 1 + rng.below(30)),
                if rng.chance(30) {
                    format!("REQ-{}", rng.below(500))
                } else {
                    String::new()
                },
                format!("<p>The {} is ready</p>", words(&mut rng, 2)),
                actions.trim_end().to_string(),
                expected.trim_end().to_string(),
            ],
        );
    }
    s
}

// ---------------------------------------------------------------------------------------------
// Measurements.

/// Operations of the report: name, description, target in milliseconds.
const OPS: &[(&str, &str, Option<f64>)] = &[
    ("project.create", "create a project", None),
    ("field.create", "create a field", None),
    ("folder.create", "create a folder", None),
    ("case.create", "create a case", None),
    ("case.update", "save a case (new version)", None),
    (
        "case.save.big",
        "save a case with save-steps steps (new version)",
        Some(150.0),
    ),
    ("code.upload", "allure-results upload request", None),
    ("code.ingest", "upload processed (request to done)", None),
    (
        "code.sync",
        "code sync: launch finished to cases linked",
        None,
    ),
    ("cases.list", "list page (50) with 2 filters", Some(200.0)),
    ("cases.tree", "tree level with counts", Some(150.0)),
    ("case.get", "get a case", None),
    ("plan.create", "create the managed plan", None),
    ("plan.launch", "start a launch from the plan", Some(3000.0)),
    ("run.item.get", "open a run item", None),
    ("run.step", "write a step result", Some(50.0)),
    ("run.final", "final status with projection", Some(150.0)),
    (
        "combined.upload",
        "allure upload into the plan launch",
        None,
    ),
    ("combined.ingest", "plan launch upload processed", None),
    ("run.close", "close the manual run", None),
    (
        "export.ndjson",
        "export all cases as NDJSON",
        Some(20_000.0),
    ),
    ("import.dryrun", "TestRail CSV import: dry run", None),
    ("import.apply", "TestRail CSV import: apply", None),
    (
        "import.total",
        "TestRail CSV import: dry run + apply",
        Some(60_000.0),
    ),
];

#[derive(Default)]
struct RecInner {
    lat: BTreeMap<&'static str, Vec<f64>>,
    requests: u64,
    errors: u64,
    first_errors: Vec<String>,
}

/// Latencies per operation and errors.
#[derive(Default)]
pub struct Recorder(Mutex<RecInner>);

impl Recorder {
    pub fn sample(&self, op: &'static str, ms: f64) {
        let mut g = self.0.lock().unwrap_or_else(|e| e.into_inner());
        g.lat.entry(op).or_default().push(ms);
    }

    fn request(&self) {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).requests += 1;
    }

    pub fn error(&self, msg: String) {
        let mut g = self.0.lock().unwrap_or_else(|e| e.into_inner());
        g.errors += 1;
        if g.first_errors.len() < 10 {
            g.first_errors.push(msg);
        }
    }
}

/// Nearest-rank percentile of sorted values.
pub fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let rank = ((p / 100.0) * sorted.len() as f64).ceil() as usize;
    sorted[rank.clamp(1, sorted.len()) - 1]
}

/// One row of the report.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OpRow {
    pub op: String,
    pub description: String,
    pub count: usize,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub max_ms: f64,
    pub target_ms: Option<f64>,
    /// p95 within the target (single measurements: the value itself).
    pub pass: Option<bool>,
}

/// What a run measured.
#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TmBenchReport {
    pub project: String,
    pub kept: bool,
    pub ops: Vec<OpRow>,
    /// Seconds per phase.
    pub phases: Vec<(String, f64)>,
    pub requests: u64,
    pub errors: u64,
    pub first_errors: Vec<String>,
    pub cases_created: usize,
    pub automated_cases: usize,
    pub plan_cases: i64,
    pub run_items: usize,
    pub step_results: usize,
    /// `manual` counters of the plan launch after close.
    pub manual: Value,
    /// Results of the plan launch after the combined upload and close.
    pub launch_results: i64,
    pub exported_cases: usize,
    pub exported_bytes: u64,
    pub imported_cases: i64,
    pub total_seconds: f64,
}

impl TmBenchReport {
    /// The report as a table.
    pub fn render(&self) -> String {
        let mut s = String::new();
        s.push_str(&format!(
            "{:<16} {:<48} {:>7} {:>10} {:>10} {:>10} {:>10}  {}\n",
            "operation", "what", "count", "p50 ms", "p95 ms", "max ms", "target", "result"
        ));
        for r in &self.ops {
            s.push_str(&format!(
                "{:<16} {:<48} {:>7} {:>10.1} {:>10.1} {:>10.1} {:>10}  {}\n",
                r.op,
                r.description,
                r.count,
                r.p50_ms,
                r.p95_ms,
                r.max_ms,
                r.target_ms
                    .map(|t| format!("< {t:.0}"))
                    .unwrap_or_else(|| "-".into()),
                match r.pass {
                    Some(true) => "pass",
                    Some(false) => "MISS",
                    None => "",
                }
            ));
        }
        s.push('\n');
        for (name, secs) in &self.phases {
            s.push_str(&format!("phase {name}: {secs:.1} s\n"));
        }
        s.push_str(&format!(
            "cases {} ({} automated), plan {} cases, {} run items, {} step results, launch results {}, manual {}\n",
            self.cases_created,
            self.automated_cases,
            self.plan_cases,
            self.run_items,
            self.step_results,
            self.launch_results,
            self.manual
        ));
        s.push_str(&format!(
            "exported {} cases ({} bytes), imported {} cases; {} requests, {} errors, total {:.1} s{}\n",
            self.exported_cases,
            self.exported_bytes,
            self.imported_cases,
            self.requests,
            self.errors,
            self.total_seconds,
            if self.kept {
                format!("; project {} kept", self.project)
            } else {
                String::new()
            }
        ));
        for e in &self.first_errors {
            s.push_str(&format!("  error: {e}\n"));
        }
        s
    }
}

fn rows(rec: &Recorder) -> Vec<OpRow> {
    let g = rec.0.lock().unwrap_or_else(|e| e.into_inner());
    let mut out = Vec::new();
    for (op, desc, target) in OPS {
        let Some(v) = g.lat.get(op) else { continue };
        let mut v = v.clone();
        v.sort_by(f64::total_cmp);
        let p95 = percentile(&v, 95.0);
        out.push(OpRow {
            op: op.to_string(),
            description: desc.to_string(),
            count: v.len(),
            p50_ms: percentile(&v, 50.0),
            p95_ms: p95,
            max_ms: v.last().copied().unwrap_or(0.0),
            target_ms: *target,
            pass: target.map(|t| p95 < t),
        });
    }
    out
}

// ---------------------------------------------------------------------------------------------
// HTTP.

/// A successful answer.
struct Reply {
    etag: Option<String>,
    body: Value,
}

#[derive(Clone)]
struct Api {
    client: Client,
    rec: Arc<Recorder>,
}

impl Api {
    fn rb(&self, m: Method, path: &str) -> anyhow::Result<RequestBuilder> {
        self.client.request(m, path)
    }

    /// Sends a request; records its latency under `op` (when given) and any failure.
    async fn send(
        &self,
        op: Option<&'static str>,
        what: &str,
        rb: RequestBuilder,
    ) -> Option<Reply> {
        self.rec.request();
        let t = Instant::now();
        let resp = match rb.send().await {
            Ok(r) => r,
            Err(e) => {
                self.rec.error(format!("{what}: {e}"));
                return None;
            }
        };
        let status = resp.status();
        let etag = resp
            .headers()
            .get(reqwest::header::ETAG)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        let text = resp.text().await.unwrap_or_default();
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        if !status.is_success() {
            let mut msg: String = text.chars().take(300).collect();
            if msg.is_empty() {
                msg = "(no body)".into();
            }
            self.rec.error(format!("{what}: {} {msg}", status.as_u16()));
            return None;
        }
        if let Some(op) = op {
            self.rec.sample(op, ms);
        }
        Some(Reply {
            etag,
            body: serde_json::from_str(&text).unwrap_or(Value::Null),
        })
    }

    async fn timed(&self, op: &'static str, rb: RequestBuilder) -> Option<Reply> {
        self.send(Some(op), op, rb).await
    }

    async fn get(&self, what: &str, path: &str) -> Option<Value> {
        let rb = self.rb(Method::GET, path).ok()?;
        self.send(None, what, rb).await.map(|r| r.body)
    }
}

fn enc(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes()).collect()
}

struct Ctx {
    api: Api,
    a: TmBenchArgs,
    base: String,
    model: Gen,
    folders: Vec<Uuid>,
}

fn ms_since(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1000.0
}

/// Checks the options against each other and the server's default limits.
pub fn validate(a: &TmBenchArgs) -> anyhow::Result<()> {
    if a.cases == 0 || a.parallel == 0 || a.testers == 0 || a.read_parallel == 0 {
        bail!("--cases, --parallel, --read-parallel and --testers must be positive");
    }
    if a.automated + a.run_cases > a.cases {
        bail!("--automated plus --run-cases must not exceed --cases");
    }
    if a.automated > a.upload_results {
        bail!("--automated must not exceed --upload-results");
    }
    if a.run_cases > a.plan_cases {
        bail!("--run-cases must not exceed --plan-cases");
    }
    if a.versions == 0 {
        bail!("--versions is at least 1");
    }
    if a.fields > 90 {
        bail!("--fields: at most 90 (the server allows 100 per project with the built-in ones)");
    }
    if a.folders == 0 || a.folders > 20_000 || a.folder_depth == 0 || a.folder_depth > 10 {
        bail!("--folders 1..=20000 and --folder-depth 1..=10");
    }
    if a.steps == 0 || a.run_steps == 0 || a.run_steps > 500 || a.save_steps > 500 {
        bail!("--steps and --run-steps must be positive; --run-steps and --save-steps at most 500");
    }
    if let Some(p) = &a.project
        && (p.len() > 59 || !fastproof_blob::valid_project_key(p))
    {
        bail!("--project must match ^[a-z0-9][a-z0-9-]{{1,58}}$ (the import project adds -imp)");
    }
    Ok(())
}

/// Runs `fastproof bench tm`.
pub async fn run(a: TmBenchArgs) -> anyhow::Result<TmBenchReport> {
    validate(&a)?;
    let token = match &a.token_file {
        Some(f) => {
            std::fs::read_to_string(f).with_context(|| format!("reading {}", f.display()))?
        }
        None => std::env::var("FASTPROOF_TOKEN").unwrap_or_default(),
    };
    let client = Client::new(&a.url, &token)?;
    let rec = Arc::new(Recorder::default());
    let api = Api { client, rec };
    let run = Uuid::now_v7().simple().to_string()[24..].to_string();
    let key = a
        .project
        .clone()
        .unwrap_or_else(|| format!("bench-tm-{run}"));
    let started = Instant::now();
    let mut report = TmBenchReport {
        project: key.clone(),
        kept: a.keep,
        ..Default::default()
    };
    let rb = api
        .rb(Method::POST, "api/v1/projects")?
        .json(&json!({"key": key, "name": format!("Bench TM {key}")}));
    if api.timed("project.create", rb).await.is_none() {
        let first = api
            .rec
            .0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .first_errors
            .last()
            .cloned()
            .unwrap_or_default();
        bail!("could not create project {key} (it must not exist): {first}");
    }
    let mut created = vec![key.clone()];
    let res = phases(&api, &a, &key, &run, &mut created, &mut report).await;
    if !a.keep {
        for k in &created {
            let rb = api.rb(Method::DELETE, &format!("api/v1/projects/{}", enc(k)))?;
            if api.send(None, "project.delete", rb).await.is_none() {
                eprintln!("could not delete project {k}");
            }
        }
    }
    res?;
    report.ops = rows(&api.rec);
    let g = api.rec.0.lock().unwrap_or_else(|e| e.into_inner());
    report.requests = g.requests;
    report.errors = g.errors;
    report.first_errors = g.first_errors.clone();
    report.total_seconds = started.elapsed().as_secs_f64();
    Ok(report)
}

async fn phases(
    api: &Api,
    a: &TmBenchArgs,
    key: &str,
    run: &str,
    created: &mut Vec<String>,
    report: &mut TmBenchReport,
) -> anyhow::Result<()> {
    let base = format!("api/v1/projects/{}", enc(key));
    let model = Gen::of(a);

    // 1. Setup: fields and folders.
    let t = Instant::now();
    for f in &model.fields {
        let rb = api
            .rb(Method::POST, &format!("{base}/case-fields"))?
            .json(&f.write());
        api.timed("field.create", rb).await;
    }
    let parents = folder_tree(a.seed, a.folders, a.folder_depth);
    let depths = folder_depths(&parents);
    let mut ids: Vec<Option<Uuid>> = vec![None; parents.len()];
    for level in 1..=a.folder_depth {
        let todo: Vec<usize> = (0..parents.len()).filter(|i| depths[*i] == level).collect();
        let made: Vec<(usize, Option<Uuid>)> = stream::iter(todo)
            .map(|i| {
                let parent = parents[i].and_then(|p| ids[p]);
                let body = json!({"name": format!("Area {i}"), "parentId": parent});
                let rb = api.rb(Method::POST, &format!("{base}/case-folders"));
                async move {
                    let r = match rb {
                        Ok(rb) => api.timed("folder.create", rb.json(&body)).await,
                        Err(_) => None,
                    };
                    (
                        i,
                        r.and_then(|r| r.body["id"].as_str().and_then(|s| s.parse().ok())),
                    )
                }
            })
            .buffer_unordered(a.parallel)
            .collect()
            .await;
        for (i, id) in made {
            ids[i] = id;
        }
    }
    let folders: Vec<Uuid> = ids.iter().flatten().copied().collect();
    let with_children: Vec<Uuid> = (0..parents.len())
        .filter(|i| parents.contains(&Some(*i)))
        .filter_map(|i| ids[i])
        .collect();
    report
        .phases
        .push(("setup".into(), t.elapsed().as_secs_f64()));
    eprintln!(
        "setup: {} fields, {} folders in {:.1} s",
        model.fields.len(),
        folders.len(),
        t.elapsed().as_secs_f64()
    );
    let ctx = Arc::new(Ctx {
        api: api.clone(),
        a: a.clone(),
        base: base.clone(),
        model,
        folders,
    });

    // 2. Cases, then versions.
    let t = Instant::now();
    let made: Vec<(usize, i64, i64)> = stream::iter(0..a.cases)
        .map(|i| {
            let c = ctx.clone();
            async move {
                let body = c.model.case(i, 0, None, &c.folders);
                let rb = c.api.rb(Method::POST, &format!("{}/cases", c.base)).ok()?;
                let r = c.api.timed("case.create", rb.json(&body)).await?;
                Some((
                    i,
                    r.body["number"].as_i64().unwrap_or(0),
                    r.body["revision"].as_i64().unwrap_or(0),
                ))
            }
        })
        .buffer_unordered(a.parallel)
        .filter_map(|x| async move { x })
        .collect()
        .await;
    let mut numbers = vec![0i64; a.cases];
    let mut revisions = vec![0i64; a.cases];
    for (i, n, r) in &made {
        numbers[*i] = *n;
        revisions[*i] = *r;
    }
    drop(made);
    report.cases_created = numbers.iter().filter(|n| **n > 0).count();
    let created_s = t.elapsed().as_secs_f64();
    eprintln!(
        "cases: {} created in {created_s:.1} s",
        report.cases_created
    );
    let edits: Vec<(usize, i64, i64)> = ctx
        .model
        .version_range()
        .filter(|i| numbers[*i] > 0)
        .map(|i| (i, numbers[i], revisions[i]))
        .collect();
    stream::iter(edits)
        .map(|(i, n, rev)| {
            let c = ctx.clone();
            async move {
                let mut rev = rev;
                for v in 1..c.a.versions {
                    let body = c.model.case(i, v, None, &c.folders);
                    let Ok(rb) = c.api.rb(Method::PUT, &format!("{}/cases/{n}", c.base)) else {
                        return;
                    };
                    let rb = rb.header("if-match", format!("\"r{rev}\"")).json(&body);
                    match c.api.timed("case.update", rb).await {
                        Some(r) => rev = r.body["revision"].as_i64().unwrap_or(rev + 1),
                        None => return,
                    }
                }
            }
        })
        .buffer_unordered(a.parallel)
        .collect::<Vec<()>>()
        .await;
    let saves: Vec<(usize, i64, i64)> = ctx
        .model
        .save_range()
        .filter(|i| numbers[*i] > 0 && !ctx.model.version_range().contains(i))
        .map(|i| (i, numbers[i], revisions[i]))
        .collect();
    stream::iter(saves)
        .map(|(i, n, rev)| {
            let c = ctx.clone();
            async move {
                let body = c.model.case(i, 1, Some(c.a.save_steps), &c.folders);
                let Ok(rb) = c.api.rb(Method::PUT, &format!("{}/cases/{n}", c.base)) else {
                    return;
                };
                let rb = rb.header("if-match", format!("\"r{rev}\"")).json(&body);
                c.api.timed("case.save.big", rb).await;
            }
        })
        .buffer_unordered(a.parallel)
        .collect::<Vec<()>>()
        .await;
    report
        .phases
        .push(("cases".into(), t.elapsed().as_secs_f64()));
    eprintln!(
        "versions: done in {:.1} s",
        t.elapsed().as_secs_f64() - created_s
    );

    // 3. Code identities: one allure-results upload on `main`, then code sync.
    let t = Instant::now();
    let now = chrono::Utc::now().timestamp_millis();
    let auto_ids = |j: usize| -> Option<i64> {
        (j < a.automated)
            .then(|| numbers.get(j).copied())
            .flatten()
            .filter(|n| *n > 0)
    };
    let rb = api
        .rb(Method::POST, &format!("{base}/launches"))?
        .json(&json!({
        "name": format!("bench tm {run} code"), "externalId": format!("bench-tm-{run}-code"),
        "vcs": {"branch": "main"}}));
    let launch = api
        .send(None, "code launch start", rb)
        .await
        .ok_or_else(|| anyhow!("could not start the launch for the code upload"))?;
    let code_launch = launch.body["id"].as_str().unwrap_or_default().to_string();
    let tgz = allure_tar_gz(a.seed, a.upload_results, &auto_ids, now)?;
    upload_and_wait(&ctx, &code_launch, tgz, "code.upload", "code.ingest").await?;
    let rb = api
        .rb(
            Method::POST,
            &format!("{base}/launches/{code_launch}/finish"),
        )?
        .json(&json!({"status": "finished"}));
    api.send(None, "code launch finish", rb).await;
    let synced = Instant::now();
    let automated_numbers = wait_sync(&ctx, a.upload_results, a.automated).await?;
    api.rec.sample("code.sync", ms_since(synced));
    report.automated_cases = automated_numbers.len();
    report
        .phases
        .push(("code identities".into(), t.elapsed().as_secs_f64()));
    eprintln!(
        "code: {} automated cases after {:.1} s",
        automated_numbers.len(),
        t.elapsed().as_secs_f64()
    );

    // 4. Reads.
    let t = Instant::now();
    reads(&ctx, &with_children).await;
    report
        .phases
        .push(("reads".into(), t.elapsed().as_secs_f64()));

    // 5. Plan and launch.
    let t = Instant::now();
    let want = a.plan_cases - a.run_cases;
    let mut explicit: Vec<i64> = automated_numbers.iter().copied().take(want).collect();
    if explicit.len() < want {
        let more = (0..a.cases)
            .filter(|i| ctx.model.role(*i) == Role::Plain && numbers[*i] > 0)
            .map(|i| numbers[i])
            .take(want - explicit.len());
        explicit.extend(more);
    }
    let first: Vec<i64> = explicit.iter().copied().take(10_000).collect();
    let rb = api.rb(Method::POST, &format!("{base}/test-plans"))?.json(
        &json!({"kind": "managed", "name": format!("Bench plan {run}"),
            "caseNumbers": first, "filter": {"tags": [RUN_TAG]}}),
    );
    let plan = api
        .timed("plan.create", rb)
        .await
        .ok_or_else(|| anyhow!("could not create the managed plan"))?;
    let plan_id = plan.body["id"].as_str().unwrap_or_default().to_string();
    for chunk in explicit[first.len()..].chunks(10_000) {
        let rb = api
            .rb(Method::POST, &format!("{base}/test-plans/{plan_id}/items"))?
            .json(&json!({"add": chunk}));
        api.send(None, "plan items add", rb).await;
    }
    if let Some(p) = api
        .get("plan get", &format!("{base}/test-plans/{plan_id}"))
        .await
    {
        report.plan_cases = p["counts"]["total"].as_i64().unwrap_or(0);
    }
    let rb = api
        .rb(Method::POST, &format!("{base}/test-plans/{plan_id}/launches"))?
        .json(&json!({"name": format!("bench tm {run} plan"), "externalId": format!("bench-tm-{run}-plan"),
            "assign": "none", "includeAutomated": true}));
    let pl = api
        .timed("plan.launch", rb)
        .await
        .ok_or_else(|| anyhow!("could not start a launch from the plan"))?;
    let plan_launch = pl.body["launch"]["id"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    report
        .phases
        .push(("plan".into(), t.elapsed().as_secs_f64()));

    // 6. Manual run.
    let t = Instant::now();
    let mut items: Vec<String> = Vec::new();
    let mut before: Option<String> = None;
    loop {
        let mut path = format!("{base}/launches/{plan_launch}/run-items?limit=500");
        if let Some(b) = &before {
            path.push_str(&format!("&before={}", enc(b)));
        }
        let Some(page) = api.get("run items list", &path).await else {
            break;
        };
        for it in page["items"].as_array().into_iter().flatten() {
            if let Some(id) = it["id"].as_str() {
                items.push(id.to_string());
            }
        }
        match page["next"].as_str() {
            Some(n) if items.len() < a.run_cases => before = Some(n.to_string()),
            _ => break,
        }
    }
    items.truncate(a.run_cases);
    report.run_items = items.len();
    let items = Arc::new(items);
    let steps_written: Vec<usize> = stream::iter(0..a.testers)
        .map(|tester| {
            let c = ctx.clone();
            let items = items.clone();
            async move {
                let mut n = 0;
                let mut k = tester;
                while k < items.len() {
                    n += execute(&c, &items[k], k).await;
                    k += c.a.testers;
                }
                n
            }
        })
        .buffer_unordered(a.testers)
        .collect()
        .await;
    report.step_results = steps_written.iter().sum();
    report
        .phases
        .push(("manual run".into(), t.elapsed().as_secs_f64()));
    eprintln!(
        "manual run: {} items, {} step results in {:.1} s",
        report.run_items,
        report.step_results,
        t.elapsed().as_secs_f64()
    );

    // 7. Combined: the automated part into the plan launch, then close the manual run.
    let t = Instant::now();
    let tgz = allure_tar_gz(a.seed, a.upload_results, &auto_ids, now + 3_600_000)?;
    upload_and_wait(
        &ctx,
        &plan_launch,
        tgz,
        "combined.upload",
        "combined.ingest",
    )
    .await?;
    let rb = api
        .rb(
            Method::POST,
            &format!("{base}/launches/{plan_launch}/manual/close"),
        )?
        .json(&json!({"untestedTo": "skipped"}));
    api.timed("run.close", rb).await;
    let rb = api
        .rb(
            Method::POST,
            &format!("{base}/launches/{plan_launch}/finish"),
        )?
        .json(&json!({"status": "finished"}));
    api.send(None, "plan launch finish", rb).await;
    if let Some(l) = api
        .get("plan launch get", &format!("{base}/launches/{plan_launch}"))
        .await
    {
        report.manual = l["manual"].clone();
        report.launch_results = l["counts"]["total"].as_i64().unwrap_or(0);
    }
    report
        .phases
        .push(("combined".into(), t.elapsed().as_secs_f64()));

    // 8. Export and import.
    let t = Instant::now();
    let (lines, bytes) = export(&ctx).await;
    report.exported_cases = lines;
    report.exported_bytes = bytes;
    report
        .phases
        .push(("export".into(), t.elapsed().as_secs_f64()));
    if a.import_cases > 0 {
        let t = Instant::now();
        let imp = format!("{key}-imp");
        let rb = api
            .rb(Method::POST, "api/v1/projects")?
            .json(&json!({"key": imp, "name": format!("Bench TM import {key}")}));
        if api.timed("project.create", rb).await.is_some() {
            created.push(imp.clone());
            report.imported_cases = import(&ctx, &imp).await;
        }
        report
            .phases
            .push(("import".into(), t.elapsed().as_secs_f64()));
    }
    Ok(())
}

async fn upload_and_wait(
    c: &Ctx,
    launch: &str,
    tgz: Vec<u8>,
    op: &'static str,
    done_op: &'static str,
) -> anyhow::Result<()> {
    let t = Instant::now();
    let rb = c
        .api
        .rb(
            Method::POST,
            &format!("{}/launches/{launch}/uploads?format=allure", c.base),
        )?
        .header("content-type", "application/octet-stream")
        .body(tgz);
    let up = c
        .api
        .timed(op, rb)
        .await
        .ok_or_else(|| anyhow!("the allure-results upload was refused"))?;
    let id = up.body["id"].as_str().unwrap_or_default().to_string();
    let deadline = Instant::now() + Duration::from_secs(c.a.wait_timeout);
    loop {
        let list = c
            .api
            .get(
                "uploads list",
                &format!("{}/launches/{launch}/uploads", c.base),
            )
            .await;
        if let Some(u) = list
            .as_ref()
            .and_then(|l| l["items"].as_array())
            .and_then(|a| a.iter().find(|u| u["id"] == id.as_str()))
        {
            match u["status"].as_str() {
                Some("done") => break,
                Some("failed") => {
                    c.api
                        .rec
                        .error(format!("{op}: upload failed: {}", u["error"]));
                    break;
                }
                _ => {}
            }
        }
        if Instant::now() > deadline {
            bail!("the upload was not processed within {} s", c.a.wait_timeout);
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    c.api.rec.sample(done_op, ms_since(t));
    Ok(())
}

/// Numbers of the automated cases, sorted.
async fn automated_numbers(c: &Ctx) -> Option<Vec<i64>> {
    let mut out = Vec::new();
    let mut before: Option<String> = None;
    loop {
        let mut path = format!("{}/cases?automation=automated&limit=500", c.base);
        if let Some(b) = &before {
            path.push_str(&format!("&before={}", enc(b)));
        }
        let page = c.api.get("automated cases list", &path).await?;
        for it in page["items"].as_array().into_iter().flatten() {
            if let Some(n) = it["number"].as_i64() {
                out.push(n);
            }
        }
        match page["next"].as_str() {
            Some(n) => before = Some(n.to_string()),
            None => break,
        }
    }
    out.sort_unstable();
    Some(out)
}

/// Waits until code sync linked every uploaded test (`want` cases), or at least the
/// `ALLURE_ID` ones (`min`) with no change for 30 s (when the server does not create cases
/// from code).
async fn wait_sync(c: &Ctx, want: usize, min: usize) -> anyhow::Result<Vec<i64>> {
    let deadline = Instant::now() + Duration::from_secs(c.a.wait_timeout);
    let mut last = (0usize, Instant::now());
    loop {
        let nums = automated_numbers(c).await.unwrap_or_default();
        if nums.len() >= want {
            return Ok(nums);
        }
        if nums.len() != last.0 {
            last = (nums.len(), Instant::now());
        } else if nums.len() >= min && last.1.elapsed() > Duration::from_secs(30) {
            eprintln!(
                "code sync: {} automated cases, fewer than the {want} uploaded tests (cases from code off?)",
                nums.len()
            );
            return Ok(nums);
        }
        if Instant::now() > deadline {
            bail!(
                "code sync linked {} of {want} tests within {} s",
                nums.len(),
                c.a.wait_timeout
            );
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

async fn reads(c: &Arc<Ctx>, with_children: &[Uuid]) {
    let n = c.a.samples;
    let tree_parents: Arc<Vec<Option<Uuid>>> = Arc::new(
        std::iter::once(None)
            .chain(with_children.iter().copied().map(Some))
            .collect(),
    );
    stream::iter(0..n * 3)
        .map(|k| {
            let c = c.clone();
            let tree_parents = tree_parents.clone();
            async move {
                let mut rng = Rng::new(c.a.seed, STREAM_READ | k as u64);
                let (op, path) = match k % 3 {
                    0 => {
                        let filter = match rng.below(3) {
                            0 => match c.model.fields.first() {
                                Some(f) if !f.options.is_empty() => format!(
                                    "status=active&field={}",
                                    enc(&format!(
                                        "{}:{}",
                                        f.key,
                                        f.options[rng.below(f.options.len())]
                                    ))
                                ),
                                _ => "status=active&automation=manual".into(),
                            },
                            1 => format!("tag=tag-{}&automation=manual", rng.below(20)),
                            _ => match c.folders.get(rng.below(c.folders.len())) {
                                Some(f) => format!("folder={f}&recursive=true&status=active"),
                                None => "status=active&automation=manual".into(),
                            },
                        };
                        ("cases.list", format!("{}/cases?{filter}&limit=50", c.base))
                    }
                    1 => {
                        let parent = tree_parents[rng.below(tree_parents.len())];
                        (
                            "cases.tree",
                            match parent {
                                Some(p) => format!("{}/cases/tree?parent={p}", c.base),
                                None => format!("{}/cases/tree", c.base),
                            },
                        )
                    }
                    _ => (
                        "case.get",
                        format!("{}/cases/{}", c.base, 1 + rng.below(c.a.cases)),
                    ),
                };
                if let Ok(rb) = c.api.rb(Method::GET, &path) {
                    c.api.timed(op, rb).await;
                }
            }
        })
        .buffer_unordered(c.a.read_parallel)
        .collect::<Vec<()>>()
        .await;
}

async fn think(c: &Ctx, rng: &mut Rng) {
    if c.a.think_ms > 0 {
        let ms = rng.below(2 * c.a.think_ms as usize + 1) as u64;
        tokio::time::sleep(Duration::from_millis(ms)).await;
    }
}

/// One virtual tester executes one run item: opens it, writes every step, sets the final
/// status. Returns the steps written.
async fn execute(c: &Ctx, item: &str, k: usize) -> usize {
    let mut rng = Rng::new(c.a.seed, STREAM_RUN | k as u64);
    let Ok(rb) = c
        .api
        .rb(Method::GET, &format!("{}/run-items/{item}", c.base))
    else {
        return 0;
    };
    let Some(it) = c.api.timed("run.item.get", rb).await else {
        return 0;
    };
    let paths = step_paths(it.body["steps"].as_array().map_or(&[][..], |v| v));
    let mut revision = it
        .etag
        .as_deref()
        .and_then(|e| {
            e.trim_matches('"')
                .trim_start_matches('r')
                .parse::<i64>()
                .ok()
        })
        .or_else(|| it.body["revision"].as_i64())
        .unwrap_or(0);
    let outcome = match rng.below(100) {
        0..=84 => "passed",
        85..=94 => "failed",
        _ => "blocked",
    };
    let stop_at = if outcome == "passed" || paths.is_empty() {
        usize::MAX
    } else {
        rng.below(paths.len())
    };
    let mut written = 0;
    for (i, p) in paths.iter().enumerate() {
        think(c, &mut rng).await;
        let status = match i.cmp(&stop_at) {
            std::cmp::Ordering::Less => "passed",
            std::cmp::Ordering::Equal => outcome,
            std::cmp::Ordering::Greater => "skipped",
        };
        let mut body = json!({"status": status});
        if i == stop_at {
            body["actual"] = json!(format!("The {} did not appear", words(&mut rng, 2)));
        }
        let Ok(rb) = c.api.rb(
            Method::PUT,
            &format!("{}/run-items/{item}/steps/{p}", c.base),
        ) else {
            continue;
        };
        if let Some(r) = c.api.timed("run.step", rb.json(&body)).await {
            written += 1;
            revision = r.body["revision"].as_i64().unwrap_or(revision);
        }
    }
    think(c, &mut rng).await;
    let mut body = json!({"status": outcome});
    if outcome != "passed" {
        body["comment"] = json!(format!("Found while checking the {}", words(&mut rng, 2)));
    }
    if let Ok(rb) = c
        .api
        .rb(Method::PATCH, &format!("{}/run-items/{item}", c.base))
    {
        let rb = rb
            .header("if-match", format!("\"r{revision}\""))
            .json(&body);
        c.api.timed("run.final", rb).await;
    }
    written
}

/// Exports every case as NDJSON; returns the cases (lines after the header) and bytes.
async fn export(c: &Ctx) -> (usize, u64) {
    let t = Instant::now();
    let Ok(rb) = c.api.rb(
        Method::GET,
        &format!("{}/cases/export?format=ndjson", c.base),
    ) else {
        return (0, 0);
    };
    c.api.rec.request();
    let resp = match rb.send().await {
        Ok(r) if r.status().is_success() => r,
        Ok(r) => {
            let s = r.status();
            let text: String = r
                .text()
                .await
                .unwrap_or_default()
                .chars()
                .take(300)
                .collect();
            c.api
                .rec
                .error(format!("export.ndjson: {} {text}", s.as_u16()));
            return (0, 0);
        }
        Err(e) => {
            c.api.rec.error(format!("export.ndjson: {e}"));
            return (0, 0);
        }
    };
    let mut body = resp.bytes_stream();
    let (mut lines, mut bytes) = (0usize, 0u64);
    while let Some(chunk) = body.next().await {
        match chunk {
            Ok(b) => {
                bytes += b.len() as u64;
                lines += b.iter().filter(|x| **x == b'\n').count();
            }
            Err(e) => {
                c.api.rec.error(format!("export.ndjson: reading: {e}"));
                return (0, bytes);
            }
        }
    }
    c.api.rec.sample("export.ndjson", ms_since(t));
    (lines.saturating_sub(1), bytes)
}

/// Imports a generated TestRail CSV into project `key` (dry run, then apply); returns the
/// cases created.
async fn import(c: &Arc<Ctx>, key: &str) -> i64 {
    let base = format!("api/v1/projects/{}", enc(key));
    let boundary = format!("fastproofbench{}", Uuid::now_v7().simple());
    let options = json!({"format": "testrail-csv", "mode": "createOnly", "unknownFieldValues": "create",
        "stepsLayout": "cells"});
    let head = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"options\"\r\n\r\n{options}\r\n\
         --{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"testrail.csv\"\r\n\
         Content-Type: text/csv\r\n\r\n{}",
        testrail_header()
    );
    let tail = format!("\r\n--{boundary}--\r\n");
    let (seed, n, steps) = (c.a.seed, c.a.import_cases, c.a.steps);
    const CHUNK: usize = 500;
    let rows = stream::iter((0..n).step_by(CHUNK)).map(move |from| {
        Ok::<Bytes, std::io::Error>(Bytes::from(testrail_rows(
            seed,
            from,
            (from + CHUNK).min(n),
            steps,
        )))
    });
    let body = stream::once(async move { Ok::<Bytes, std::io::Error>(Bytes::from(head)) })
        .chain(rows)
        .chain(stream::once(async move {
            Ok::<Bytes, std::io::Error>(Bytes::from(tail))
        }));
    let t = Instant::now();
    let Ok(rb) = c.api.rb(Method::POST, &format!("{base}/imports")) else {
        return 0;
    };
    let rb = rb
        .header(
            "content-type",
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(reqwest::Body::wrap_stream(body));
    let Some(job) = c.api.send(None, "import start", rb).await else {
        return 0;
    };
    let id = job.body["id"].as_str().unwrap_or_default().to_string();
    let Some(j) = wait_job(c, &base, &id, "ready").await else {
        return 0;
    };
    let dry = ms_since(t);
    c.api.rec.sample("import.dryrun", dry);
    let report = &j["report"];
    if report["errors"].as_array().is_some_and(|e| !e.is_empty()) {
        c.api.rec.error(format!(
            "import dry run: {} row errors, first {}",
            report["errors"].as_array().map_or(0, Vec::len),
            report["errors"][0]
        ));
    }
    let t2 = Instant::now();
    let Ok(rb) = c
        .api
        .rb(Method::POST, &format!("{base}/imports/{id}/apply"))
    else {
        return 0;
    };
    if c.api.send(None, "import apply", rb).await.is_none() {
        return 0;
    }
    let Some(j) = wait_job(c, &base, &id, "done").await else {
        return 0;
    };
    c.api.rec.sample("import.apply", ms_since(t2));
    c.api.rec.sample("import.total", ms_since(t));
    j["report"]["create"].as_i64().unwrap_or(0)
}

async fn wait_job(c: &Ctx, base: &str, id: &str, want: &str) -> Option<Value> {
    let deadline = Instant::now() + Duration::from_secs(c.a.wait_timeout);
    loop {
        let j = c
            .api
            .get("import status", &format!("{base}/imports/{id}"))
            .await?;
        let st = j["status"].as_str().unwrap_or("");
        if st == want {
            return Some(j);
        }
        if matches!(st, "failed" | "cancelled" | "done") {
            c.api
                .rec
                .error(format!("import {id}: {st}: {}", j["report"]));
            return None;
        }
        if Instant::now() > deadline {
            c.api.rec.error(format!(
                "import {id}: not {want} within {} s",
                c.a.wait_timeout
            ));
            return None;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

/// Prints the report: the table to stdout, then the JSON line.
pub fn print(r: &TmBenchReport) -> anyhow::Result<()> {
    let mut out = std::io::stdout().lock();
    out.write_all(r.render().as_bytes())?;
    writeln!(out, "{}", serde_json::to_string(r)?)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args() -> TmBenchArgs {
        use clap::Parser;
        #[derive(Parser)]
        struct P {
            #[command(flatten)]
            a: TmBenchArgs,
        }
        P::try_parse_from(["x", "--url", "http://localhost:1"])
            .unwrap()
            .a
    }

    #[test]
    fn defaults_are_the_lab_scenario() {
        let a = args();
        assert_eq!(
            (
                a.cases,
                a.folders,
                a.folder_depth,
                a.fields,
                a.steps,
                a.versions
            ),
            (20_000, 300, 5, 30, 12, 5)
        );
        assert_eq!(
            (
                a.automated,
                a.plan_cases,
                a.run_cases,
                a.run_steps,
                a.testers,
                a.upload_results
            ),
            (2_000, 5_000, 2_000, 25, 50, 5_000)
        );
        assert_eq!((a.import_cases, a.parallel, a.samples), (20_000, 16, 200));
        validate(&a).unwrap();
        let mut bad = a.clone();
        bad.run_cases = 19_000;
        assert!(validate(&bad).is_err());
        bad = a.clone();
        bad.project = Some("Bad_Key".into());
        assert!(validate(&bad).is_err());
    }

    #[test]
    fn folder_tree_respects_depth_and_order() {
        for (n, d) in [(300, 5), (1, 1), (50, 2), (2000, 10)] {
            let p = folder_tree(7, n, d);
            assert_eq!(p.len(), n);
            let depth = folder_depths(&p);
            assert!(depth.iter().all(|x| (1..=d).contains(x)), "{n} {d}");
            assert!(p.iter().enumerate().all(|(i, q)| q.is_none_or(|q| q < i)));
            if n >= 300 && d >= 5 {
                assert_eq!(
                    *depth.iter().max().unwrap(),
                    d,
                    "the tree reaches the depth"
                );
            }
        }
        assert_eq!(folder_tree(7, 300, 5), folder_tree(7, 300, 5));
    }

    #[test]
    fn steps_count_and_paths() {
        let mut rng = Rng::new(1, 2);
        for n in [1, 2, 3, 12, 25, 100] {
            let s = steps(&mut rng, n);
            assert_eq!(count_steps(&s), n);
            let paths = step_paths(&s);
            assert_eq!(paths.len(), n);
            assert_eq!(paths[0], "1");
            assert!(paths.iter().all(|p| p.split('.').count() <= 2));
        }
        let s = vec![
            json!({"steps": [{"steps": []}, {"steps": []}]}),
            json!({"steps": []}),
        ];
        assert_eq!(step_paths(&s), vec!["1", "1.1", "1.2", "2"]);
    }

    #[test]
    fn cases_are_deterministic_valid_and_average_the_steps() {
        let a = args();
        let g = Gen::of(&a);
        let folders: Vec<Uuid> = (0..10).map(|i| Uuid::from_u64_pair(1, i)).collect();
        assert_eq!(g.case(5, 0, None, &folders), g.case(5, 0, None, &folders));
        assert_ne!(g.case(5, 0, None, &folders), g.case(5, 1, None, &folders));
        let mut total = 0;
        for i in 0..a.cases {
            total += g.step_count(i);
        }
        let mean = total as f64 / a.cases as f64;
        assert!((mean - 12.0).abs() < 1.0, "mean steps {mean}");
        let run = g.case(a.automated, 0, None, &folders);
        assert_eq!(g.role(a.automated), Role::Run);
        assert_eq!(count_steps(run["body"]["steps"].as_array().unwrap()), 25);
        assert!(run["tags"].as_array().unwrap().contains(&json!(RUN_TAG)));
        assert_eq!(run["status"], "active");
        let big = g.case(a.cases - 1, 1, Some(100), &folders);
        assert_eq!(count_steps(big["body"]["steps"].as_array().unwrap()), 100);
        assert!(serde_json::to_vec(&big).unwrap().len() < 64 * 1024);
        for i in 0..200 {
            let c = g.case(i, 0, None, &folders);
            assert!(c["name"].as_str().unwrap().len() <= 500);
            for (k, v) in c["fields"].as_object().unwrap() {
                let f = g.fields.iter().find(|f| &f.key == k).unwrap();
                match f.kind {
                    "select" => assert!(f.options.contains(&v.as_str().unwrap().to_string())),
                    "multiselect" => assert!(
                        v.as_array()
                            .unwrap()
                            .iter()
                            .all(|x| f.options.contains(&x.as_str().unwrap().to_string()))
                    ),
                    "number" => assert!(v.is_number()),
                    "date" => assert_eq!(v.as_str().unwrap().len(), 10),
                    _ => assert!(v.is_string()),
                }
            }
        }
        let save = g.save_range();
        assert!(save.start >= a.automated + a.run_cases && save.len() == a.save_sample);
        let ver = g.version_range();
        assert_eq!(ver.len(), a.version_sample);
    }

    #[test]
    fn fields_mix_types() {
        let f = field_specs(3, 30);
        assert_eq!(f.len(), 30);
        assert_eq!(f[0].kind, "select");
        for k in FIELD_KINDS {
            assert_eq!(f.iter().filter(|x| x.kind == k).count(), 6);
        }
        assert!(
            f.iter()
                .filter(|x| x.kind.ends_with("select"))
                .all(|x| x.options.len() >= 3)
        );
        assert!(f.iter().all(|x| x.write()["key"] == x.key.as_str()));
    }

    #[test]
    fn allure_archive_carries_allure_ids() {
        let tgz = allure_tar_gz(1, 20, &|j| (j < 5).then_some(j as i64 + 100), 0).unwrap();
        let mut ar = tar::Archive::new(flate2::read::GzDecoder::new(tgz.as_slice()));
        let mut ids = 0;
        let mut n = 0;
        for e in ar.entries().unwrap() {
            let mut e = e.unwrap();
            assert!(
                e.path()
                    .unwrap()
                    .to_string_lossy()
                    .ends_with("-result.json")
            );
            let v: Value = serde_json::from_reader(&mut e).unwrap();
            n += 1;
            if v["labels"]
                .as_array()
                .unwrap()
                .iter()
                .any(|l| l["name"] == "ALLURE_ID")
            {
                ids += 1;
            }
        }
        assert_eq!((n, ids), (20, 5));
        assert_eq!(allure_result(1, 3, None, 0), allure_result(1, 3, None, 0));
    }

    #[test]
    fn testrail_csv_has_one_row_per_case() {
        let mut s = testrail_header();
        s.push_str(&testrail_rows(1, 0, 30, 12));
        s.push_str(&testrail_rows(1, 30, 40, 12));
        let opts = fastproof_tm_import::ParseOptions {
            steps_layout: Some(fastproof_tm_import::StepsLayout::Cells),
            ..Default::default()
        };
        let (cases, errors) = fastproof_tm_import::parse_all(
            fastproof_tm_import::Format::TestrailCsv,
            s.as_bytes(),
            &opts,
        )
        .unwrap();
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(cases.len(), 40);
        assert!(
            cases
                .iter()
                .all(|c| c.steps.len() >= 6 && c.folder.len() == 3)
        );
        assert!(cases[0].keys.iter().any(|k| k.value == "C1"));
        assert_eq!(testrail_rows(1, 0, 30, 12), testrail_rows(1, 0, 30, 12));
    }

    #[test]
    fn percentiles_and_table() {
        let v: Vec<f64> = (1..=100).map(f64::from).collect();
        assert_eq!(percentile(&v, 50.0), 50.0);
        assert_eq!(percentile(&v, 95.0), 95.0);
        assert_eq!(percentile(&[7.0], 95.0), 7.0);
        assert_eq!(percentile(&[], 95.0), 0.0);
        let rec = Recorder::default();
        for i in 0..100 {
            rec.sample("run.step", f64::from(i) / 2.0);
        }
        rec.sample("plan.launch", 4000.0);
        let r = rows(&rec);
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].op, "plan.launch");
        assert_eq!(r[0].pass, Some(false));
        assert_eq!(r[1].pass, Some(true));
        let rep = TmBenchReport {
            ops: r,
            ..Default::default()
        };
        assert!(rep.render().contains("MISS"));
    }
}
