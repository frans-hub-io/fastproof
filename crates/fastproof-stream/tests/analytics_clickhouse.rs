//! ClickHouse schema version 2 and the Analysis reads against a real server (contract `76`
//! §7.9, §13 "C"): the schema from empty and from version 1, the triage mirror, the daily
//! aggregates, deletions, every read and parameters that try to break out of their literal.
//! Gated on `FASTPROOF_TEST_CLICKHOUSE_URL` (with `_USER` and `_PASSWORD_FILE`).

use chrono::{DateTime, Duration, NaiveDate, TimeZone, Utc};
use fastproof_model::{Status, Step};
use fastproof_store::analysis::types::{FailureGroupBy, RuleLogMatch};
use fastproof_store::logs::{LogChunk, LogLevel, LogLine, LogStream};
use fastproof_store::stream::{SimilarSource, SinkAttempt, SinkResult, SinkTriage};
use fastproof_stream::clickhouse::{ClickHouse, SCHEMA_VERSION, Scope, str_array};
use fastproof_stream::sink::{Deletions, Sink, SinkBatch};
use fastproof_stream::{ClickHouseConfig, Vars};
use uuid::Uuid;

fn server(db: &str) -> Option<ClickHouse> {
    std::env::var("FASTPROOF_TEST_CLICKHOUSE_URL").ok()?;
    let db = db.to_string();
    let get = move |k: &str| -> Option<String> {
        if k == "FASTPROOF_CLICKHOUSE_DATABASE" {
            return Some(db.clone());
        }
        let rest = k.strip_prefix("FASTPROOF_CLICKHOUSE_")?;
        std::env::var(format!("FASTPROOF_TEST_CLICKHOUSE_{rest}")).ok()
    };
    Some(ClickHouse::new(ClickHouseConfig::from_vars(&Vars(&get)).unwrap().unwrap()).unwrap())
}

async fn lines(ch: &ClickHouse, sql: &str) -> Vec<String> {
    ch.exec(sql, &[], None)
        .await
        .unwrap()
        .lines()
        .map(str::to_string)
        .collect()
}

/// The days of the fixtures' month.
fn october() -> Vec<NaiveDate> {
    (1..=31)
        .map(|d| NaiveDate::from_ymd_opt(2026, 10, d).unwrap())
        .collect()
}

fn day(d: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, d, 12, 0, 0).unwrap()
}

struct Fixture {
    launches: Vec<(Uuid, DateTime<Utc>)>,
    results: Vec<SinkResult>,
    attempts: Vec<SinkAttempt>,
}

fn result(
    project: &str,
    launch: (Uuid, DateTime<Utc>),
    series: Uuid,
    status: &str,
    fingerprint: Option<&str>,
    duration: i64,
) -> SinkResult {
    SinkResult {
        id: Uuid::now_v7(),
        project: project.into(),
        project_id: Uuid::nil(),
        launch_id: launch.0,
        launch_number: 1,
        launch_created_at: launch.1,
        series_id: series,
        test_case_id: series,
        name: format!("case {series}"),
        full_name: format!("suite.Case#{series}"),
        status: status.into(),
        flaky_in_run: false,
        attempt_count: 1,
        started_at: Some(launch.1),
        duration_ms: Some(duration),
        message: fingerprint.map(|f| format!("Connection refused to db {f}")),
        fingerprint: fingerprint.map(str::to_string),
        branch: Some("main".into()),
        env_key: Some("linux".into()),
        updated_at: launch.1,
        defect_type_id: None,
        defect_group: String::new(),
        defect_source: String::new(),
        muted: false,
        canonical_fingerprint: fingerprint.map(str::to_string),
        suite: "api / orders".into(),
    }
}

