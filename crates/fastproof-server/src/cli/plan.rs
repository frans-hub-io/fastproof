//! `fastproof plan run`: starts a launch per environment from a managed plan and dispatches the
//! plan's automated tests to the project's CI.

use std::collections::BTreeMap;

use anyhow::{Context, bail};
use clap::{Args, Subcommand};
use reqwest::Method;
use serde_json::{Value, json};
use uuid::Uuid;

use super::Conn;
use super::client::Client;

#[derive(Subcommand, Debug)]
pub enum PlanCommand {
    /// Start launches from a managed plan and run its automated tests in the project's CI
    /// (`rerun` settings). Prints the external id of every launch; exits 1 when a dispatch failed.
    Run(PlanRunArgs),
}

#[derive(Args, Debug)]
pub struct PlanRunArgs {
    #[command(flatten)]
    pub conn: Conn,
    /// The managed plan.
    #[arg(long)]
    pub plan: Uuid,
    /// Environment name, repeatable: one launch and one CI run per environment.
    #[arg(long = "env", value_name = "NAME")]
    pub envs: Vec<String>,
    /// CI variable `NAME=VALUE` from the project's allowlist, repeatable.
    #[arg(long = "var", value_name = "NAME=VALUE")]
    pub vars: Vec<String>,
    /// Branch or tag the CI runs on (one of the project's allowed refs).
    #[arg(long = "ref")]
    pub git_ref: Option<String>,
    /// Launch name (default: `Plan run <plan id>`).
    #[arg(long)]
    pub name: Option<String>,
    /// Also create run items for the plan's manual cases.
    #[arg(long)]
    pub with_manual: bool,
}

/// `NAME=VALUE` pairs; a repeated name is an error.
pub fn parse_vars(vars: &[String]) -> anyhow::Result<BTreeMap<String, String>> {
    let mut out = BTreeMap::new();
    for v in vars {
        let (k, val) = v
            .split_once('=')
            .with_context(|| format!("--var {v:?}: expected NAME=VALUE"))?;
        if k.is_empty() {
            bail!("--var {v:?}: the name is empty");
        }
        if out.insert(k.to_string(), val.to_string()).is_some() {
            bail!("--var {k}: given twice");
        }
    }
    Ok(out)
}

/// The request body of `POST /test-plans/{id}/launches`.
pub fn request_body(a: &PlanRunArgs) -> anyhow::Result<Value> {
    let vars = parse_vars(&a.vars)?;
    let mut ci = json!({"dispatch": true});
    if !vars.is_empty() {
        ci["variables"] = json!(vars);
    }
    if let Some(r) = &a.git_ref {
        ci["ref"] = json!(r);
    }
    if !a.envs.is_empty() {
        ci["environments"] = json!(
            a.envs
                .iter()
                .map(|n| json!({"name": n}))
                .collect::<Vec<_>>()
        );
    }
    Ok(json!({
        "name": a.name.clone().unwrap_or_else(|| format!("Plan run {}", a.plan)),
        "includeAutomated": true,
        "includeManual": a.with_manual,
        "ci": ci,
    }))
}

pub async fn run(c: PlanCommand) -> anyhow::Result<i32> {
    match c {
        PlanCommand::Run(a) => run_plan(a).await,
    }
}

async fn run_plan(a: PlanRunArgs) -> anyhow::Result<i32> {
    let body = request_body(&a)?;
    let client = a.conn.client()?;
    let rb = client
        .request(
            Method::POST,
            &format!(
                "api/v1/projects/{}/test-plans/{}/launches",
                Client::enc(&a.conn.project),
                a.plan
            ),
        )?
        .json(&body);
    let out: Value = client.send(rb).await?;
    Ok(report(&out))
}

/// Prints the launch external ids (stdout) and the dispatch outcomes (stderr); the exit code.
pub fn report(out: &Value) -> i32 {
    let launches = out["launches"].as_array().cloned().unwrap_or_default();
    for l in &launches {
        println!("{}", l["externalId"].as_str().unwrap_or_default());
    }
    let mut failed = 0;
    for d in out["dispatches"].as_array().into_iter().flatten() {
        let number = launches
            .iter()
            .find(|l| l["id"] == d["launchId"])
            .map(|l| l["number"].to_string())
            .unwrap_or_default();
        let env = d["environment"].as_str().unwrap_or("-");
        if d["status"] == "dispatched" {
            eprintln!(
                "launch #{number} ({env}): dispatched to {}",
                d["provider"].as_str().unwrap_or_default()
            );
        } else {
            failed += 1;
            eprintln!(
                "launch #{number} ({env}): not dispatched: {}",
                d["error"].as_str().unwrap_or("unknown error")
            );
        }
    }
    i32::from(failed > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(vars: &[&str], envs: &[&str]) -> PlanRunArgs {
        PlanRunArgs {
            conn: Conn::default(),
            plan: Uuid::nil(),
            envs: envs.iter().map(|s| s.to_string()).collect(),
            vars: vars.iter().map(|s| s.to_string()).collect(),
            git_ref: Some("release/1".into()),
            name: Some("nightly".into()),
            with_manual: false,
        }
    }

    #[test]
    fn variables_split_at_the_first_equals_sign() {
        let v = parse_vars(&["BROWSER=firefox".into(), "SUITE=a=b".into()]).unwrap();
        assert_eq!(v["SUITE"], "a=b");
        assert!(parse_vars(&["NOEQUALS".into()]).is_err());
        assert!(parse_vars(&["=x".into()]).is_err());
        assert!(parse_vars(&["A=1".into(), "A=2".into()]).is_err());
    }

    #[test]
    fn the_body_asks_for_a_ci_start_without_manual_items() {
        let b = request_body(&args(&["BROWSER=firefox"], &["stage", "prod"])).unwrap();
        assert_eq!(b["includeManual"], false);
        assert_eq!(b["ci"]["dispatch"], true);
        assert_eq!(b["ci"]["ref"], "release/1");
        assert_eq!(b["ci"]["variables"]["BROWSER"], "firefox");
        assert_eq!(b["ci"]["environments"][1]["name"], "prod");
        let b = request_body(&args(&[], &[])).unwrap();
        assert!(b["ci"].get("variables").is_none() && b["ci"].get("environments").is_none());
    }

    #[test]
    fn a_failed_dispatch_makes_the_exit_code_one() {
        let ok = json!({"launches": [{"id": "l1", "number": 3, "externalId": "plan-a"}],
            "dispatches": [{"launchId": "l1", "status": "dispatched", "provider": "github"}]});
        assert_eq!(report(&ok), 0);
        let bad = json!({"launches": [{"id": "l1", "number": 3, "externalId": "plan-a"}],
            "dispatches": [{"launchId": "l1", "status": "failed", "error": "the CI answered HTTP 500"}]});
        assert_eq!(report(&bad), 1);
    }
}
