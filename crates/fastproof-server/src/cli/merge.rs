//! `fastproof launch merge` and `fastproof launch undo-merge`, and the launch a re-run joins
//! (`--rerun-of`).

use anyhow::{Context, bail};
use clap::Args;
use fastproof_store::Launch;
use fastproof_store::merges::LaunchMerge;
use reqwest::Method;
use uuid::Uuid;

use super::Conn;
use super::client::{Client, Items};

#[derive(Args, Debug)]
pub struct MergeArgs {
    #[command(flatten)]
    pub conn: Conn,
    /// Launches to merge: ids or numbers (`42` or `#42`), 2 to 20.
    #[arg(required = true, num_args = 1..)]
    pub launches: Vec<String>,
    /// Merge into this launch (one of the launches, or added to them) instead of a new one.
    #[arg(long)]
    pub into: Option<String>,
    /// Name of the new launch (default: the earliest launch's name).
    #[arg(long, conflicts_with = "into")]
    pub name: Option<String>,
    /// Show what the merge would do without merging.
    #[arg(long)]
    pub dry_run: bool,
    /// Merge without undo: the merged launches are deleted at once (needs --yes).
    #[arg(long = "final")]
    pub final_merge: bool,
    /// Confirm a merge without undo.
    #[arg(long)]
    pub yes: bool,
}

#[derive(Args, Debug)]
pub struct UndoMergeArgs {
    #[command(flatten)]
    pub conn: Conn,
    /// The merge id printed by `launch merge`.
    pub merge: Uuid,
}

fn path(project: &str, rest: &str) -> String {
    let p: String = project
        .bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.') {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect();
    format!("api/v1/projects/{p}/{rest}")
}

/// A launch named by id, `#N` or `N`; a merged launch resolves to the launch it went into.
pub async fn find_launch(client: &Client, project: &str, name: &str) -> anyhow::Result<Launch> {
    let name = name.trim();
    let launch = if let Ok(id) = name.parse::<Uuid>() {
        client.launch(project, id).await?
    } else {
        let n: i64 = name
            .trim_start_matches('#')
            .parse()
            .with_context(|| format!("{name:?}: expected a launch id or number"))?;
        let mut found = None;
        for debug in ["false", "true"] {
            let rb = client
                .request(Method::GET, &path(project, "launches"))?
                .query(&[
                    ("before", (n.saturating_add(1)).to_string().as_str()),
                    ("limit", "1"),
                    ("debug", debug),
                ]);
            let page: Items<Launch> = client.send(rb).await?;
            if let Some(l) = page.items.into_iter().find(|l| l.number == n) {
                found = Some(l);
                break;
            }
        }
        found.with_context(|| format!("launch #{n} not found"))?
    };
    let mut l = launch;
    for _ in 0..4 {
        match l.merged_into {
            Some(into) => {
                eprintln!(
                    "launch #{} was merged into another launch; using that one",
                    l.number
                );
                l = client.launch(project, into).await?;
            }
            None => break,
        }
    }
    Ok(l)
}

fn print_merge(m: &LaunchMerge) {
    let sources: Vec<String> = m
        .sources
        .iter()
        .map(|s| {
            s.number
                .map(|n| format!("#{n}"))
                .unwrap_or_else(|| s.id.to_string())
        })
        .collect();
    let target = m
        .target_number
        .map(|n| format!("#{n}"))
        .unwrap_or_else(|| "a new launch".into());
    let c = &m.counts;
    eprintln!(
        "merge {} {}: {} into {target}: {} attempts copied, {} already there, {} results, {} tests in several launches",
        m.id.map(|i| i.to_string())
            .unwrap_or_else(|| "(dry run)".into()),
        m.status,
        sources.join(", "),
        c.attempts,
        c.duplicates,
        c.results,
        c.shared_series
    );
    match (m.status.as_str(), m.undo_until) {
        ("active", Some(u)) => eprintln!(
            "undo until {}: fastproof launch undo-merge {}",
            u.format("%Y-%m-%d %H:%M UTC"),
            m.id.map(|i| i.to_string()).unwrap_or_default()
        ),
        ("final", _) => eprintln!("the merged launches are deleted; this merge cannot be undone"),
        _ => {}
    }
}

pub async fn merge(a: MergeArgs) -> anyhow::Result<i32> {
    if a.final_merge && !a.yes && !a.dry_run {
        bail!("--final deletes the merged launches and cannot be undone: add --yes to confirm");
    }
    let client = a.conn.client()?;
    let project = &a.conn.project;
    let mut ids = Vec::new();
    for l in &a.launches {
        let found = find_launch(&client, project, l).await?;
        if !ids.contains(&found.id) {
            ids.push(found.id);
        }
    }
    let into = match &a.into {
        Some(t) => {
            let t = find_launch(&client, project, t).await?.id;
            if !ids.contains(&t) {
                ids.push(t);
            }
            Some(t)
        }
        None => None,
    };
    let body = serde_json::json!({"launchIds": ids, "into": into, "name": a.name,
        "final": a.final_merge, "dryRun": a.dry_run});
    let rb = client
        .request(Method::POST, &path(project, "launches/merge"))?
        .json(&body);
    let m: LaunchMerge = client.send(rb).await?;
    print_merge(&m);
    if let Some(t) = m.target_id {
        println!("{t}");
    }
    Ok(0)
}

pub async fn undo(a: UndoMergeArgs) -> anyhow::Result<i32> {
    let client = a.conn.client()?;
    let rb = client.request(
        Method::POST,
        &path(&a.conn.project, &format!("merges/{}/undo", a.merge)),
    )?;
    let m: LaunchMerge = client.send(rb).await?;
    print_merge(&m);
    Ok(0)
}