fn fixture(project: &str) -> Fixture {
    let launches: Vec<(Uuid, DateTime<Utc>)> = (1..=20).map(|d| (Uuid::now_v7(), day(d))).collect();
    let slow = Uuid::now_v7();
    let steady = Uuid::now_v7();
    let mut results = Vec::new();
    let mut attempts = Vec::new();
    for (i, l) in launches.iter().enumerate() {
        let fp = if i % 2 == 0 {
            "aaaaaaaaaaaaaaaa"
        } else {
            "bbbbbbbbbbbbbbbb"
        };
        let r = result(
            project,
            *l,
            slow,
            "failed",
            Some(fp),
            if i >= 10 { 300 } else { 100 },
        );
        for seq in 0..2 {
            attempts.push(SinkAttempt {
                id: Uuid::now_v7(),
                project: project.into(),
                project_id: Uuid::nil(),
                launch_id: l.0,
                launch_created_at: l.1,
                result_id: r.id,
                series_id: slow,
                client_key: format!("r:{i}#{seq}"),
                seq,
                status: if seq == 0 { "failed" } else { "passed" }.into(),
                started_at: Some(l.1),
                duration_ms: Some(40),
                message: None,
                trace: None,
                steps: vec![Step {
                    name: "open page".into(),
                    status: Status::Passed,
                    duration_ms: Some(10 + i as i64),
                    ..Default::default()
                }],
                fingerprint: Some(fp.into()),
                frames: vec!["a.b(c)".into()],
                trace_id: Some(format!("{:032x}", i)),
            });
        }
        results.push(r);
        let mut ok = result(project, *l, steady, "passed", None, 50);
        ok.flaky_in_run = i % 5 == 0;
        results.push(ok);
    }
    Fixture {
        launches,
        results,
        attempts,
    }
}

fn triage_row(
    r: &SinkResult,
    id: i64,
    batch: Uuid,
    group: &str,
    type_id: Uuid,
    via: &str,
) -> SinkTriage {
    SinkTriage {
        decision_id: id,
        project: r.project.clone(),
        project_id: Uuid::nil(),
        launch_id: r.launch_id,
        launch_created_at: r.launch_created_at,
        result_id: r.id,
        batch_id: batch,
        fingerprint: r.fingerprint.clone().unwrap_or_default(),
        via: via.into(),
        source: "user".into(),
        by: "alice".into(),
        at: r.launch_created_at + Duration::hours(1),
        before_type_id: None,
        before_group: String::new(),
        after_type_id: Some(type_id),
        after_group: group.into(),
        message_norm: "connection refused to db".into(),
        test_case_id: r.test_case_id,
        branch: "main".into(),
        env_key: "linux".into(),
        reverts_batch: None,
    }
}

#[tokio::test]
async fn schema_from_empty_and_from_version_one() {
    let db = format!("fptest_chv_{:08x}", rand::random::<u32>());
    let Some(ch) = server(&db) else {
        eprintln!("FASTPROOF_TEST_CLICKHOUSE_URL is not set: skipped");
        return;
    };
    ch.prepare().await.unwrap();
    ch.prepare().await.unwrap();
    assert_eq!(ch.schema_version().await.unwrap(), SCHEMA_VERSION);
    let tables = lines(
        &ch,
        &format!("SELECT name FROM system.tables WHERE database = '{db}' ORDER BY name"),
    )
    .await;
    assert_eq!(
        tables,
        [
            "attempts",
            "defect_daily",
            "fingerprint_daily",
            "fingerprint_daily_mv",
            "logs",
            "results",
            "schema_version",
            "series_daily",
            "series_daily_mv",
            "steps",
            "triage"
        ]
    );
    assert_eq!(
        lines(
            &ch,
            &format!("SELECT version FROM {db}.schema_version ORDER BY version")
        )
        .await,
        ["1", "2"]
    );

    // Back to version 1: the new objects and columns gone, no version recorded, one old row.
    for t in [
        "triage",
        "fingerprint_daily_mv",
        "fingerprint_daily",
        "defect_daily",
        "schema_version",
    ] {
        ch.exec(&format!("DROP TABLE {db}.{t}"), &[], None)
            .await
            .unwrap();
    }
    ch.exec(
        &format!(
            "ALTER TABLE {db}.results DROP COLUMN defect_type_id, DROP COLUMN defect_group, \
             DROP COLUMN defect_source, DROP COLUMN muted, DROP COLUMN canonical_fingerprint, DROP COLUMN suite"
        ),
        &[],
        None,
    )
    .await
    .unwrap();
    ch.exec(
        &format!("ALTER TABLE {db}.attempts DROP COLUMN fingerprint, DROP COLUMN frames, DROP COLUMN trace_id"),
        &[],
        None,
    )
    .await
    .unwrap();
    ch.exec(
        &format!(
            "INSERT INTO {db}.results (project, project_id, launch_id, launch_number, launch_created_at, id, \
             series_id, test_case_id, name, full_name, status, flaky_in_run, attempt_count, message, message_norm, \
             match_key, fingerprint, branch, env_key, version) VALUES ('old', generateUUIDv4(), generateUUIDv4(), 1, \
             now64(3), generateUUIDv4(), generateUUIDv4(), generateUUIDv4(), 'n', 'f', 'failed', false, 1, 'm', 'm', \
             '', 'cccccccccccccccc', '', '', 1)"
        ),
        &[],
        None,
    )
    .await
    .unwrap();
    assert_eq!(ch.schema_version().await.unwrap_or(0), 0);
    ch.prepare().await.unwrap();
    ch.prepare().await.unwrap();
    assert_eq!(ch.schema_version().await.unwrap(), 2);
    assert_eq!(
        lines(
            &ch,
            &format!(
                "SELECT project, defect_group, muted, canonical_fingerprint, suite FROM {db}.results FORMAT TSV"
            )
        )
        .await,
        ["old\t\tfalse\t\t"]
    );
    assert_eq!(
        lines(
            &ch,
            &format!(
                "SELECT name FROM system.columns WHERE database = '{db}' AND table = 'attempts' \
                 AND name IN ('fingerprint', 'frames', 'trace_id') ORDER BY name"
            )
        )
        .await,
        ["fingerprint", "frames", "trace_id"]
    );
    ch.exec(&format!("DROP DATABASE {db}"), &[], None)
        .await
        .unwrap();
}

