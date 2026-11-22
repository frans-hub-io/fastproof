//! `fastproof gate`: prints the quality gate of a launch and exits 0 (passed), 1 (failed) or
//! 2 (errors, or still pending when the wait is over).

use std::time::Duration;

use anyhow::anyhow;
use clap::Args;
use uuid::Uuid;

use super::Conn;
use super::ci;

#[derive(Args, Debug)]
pub struct GateArgs {
    #[command(flatten)]
    pub conn: Conn,
    /// External id of the launch (default: FASTPROOF_LAUNCH, else from CI).
    #[arg(long, env = "FASTPROOF_LAUNCH")]
    pub launch: Option<String>,
    /// Launch id (instead of the external id).
    #[arg(long)]
    pub id: Option<Uuid>,
    /// Wait while the gate is pending, at most this long (`90s`, `10m`, `1h`).
    #[arg(long, value_parser = parse_duration)]
    pub wait: Option<Duration>,
    /// Poll interval while waiting.
    #[arg(long, default_value = "5s", value_parser = parse_duration)]
    pub poll: Duration,
    /// Ignore the CI environment: the launch is not looked up by the CI pipeline's id.
    #[arg(long, env = "FASTPROOF_NO_CI", value_parser = clap::builder::BoolishValueParser::new())]
    pub no_ci: bool,
}

/// `90`, `90s`, `10m`, `1h`, `500ms`.
pub fn parse_duration(s: &str) -> Result<Duration, String> {
    let s = s.trim();
    let (num, unit) = match s.find(|c: char| !c.is_ascii_digit()) {
        Some(i) => s.split_at(i),
        None => (s, "s"),
    };
    let n: u64 = num
        .parse()
        .map_err(|_| format!("{s:?}: expected a number with s, m, h or ms"))?;
    let d = match unit {
        "ms" => Duration::from_millis(n),
        "s" => Duration::from_secs(n),
        "m" => Duration::from_secs(n * 60),
        "h" => Duration::from_secs(n * 3600),
        _ => return Err(format!("{s:?}: unit must be ms, s, m or h")),
    };
    if d > Duration::from_secs(24 * 3600) {
        return Err(format!("{s:?}: at most 24h"));
    }
    Ok(d)
}

/// Prints the gate and returns the exit code.
pub fn print(launch: &str, v: &serde_json::Value) -> i32 {
    let status = v["status"].as_str().unwrap_or("unknown");
    println!("quality gate of launch {launch}: {status}");
    if let Some(rs) = v["results"].as_array() {
        for r in rs {
            let rule = &r["rule"];
            let base = rule["baseline"]
                .as_str()
                .map(|b| format!(" vs {b}"))
                .unwrap_or_default();
            let actual = if r["actual"].is_null() {
                "n/a".to_string()
            } else {
                r["actual"].to_string()
            };
            println!(
                "  [{}] {}: {} {} {}{base} (actual {actual}){}",
                if r["ok"].as_bool() == Some(true) {
                    "ok"
                } else {
                    "FAIL"
                },
                r["gate"].as_str().unwrap_or_default(),
                rule["metric"].as_str().unwrap_or_default(),
                rule["op"].as_str().unwrap_or_default(),
                rule["value"],
                r["note"]
                    .as_str()
                    .map(|n| format!(" - {n}"))
                    .unwrap_or_default(),
            );
        }
    }
    match status {
        "passed" => 0,
        "failed" => 1,
        _ => 2,
    }
}

async fn check(a: &GateArgs) -> anyhow::Result<i32> {
    let client = a.conn.client()?;
    let project = &a.conn.project;
    let (id, label) = match a.id {
        Some(id) => (id, id.to_string()),
        None => {
            let ext = a
                .launch
                .clone()
                .or_else(|| {
                    (!a.no_ci)
                        .then(|| ci::detect(&|k| std::env::var(k).ok()))
                        .flatten()
                        .and_then(|c| c.external_id)
                })
                .ok_or_else(|| anyhow!("give --launch (FASTPROOF_LAUNCH), --id, or run in CI"))?;
            let l = client
                .launch_by_external(project, &ext)
                .await?
                .ok_or_else(|| anyhow!("no launch with external id {ext:?} in {project}"))?;
            (l.id, format!("#{} {}", l.number, l.name))
        }
    };
    let deadline = tokio::time::Instant::now() + a.wait.unwrap_or_default();
    loop {
        let v = client.gate(project, id).await?;
        let pending = v["status"] == "pending";
        if !pending || tokio::time::Instant::now() >= deadline {
            if pending {
                eprintln!("the gate is still pending (the launch runs or has unprocessed uploads)");
            }
            return Ok(print(&label, &v));
        }
        tokio::time::sleep(a.poll.max(Duration::from_millis(100))).await;
    }
}

/// Runs the command; errors print and exit 2.
pub async fn run(a: GateArgs) -> i32 {
    match check(&a).await {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e:#}");
            2
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn durations() {
        assert_eq!(parse_duration("10m").unwrap(), Duration::from_secs(600));
        assert_eq!(parse_duration("90").unwrap(), Duration::from_secs(90));
        assert_eq!(parse_duration("500ms").unwrap(), Duration::from_millis(500));
        assert!(parse_duration("10x").is_err());
        assert!(parse_duration("48h").is_err());
    }

    #[test]
    fn exit_codes() {
        assert_eq!(print("x", &json!({"status": "passed", "results": []})), 0);
        assert_eq!(
            print(
                "x",
                &json!({"status": "failed", "results": [{"gate": "g", "rule": {"metric": "failed", "op": "lte", "value": 0}, "actual": 2, "expected": 0, "ok": false}]})
            ),
            1
        );
        assert_eq!(print("x", &json!({"status": "pending", "results": []})), 2);
    }
}