#[tokio::test]
async fn sink_rows_aggregates_reads_and_deletion() {
    let db = format!("fptest_cha_{:08x}", rand::random::<u32>());
    let Some(ch) = server(&db) else {
        return;
    };
    ch.prepare().await.unwrap();
    let f = fixture("shop");
    let other = fixture("other");
    for fx in [&f, &other] {
        let b = SinkBatch {
            results: fx.results.clone(),
            attempts: fx.attempts.clone(),
            ..SinkBatch::default()
        };
        ch.write(&b, &b.token()).await.unwrap();
    }
    let logs = SinkBatch {
        logs: vec![LogChunk {
            id: 1,
            project_id: Uuid::nil(),
            project_key: "shop".into(),
            launch_id: f.launches[3].0,
            launch_created_at: f.launches[3].1,
            lines: vec![
                LogLine {
                    result_id: "t3".into(),
                    attempt: 0,
                    seq: 1,
                    ts: f.launches[3].1.timestamp_millis(),
                    level: LogLevel::Error,
                    stream: LogStream::Stderr,
                    message: "FATAL: Disk FULL on /var".into(),
                    step: None,
                },
                LogLine {
                    result_id: "t3".into(),
                    attempt: 0,
                    seq: 2,
                    ts: f.launches[3].1.timestamp_millis(),
                    level: LogLevel::Info,
                    stream: LogStream::Stdout,
                    message: "disk full again".into(),
                    step: None,
                },
            ],
        }],
        ..SinkBatch::default()
    };
    ch.write(&logs, &logs.token()).await.unwrap();

    let sc = Scope {
        project: "shop",
        from: day(1) - Duration::hours(12),
        to: day(21),
        branch: None,
        env: None,
    };

    // Failures by fingerprint, with an alias merging b into a.
    let (total, groups, days) = ch
        .failures(&sc, FailureGroupBy::Fingerprint, &[], 10)
        .await
        .unwrap();
    assert_eq!(total, 20);
    assert_eq!(
        groups
            .iter()
            .map(|g| (g.key.as_str(), g.failures))
            .collect::<Vec<_>>(),
        [("aaaaaaaaaaaaaaaa", 10), ("bbbbbbbbbbbbbbbb", 10)]
    );
    assert_eq!(days.len(), 20);
    let merged = vec![(
        "bbbbbbbbbbbbbbbb".to_string(),
        "aaaaaaaaaaaaaaaa".to_string(),
    )];
    let (_, groups, _) = ch
        .failures(&sc, FailureGroupBy::Fingerprint, &merged, 10)
        .await
        .unwrap();
    assert_eq!(
        groups
            .iter()
            .map(|g| (g.key.as_str(), g.failures, g.launches, g.tests))
            .collect::<Vec<_>>(),
        [("aaaaaaaaaaaaaaaa", 20, 20, 1)]
    );
    let (_, groups, _) = ch
        .failures(&sc, FailureGroupBy::Suite, &[], 10)
        .await
        .unwrap();
    assert_eq!(groups[0].key, "api / orders");
    let (_, groups, _) = ch
        .failures(&sc, FailureGroupBy::Env, &[], 10)
        .await
        .unwrap();
    assert_eq!(groups[0].key, "linux");
    let none = Scope {
        branch: Some("release"),
        ..sc.clone()
    };
    assert_eq!(
        ch.failures(&none, FailureGroupBy::Env, &[], 10)
            .await
            .unwrap()
            .0,
        0
    );

    // Triage: three results become product bugs, written again with their defect.
    let pb = Uuid::now_v7();
    let batch = Uuid::now_v7();
    let mut changed = Vec::new();
    let mut decisions = Vec::new();
    for (i, r) in f
        .results
        .iter()
        .filter(|r| r.status == "failed")
        .take(3)
        .enumerate()
    {
        let mut r = r.clone();
        r.defect_type_id = Some(pb);
        r.defect_group = "productBug".into();
        r.defect_source = "user".into();
        r.updated_at += Duration::hours(1);
        decisions.push(triage_row(
            &r,
            100 + i as i64,
            batch,
            "productBug",
            pb,
            "api",
        ));
        changed.push(r);
    }
    let b = SinkBatch {
        results: changed.clone(),
        triage: decisions.clone(),
        ..SinkBatch::default()
    };
    ch.write(&b, &b.token()).await.unwrap();
    ch.write(&b, &b.token()).await.unwrap();
    assert_eq!(
        lines(&ch, &format!("SELECT count() FROM {db}.triage FINAL")).await,
        ["3"]
    );
    for p in ["shop", "other"] {
        ch.rollup_defects(p, &october()).await.unwrap();
    }
    let (total, groups, days) = ch
        .failures(&sc, FailureGroupBy::DefectGroup, &[], 10)
        .await
        .unwrap();
    assert_eq!(total, 20);
    assert_eq!(
        groups
            .iter()
            .map(|g| (g.key.as_str(), g.failures))
            .collect::<Vec<_>>(),
        [("toInvestigate", 17), ("productBug", 3)]
    );
    assert_eq!(
        days.iter()
            .filter(|d| d.0 == "productBug")
            .map(|d| d.2)
            .sum::<i64>(),
        3
    );
    assert_eq!(
        days.iter()
            .filter(|d| d.0 == "toInvestigate")
            .map(|d| d.2)
            .sum::<i64>(),
        17
    );
    // A rollup after the first result moved back writes 0 for the group it left.
    let mut back = changed[0].clone();
    back.defect_type_id = None;
    back.defect_group = String::new();
    back.updated_at += Duration::hours(1);
    let b = SinkBatch {
        results: vec![back],
        ..SinkBatch::default()
    };
    ch.write(&b, &b.token()).await.unwrap();
    ch.rollup_defects("shop", &october()).await.unwrap();
    let (_, _, days) = ch
        .failures(&sc, FailureGroupBy::DefectGroup, &[], 10)
        .await
        .unwrap();
    assert_eq!(
        days.iter()
            .filter(|d| d.0 == "productBug")
            .map(|d| d.2)
            .sum::<i64>(),
        2
    );

    // History: the decided fingerprint, one row per type; an undone batch teaches nothing.
    let fps = vec![
        "aaaaaaaaaaaaaaaa".to_string(),
        "bbbbbbbbbbbbbbbb".to_string(),
    ];
    let h = ch.history("shop", &fps, 3).await.unwrap();
    assert!(!h.is_empty());
    assert!(
        h.iter()
            .all(|m| m.defect_type_id == pb && m.group == "productBug")
    );
    assert_eq!(h.iter().map(|m| m.confirmed).sum::<i32>(), h.len() as i32);
    assert!(ch.history("other", &fps, 3).await.unwrap().is_empty());
    let mut undo = triage_row(&changed[0], 200, Uuid::now_v7(), "", pb, "undo");
    undo.reverts_batch = Some(batch);
    let b = SinkBatch {
        triage: vec![undo],
        ..SinkBatch::default()
    };
    ch.write(&b, &b.token()).await.unwrap();
    assert!(ch.history("shop", &fps, 3).await.unwrap().is_empty());

    // Error timeline from fingerprint_daily (aliases are passed by the caller).
    let t = ch
        .error_timeline("shop", &fps, day(1) - Duration::hours(12), day(20))
        .await
        .unwrap();
    assert_eq!(t.len(), 20);
    assert!(t.iter().all(|p| p.occurrences == 1 && p.launches == 1));
    let t = ch
        .error_timeline("shop", &fps[..1], day(1) - Duration::hours(12), day(20))
        .await
        .unwrap();
    assert_eq!(t.iter().map(|p| p.occurrences).sum::<i64>(), 10);

    // Durations: the slow series went from 100 to 300 ms in the last 10 days.
    let to = day(20) + Duration::hours(1);
    let dsc = Scope {
        from: to - Duration::days(35),
        to,
        ..sc.clone()
    };
    let d = ch
        .durations(&dsc, to - Duration::days(10), 10)
        .await
        .unwrap();
    assert_eq!(d.len(), 1, "{d:?}");
    assert_eq!(
        (d[0].p50_recent_ms, d[0].p50_before_ms, d[0].ratio),
        (300, 100, 3.0)
    );
    assert!(
        ch.durations(&dsc, to - Duration::days(3), 10)
            .await
            .unwrap()
            .is_empty()
    );

    // Retries, flaky trend, slow steps.
    let (wasted, items) = ch.retries(&sc, 10).await.unwrap();
    assert_eq!(wasted, 20 * 40);
    assert_eq!(
        (items[0].suite.as_str(), items[0].retries, items[0].tests),
        ("api / orders", 20, 1)
    );
    let ft = ch.flaky_trend(&sc).await.unwrap();
    assert_eq!(ft.iter().map(|p| p.flaky).sum::<i64>(), 4);
    assert_eq!(ft.iter().map(|p| p.runs).sum::<i64>(), 40);
    let steps = ch.slow_steps(&sc, 5).await.unwrap();
    assert_eq!((steps[0].name.as_str(), steps[0].count), ("open page", 40));
    assert_eq!((steps[0].p50_ms, steps[0].p95_ms), (19, 29));

    // Log matches for a retro apply: regex and contains, levels and streams.
    let shop_launches: Vec<Uuid> = f.launches.iter().map(|l| l.0).collect();
    let m = ch
        .log_matches(
            "shop",
            &RuleLogMatch {
                contains: Some("disk full".into()),
                ..Default::default()
            },
            day(1) - Duration::days(1),
            &shop_launches,
            10,
        )
        .await
        .unwrap();
    assert_eq!(m.len(), 1);
    assert_eq!((m[0].result_key.as_str(), m[0].seq), ("t3", 2));
    let m = ch
        .log_matches(
            "shop",
            &RuleLogMatch {
                regex: Some("(?i)disk\\s+full".into()),
                levels: Some(vec![LogLevel::Error]),
                ..Default::default()
            },
            day(1) - Duration::days(1),
            &shop_launches,
            10,
        )
        .await
        .unwrap();
    assert_eq!(m.len(), 1);
    assert_eq!(m[0].seq, 1);
    assert!(
        ch.log_matches(
            "other",
            &RuleLogMatch {
                contains: Some("disk".into()),
                ..Default::default()
            },
            day(1) - Duration::days(1),
            &shop_launches,
            10
        )
        .await
        .unwrap()
        .is_empty()
    );

    // Series runs with their attempts' fingerprints, oldest first.
    let slow = f.results[0].series_id;
    let runs = ch
        .series_runs("shop", slow, day(1) - Duration::days(1), 100)
        .await
        .unwrap();
    assert_eq!(runs.len(), 20);
    assert!(runs[0].created_at < runs[19].created_at);
    assert_eq!(
        runs[0].attempt_fingerprints,
        ["aaaaaaaaaaaaaaaa", "aaaaaaaaaaaaaaaa"]
    );
    assert!(
        ch.series_runs("other", slow, day(1), 100)
            .await
            .unwrap()
            .is_empty()
    );

    // Portfolio and cross-project similar only see the listed projects.
    let p = ch
        .portfolio(&["shop".to_string()], sc.from, sc.to)
        .await
        .unwrap();
    assert_eq!(p.len(), 1);
    assert_eq!(
        (
            p[0].launches,
            p[0].ran,
            p[0].passed,
            p[0].failures,
            p[0].flaky
        ),
        (20, 40, 20, 20, 1)
    );
    let src = SimilarSource {
        id: Uuid::now_v7(),
        project_id: Uuid::nil(),
        message: Some("Connection refused to db aaaaaaaaaaaaaaaa".into()),
        fingerprint: Some("aaaaaaaaaaaaaaaa".into()),
    };
    let sim = ch
        .similar_across(&["other".to_string()], &src, 5)
        .await
        .unwrap();
    assert!(!sim.is_empty() && sim.iter().all(|s| s.project == "other"));
    assert_eq!(sim[0].score, 1.0);
    assert!(ch.similar_across(&[], &src, 5).await.unwrap().is_empty());

    // Deleting a launch drops its rows everywhere and rebuilds the days' aggregates.
    let gone = f.launches[0];
    ch.delete(&Deletions {
        launches: vec![("shop".into(), gone.0)],
        projects: vec![],
    })
    .await
    .unwrap();
    for t in ["results", "attempts", "steps", "triage"] {
        assert_eq!(
            lines(
                &ch,
                &format!(
                    "SELECT count() FROM {db}.{t} WHERE launch_id = '{}'",
                    gone.0
                )
            )
            .await,
            ["0"],
            "{t}"
        );
    }
    let t = ch
        .error_timeline("shop", &fps, day(1) - Duration::hours(12), day(20))
        .await
        .unwrap();
    assert_eq!(t[0].occurrences, 0);
    assert_eq!(t.iter().map(|p| p.occurrences).sum::<i64>(), 19);
    // Deleting a project drops its aggregates and history; the other project keeps its own.
    ch.delete(&Deletions {
        launches: vec![],
        projects: vec![("shop".into(), Utc::now())],
    })
    .await
    .unwrap();
    for t in ["results", "triage", "fingerprint_daily", "defect_daily"] {
        assert_eq!(
            lines(
                &ch,
                &format!("SELECT count() FROM {db}.{t} WHERE project = 'shop'")
            )
            .await,
            ["0"],
            "{t}"
        );
    }
    assert_eq!(
        lines(
            &ch,
            &format!("SELECT count() FROM {db}.results WHERE project = 'other'")
        )
        .await,
        ["40"]
    );
    ch.exec(&format!("DROP DATABASE {db}"), &[], None)
        .await
        .unwrap();
}

#[tokio::test]
async fn parameters_never_leave_their_literal() {
    let db = format!("fptest_chi_{:08x}", rand::random::<u32>());
    let Some(ch) = server(&db) else {
        return;
    };
    ch.prepare().await.unwrap();
    let f = fixture("shop");
    let b = SinkBatch {
        results: f.results.clone(),
        ..SinkBatch::default()
    };
    ch.write(&b, &b.token()).await.unwrap();
    let corpus = [
        "' OR 1=1 --",
        "main' UNION ALL SELECT name FROM system.tables --",
        "\\' OR '1'='1",
        "x'); DROP TABLE results; --",
        "a\\",
        "line\nbreak\ttab\0nul",
        "{p:String}",
        "]['",
        "shop",
    ];
    let sc = Scope {
        project: "shop",
        from: day(1) - Duration::days(1),
        to: day(21),
        branch: None,
        env: None,
    };
    for bad in corpus {
        for (branch, env) in [(Some(bad), None), (None, Some(bad))] {
            let s = Scope {
                branch,
                env,
                ..sc.clone()
            };
            let (total, groups, _) = ch.failures(&s, FailureGroupBy::Env, &[], 10).await.unwrap();
            assert_eq!((total, groups.len()), (0, 0), "{bad:?}");
        }
        let s = Scope {
            project: bad,
            ..sc.clone()
        };
        if bad != "shop" {
            assert_eq!(
                ch.failures(&s, FailureGroupBy::Fingerprint, &[], 10)
                    .await
                    .unwrap()
                    .0,
                0
            );
            assert!(
                ch.portfolio(&[bad.to_string()], sc.from, sc.to)
                    .await
                    .unwrap()
                    .is_empty()
            );
        }
        let aliases = vec![(bad.to_string(), "zzzz".to_string())];
        let (_, groups, _) = ch
            .failures(&sc, FailureGroupBy::Fingerprint, &aliases, 10)
            .await
            .unwrap();
        assert_eq!(
            groups.iter().map(|g| g.failures).sum::<i64>(),
            20,
            "{bad:?}"
        );
        assert!(
            ch.history("shop", &[bad.to_string()], 3)
                .await
                .unwrap()
                .is_empty()
        );
        let m = ch
            .log_matches(
                "shop",
                &RuleLogMatch {
                    contains: Some(bad.into()),
                    ..Default::default()
                },
                day(1),
                &[f.launches[0].0],
                10,
            )
            .await
            .unwrap();
        assert!(m.is_empty());
        let lit = str_array(&[bad]);
        let back = ch
            .exec(
                "SELECT arrayJoin({a:Array(String)}) FORMAT TSVRaw",
                &[("a", lit)],
                None,
            )
            .await
            .unwrap();
        assert_eq!(back.strip_suffix('\n').unwrap_or(&back), bad, "{bad:?}");
    }
    assert_eq!(
        lines(&ch, &format!("SELECT count() FROM {db}.results")).await,
        ["40"]
    );
    ch.exec(&format!("DROP DATABASE {db}"), &[], None)
        .await
        .unwrap();
}

/// Lines of a log chunk of one launch of the fixture, one per result key.
fn chunk(
    project: &str,
    id: i64,
    launch: (Uuid, DateTime<Utc>),
    keys: &[&str],
    message: &str,
) -> LogChunk {
    LogChunk {
        id,
        project_id: Uuid::nil(),
        project_key: project.into(),
        launch_id: launch.0,
        launch_created_at: launch.1,
        lines: keys
            .iter()
            .map(|k| LogLine {
                result_id: (*k).into(),
                attempt: 0,
                seq: 1,
                ts: launch.1.timestamp_millis(),
                level: LogLevel::Error,
                stream: LogStream::Stderr,
                message: message.into(),
                step: None,
            })
            .collect(),
    }
}

#[tokio::test]
async fn a_dropped_read_stops_on_the_server_and_reads_cannot_write() {
    let db = format!("fptest_chc_{:08x}", rand::random::<u32>());
    let Some(ch) = server(&db) else {
        return;
    };
    ch.prepare().await.unwrap();
    let marker = format!("fpcancel{:08x}", rand::random::<u32>());
    let slow = format!(
        "SELECT /*{marker}*/ sum(cityHash64(number)) AS n FROM numbers(1000000000000) FORMAT JSONEachRow"
    );
    let dropped = tokio::time::timeout(
        std::time::Duration::from_millis(1500),
        ch.rows::<serde_json::Value>(&slow, &[]),
    )
    .await;
    assert!(dropped.is_err(), "the slow read should still run");
    let running = format!(
        "SELECT count() FROM system.processes WHERE query LIKE '%{marker}%' AND query NOT LIKE '%system.processes%'"
    );
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    loop {
        let n = lines(&ch, &running).await;
        if n == ["0"] {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the read kept running on the server after its caller went away"
        );
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
    let write = ch
        .rows::<serde_json::Value>(
            &format!("INSERT INTO {db}.schema_version (component, version, applied_at) VALUES ('x', 9, now64(3))"),
            &[],
        )
        .await;
    assert!(write.is_err(), "a read query wrote");
    assert_eq!(
        lines(
            &ch,
            &format!("SELECT count() FROM {db}.schema_version WHERE component = 'x'")
        )
        .await,
        ["0"]
    );
    let big = ch
        .rows::<serde_json::Value>(
            "SELECT repeat('x', 1000000) AS s FROM numbers(140) FORMAT JSONEachRow",
            &[],
        )
        .await;
    assert!(
        format!("{:#}", big.unwrap_err()).contains("larger than"),
        "an answer above the cap was read into memory"
    );
    ch.exec(&format!("DROP DATABASE {db}"), &[], None)
        .await
        .unwrap();
}

#[tokio::test]
async fn retro_log_matches_read_only_the_task_launches() {
    let db = format!("fptest_chl_{:08x}", rand::random::<u32>());
    let Some(ch) = server(&db) else {
        return;
    };
    ch.prepare().await.unwrap();
    let f = fixture("shop");
    let long = format!("disk full {}", "y".repeat(5000));
    let keys: Vec<String> = (0..50).map(|i| format!("k{i}")).collect();
    let keys: Vec<&str> = keys.iter().map(String::as_str).collect();
    let b = SinkBatch {
        logs: vec![
            chunk("shop", 1, f.launches[0], &["old"], "disk full"),
            chunk("shop", 2, f.launches[19], &keys, &long),
        ],
        ..SinkBatch::default()
    };
    ch.write(&b, &b.token()).await.unwrap();
    let m = RuleLogMatch {
        contains: Some("disk full".into()),
        ..Default::default()
    };
    // The newest launch has more matches than the cap: the task's older launch is still read.
    let got = ch
        .log_matches(
            "shop",
            &m,
            day(1) - Duration::days(1),
            &[f.launches[0].0],
            10,
        )
        .await
        .unwrap();
    assert_eq!(got.len(), 1);
    assert_eq!(
        (got[0].launch_id, got[0].result_key.as_str()),
        (f.launches[0].0, "old")
    );
    let got = ch
        .log_matches(
            "shop",
            &m,
            day(1) - Duration::days(1),
            &[f.launches[19].0],
            100,
        )
        .await
        .unwrap();
    assert_eq!(got.len(), 50);
    assert!(
        got.iter()
            .all(|x| x.excerpt.chars().count() == fastproof_stream::clickhouse::LOG_MATCH_CHARS)
    );
    assert!(
        ch.log_matches("shop", &m, day(1) - Duration::days(1), &[], 10)
            .await
            .unwrap()
            .is_empty()
    );
    ch.exec(&format!("DROP DATABASE {db}"), &[], None)
        .await
        .unwrap();
}

#[tokio::test]
async fn rollups_and_deletion_rebuild_only_their_project_and_days() {
    let db = format!("fptest_chr_{:08x}", rand::random::<u32>());
    let Some(ch) = server(&db) else {
        return;
    };
    ch.prepare().await.unwrap();
    let f = fixture("shop");
    let other = fixture("other");
    for fx in [&f, &other] {
        let b = SinkBatch {
            results: fx.results.clone(),
            ..SinkBatch::default()
        };
        ch.write(&b, &b.token()).await.unwrap();
    }
    let versions = |p: &'static str| {
        let ch = ch.clone();
        let db = db.clone();
        async move {
            lines(
                &ch,
                &format!(
                    "SELECT day, max(version) FROM {db}.defect_daily WHERE project = '{p}' \
                     GROUP BY day ORDER BY day FORMAT TSV"
                ),
            )
            .await
        }
    };
    let first: Vec<NaiveDate> = (1..=10)
        .map(|d| NaiveDate::from_ymd_opt(2026, 10, d).unwrap())
        .collect();
    ch.rollup_defects("shop", &first).await.unwrap();
    assert_eq!(versions("shop").await.len(), 10, "only the days asked");
    assert!(versions("other").await.is_empty(), "only the project asked");
    assert_eq!(
        ch.projects_since(NaiveDate::from_ymd_opt(2026, 10, 1).unwrap())
            .await
            .unwrap(),
        ["other", "shop"]
    );
    ch.rollup_defects("shop", &october()).await.unwrap();
    ch.rollup_defects("other", &october()).await.unwrap();
    let before = versions("shop").await;
    let before_other = versions("other").await;
    assert_eq!(before.len(), 20);
    // Deleting the launch of 5 October rebuilds that day only (not every day up to now).
    let gone = f.launches[4];
    ch.delete(&Deletions {
        launches: vec![("shop".into(), gone.0)],
        projects: vec![],
    })
    .await
    .unwrap();
    let after = versions("shop").await;
    assert_eq!(after.len(), 20);
    for (b, a) in before.iter().zip(&after) {
        if b.starts_with("2026-10-05") {
            assert_ne!(b, a, "the deleted launch's day is rebuilt");
        } else {
            assert_eq!(b, a, "another day was recomputed");
        }
    }
    assert_eq!(versions("other").await, before_other);
    assert_eq!(
        lines(
            &ch,
            &format!(
                "SELECT sum(results) FROM {db}.defect_daily FINAL WHERE project = 'shop' AND day = '2026-10-05'"
            )
        )
        .await,
        ["0"]
    );
    ch.exec(&format!("DROP DATABASE {db}"), &[], None)
        .await
        .unwrap();
}
