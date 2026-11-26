//! FPQL on a real PostgreSQL: every case attribute against a seeded project, the structured
//! filter and its FPQL matching the same cases, the 5 s statement timeout, validation with
//! the capped count, saved queries (visibility, sharing, cycles, limits), bulk, export and
//! plans by query. They need `FASTPROOF_TEST_DATABASE_URL`; without it they pass without doing
//! anything.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use fastproof_store::projects::CreateProject;
use fastproof_store::tm::cases::{CaseCtx, CaseQuery};
use fastproof_store::tm::query::QueryCtx;
use fastproof_store::tm::{
    self, CaseBody, CaseBulk, CaseBulkAction, CaseFilter, CaseStatus, QueryEntity, ResultSource,
    SavedQueryPatch, SavedQueryWrite, TestPlanWrite, TmError, TmLimits,
};
use fastproof_store::{Project, Store};
use sqlx::PgPool;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use uuid::Uuid;

#[path = "support/testdb.rs"]
mod testdb;

struct TestDb {
    admin: PgConnectOptions,
    name: String,
    pool: PgPool,
}

impl TestDb {
    async fn migrated() -> Option<(Self, Store)> {
        let admin = testdb::admin()?;
        let name = testdb::create(&admin).await;
        let pool = PgPoolOptions::new()
            .max_connections(6)
            .connect_with(admin.clone().database(&name))
            .await
            .unwrap();
        fastproof_store::migrate(&pool).await.unwrap();
        let store = Store::new(pool.clone());
        Some((Self { admin, name, pool }, store))
    }
}

impl Drop for TestDb {
    fn drop(&mut self) {
        drop(self.pool.close());
        testdb::drop_database(&self.admin, &self.name);
    }
}

async fn project(store: &Store, key: &str) -> Project {
    match store.create_project(key, key, "").await.unwrap() {
        CreateProject::Created(p) => *p,
        CreateProject::Exists => panic!("project {key} exists"),
    }
}

async fn exec(pool: &PgPool, sql: &str, project: Uuid) {
    sqlx::query(sqlx::AssertSqlSafe(sql.to_string()))
        .bind(project)
        .execute(pool)
        .await
        .unwrap_or_else(|e| panic!("{sql}: {e}"));
}

async fn folder(
    pool: &PgPool,
    project: Uuid,
    parent: Option<(Uuid, Vec<Uuid>)>,
    name: &str,
) -> (Uuid, Vec<Uuid>) {
    let id = Uuid::now_v7();
    let mut path = parent.as_ref().map(|p| p.1.clone()).unwrap_or_default();
    path.push(id);
    sqlx::query(
        "insert into case_folders (id, project_id, parent_id, name, path_ids, depth) values ($1, $2, $3, $4, $5, $6)",
    )
    .bind(id)
    .bind(project)
    .bind(parent.map(|p| p.0))
    .bind(name)
    .bind(&path)
    .bind((path.len() - 1) as i16)
    .execute(pool)
    .await
    .unwrap();
    (id, path)
}

struct Seed {
    p: Project,
    payments: Uuid,
    cards: Uuid,
    other: Uuid,
    plan: Uuid,
}

/// Five cases (TC-4 archived) with every attribute FPQL reads.
async fn seed(store: &Store, pool: &PgPool, key: &str) -> Seed {
    let p = project(store, key).await;
    let pid = p.id;
    let payments = folder(pool, pid, None, "Payments").await;
    let cards = folder(pool, pid, Some(payments.clone()), "Cards").await;
    let other = folder(pool, pid, None, "Other").await;
    for (k, n, t, opts) in [
        (
            "prio",
            "Importance",
            "select",
            r#"[{"value":"high"},{"value":"low"},{"value":"critical"}]"#,
        ),
        ("points", "Points", "number", "[]"),
        ("due", "Due", "date", "[]"),
    ] {
        sqlx::query(
            "insert into case_field_defs (id, project_id, key, name, type, options) values ($1, $2, $3, $4, $5, $6::jsonb)",
        )
        .bind(Uuid::now_v7())
        .bind(pid)
        .bind(k)
        .bind(n)
        .bind(t)
        .bind(opts)
        .execute(pool)
        .await
        .unwrap();
    }
    let mut conn = pool.acquire().await.unwrap();
    let body = CaseBody::default();
    let names = [
        ("Login page works", Some(cards.0)),
        ("Checkout total", Some(payments.0)),
        ("Refund flow", Some(other.0)),
        ("Archived thing", None),
        ("Plain case", None),
    ];
    let mut ids = Vec::new();
    for (n, f) in names {
        let c = tm::testutil::insert_case(&mut conn, pid, n, &body, f)
            .await
            .unwrap();
        ids.push(c.id);
    }
    drop(conn);
    let set = |n: usize, sql: &str| {
        let sql = format!("update cases set {sql} where project_id = $1 and number = {n}");
        let pool = pool.clone();
        async move { exec(&pool, &sql, pid).await }
    };
    set(
        1,
        "fields = '{\"prio\":\"high\",\"points\":5,\"due\":\"2026-03-01\"}', tags = '{smoke,ui}', created_by = 'alice', \
         search_text = 'Login page works with a password', review_state = 'approved', state = 'active', \
         last_status = 'failed', last_status_at = now() - interval '1 day', last_source = 'automated'",
    )
    .await;
    set(
        2,
        "status = 'draft', automated = true, fields = '{\"prio\":\"low\",\"points\":12}', tags = '{api}', \
         created_by = 'bob', origin = 'code', review_state = 'pending', state = 'draft', last_status = 'passed', \
         last_status_at = now() - interval '20 days', last_source = 'manual', \
         last_trusted_at = now() - interval '40 days', search_text = 'Checkout total', \
         body = body || '{\"parameters\": {\"columns\": [\"a\"], \"rows\": []}}'",
    )
    .await;
    set(
        3,
        "status = 'outdated', automated = true, fields = '{\"prio\":\"critical\"}', created_by = null, \
         origin = 'ai', ai_unreviewed = true, search_text = 'Refund flow'",
    )
    .await;
    set(4, "archived_at = now(), tags = '{smoke}'").await;
    set(
        5,
        "created_at = '2025-12-31T23:30:00Z', updated_at = '2025-12-31T23:30:00Z', search_text = 'Plain case'",
    )
    .await;
    let req = Uuid::now_v7();
    sqlx::query(
        "insert into requirements (id, project_id, number, title, path_ids, depth) values ($1, $2, 1, 'Login', $3, 0)",
    )
    .bind(req)
    .bind(pid)
    .bind(vec![req])
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "insert into requirement_links (requirement_id, case_id, project_id, origin, created_by) values ($1, $2, $3, 'ui', 't')",
    )
    .bind(req)
    .bind(ids[0])
    .bind(pid)
    .execute(pool)
    .await
    .unwrap();
    let ss = Uuid::now_v7();
    sqlx::query("insert into shared_steps (id, project_id, number, name, steps) values ($1, $2, 1, 'Sign in', '[]')")
        .bind(ss)
        .bind(pid)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query(
        "insert into shared_step_usages (shared_step_id, case_id, project_id) values ($1, $2, $3)",
    )
    .bind(ss)
    .bind(ids[0])
    .bind(pid)
    .execute(pool)
    .await
    .unwrap();
    let plan = store
        .create_managed_plan(
            pid,
            &TestPlanWrite {
                name: "Regression".into(),
                case_numbers: Some(vec![1]),
                ..Default::default()
            },
            "alice",
            &TmLimits::default(),
        )
        .await
        .unwrap();
    Seed {
        p,
        payments: payments.0,
        cards: cards.0,
        other: other.0,
        plan: plan.id,
    }
}

async fn numbers(store: &Store, pid: Uuid, query: &str, me: &str) -> Result<Vec<i64>, TmError> {
    let l = store
        .list_cases(
            pid,
            &CaseQuery {
                query: Some(query.to_string()),
                me: me.to_string(),
                owner: if me.is_empty() {
                    String::new()
                } else {
                    format!("user:{me}")
                },
                limit: 100,
                ..Default::default()
            },
        )
        .await?;
    Ok(l.items.iter().map(|c| c.number).collect())
}

/// A signed-in user as a query caller.
fn who(name: &str) -> QueryCtx {
    QueryCtx::caller(name, &format!("user:{name}"))
}

fn query_error(e: TmError) -> tm::QueryError {
    match e {
        TmError::Invalid {
            code: "invalid_query",
            details: Some(d),
            ..
        } => serde_json::from_value(d).unwrap(),
        e => panic!("expected invalid_query, got {e}"),
    }
}

#[tokio::test]
async fn every_case_attribute_against_a_seeded_project() {
    let Some((db, store)) = TestDb::migrated().await else {
        return;
    };
    let s = seed(&store, &db.pool, "fq").await;
    let pid = s.p.id;
    let smoke = store
        .create_saved_query(
            pid,
            &who("alice"),
            true,
            &SavedQueryWrite {
                entity: QueryEntity::Cases,
                name: "Smoke".into(),
                query: "tag = smoke".into(),
                shared: Some(true),
                position: None,
            },
            &TmLimits::default(),
        )
        .await
        .unwrap();
    assert!(smoke.shared && smoke.mine);
    let cases: Vec<(String, Vec<i64>)> = vec![
        ("number = TC-2".into(), vec![2]),
        ("number in [1, 3]".into(), vec![1, 3]),
        ("number > 3".into(), vec![5]),
        ("name ~ \"PAGE\"".into(), vec![1]),
        ("name = \"Refund flow\"".into(), vec![3]),
        ("name !~ \"flow\"".into(), vec![1, 2, 5]),
        ("text ~ password".into(), vec![1]),
        ("text !~ password".into(), vec![2, 3, 5]),
        ("status = active".into(), vec![1, 5]),
        ("status in [draft, outdated]".into(), vec![2, 3]),
        ("state = active".into(), vec![1]),
        ("state is empty".into(), vec![3, 5]),
        ("state != draft".into(), vec![1, 3, 5]),
        ("automated = true".into(), vec![2, 3]),
        ("automated != true".into(), vec![1, 5]),
        ("folder = \"Payments\"".into(), vec![2]),
        ("folder in [subtree(\"payments\")]".into(), vec![1, 2]),
        ("folder not in subtree(Payments)".into(), vec![3, 5]),
        ("folder = subtree(\"Payments / Cards\")".into(), vec![1]),
        ("folder is empty".into(), vec![5]),
        ("folder != Payments".into(), vec![1, 3, 5]),
        (format!("folder = \"{}\"", s.other), vec![3]),
        (
            format!("folder in [\"{}\", \"{}\"]", s.cards, s.payments),
            vec![1, 2],
        ),
        ("tag = smoke".into(), vec![1]),
        ("tag in [api, ui]".into(), vec![1, 2]),
        ("tag ~ smo".into(), vec![1]),
        ("tag is empty".into(), vec![3, 5]),
        ("tag != smoke".into(), vec![2, 3, 5]),
        ("field[\"importance\"] = high".into(), vec![1]),
        ("field[prio] in [low, critical]".into(), vec![2, 3]),
        ("field[points] > 10".into(), vec![2]),
        ("field[\"Points\"] <= 5".into(), vec![1]),
        ("field[points] = 12".into(), vec![2]),
        ("field[due] < 2026-04-01".into(), vec![1]),
        ("field[due] = 2026-03-01".into(), vec![1]),
        ("field[prio] is empty".into(), vec![5]),
        ("field[prio] is not empty".into(), vec![1, 2, 3]),
        ("field[prio] ~ hig".into(), vec![1]),
        ("createdBy = me()".into(), vec![1]),
        ("createdBy is empty".into(), vec![3]),
        ("createdBy != alice".into(), vec![2, 3, 5]),
        ("not createdBy = alice".into(), vec![2, 3, 5]),
        ("createdBy in [alice, bob]".into(), vec![1, 2]),
        ("createdAt < 2026-01-01".into(), vec![5]),
        ("createdAt = 2025-12-31".into(), vec![5]),
        ("createdAt <= 2025-12-31".into(), vec![5]),
        ("createdAt > 2025-12-31".into(), vec![1, 2, 3]),
        ("createdAt > now() - 1h".into(), vec![1, 2, 3]),
        ("updatedAt >= 2026-01-01".into(), vec![1, 2, 3]),
        ("origin = ai".into(), vec![3]),
        ("origin in [ui, code]".into(), vec![1, 2, 5]),
        ("aiUnreviewed = true".into(), vec![3]),
        ("review = pending".into(), vec![2]),
        ("review is empty".into(), vec![3, 5]),
        ("review != approved".into(), vec![2, 3, 5]),
        ("lastStatus = failed".into(), vec![1]),
        ("lastStatus is empty".into(), vec![3, 5]),
        ("lastStatus in [passed, failed]".into(), vec![1, 2]),
        ("lastRunAt < now() - 7d".into(), vec![2]),
        ("lastRunAt > now() - 48h".into(), vec![1]),
        ("lastRunAt is empty".into(), vec![3, 5]),
        ("lastSource = manual".into(), vec![2]),
        ("requirement = REQ-1".into(), vec![1]),
        ("requirement = REQ-99".into(), vec![]),
        ("requirement is empty".into(), vec![2, 3, 5]),
        ("sharedStep = SS-1".into(), vec![1]),
        ("sharedStep is not empty".into(), vec![1]),
        ("sharedStep != 1".into(), vec![2, 3, 5]),
        ("plan = \"regression\"".into(), vec![1]),
        (format!("plan = \"{}\"", s.plan), vec![1]),
        ("plan != Regression".into(), vec![2, 3, 5]),
        ("hasParameters = true".into(), vec![2]),
        ("hasParameters = false".into(), vec![1, 3, 5]),
        ("staleDays > 30".into(), vec![2, 3]),
        ("staleDays >= 50".into(), vec![3]),
        ("staleDays < 50".into(), vec![2]),
        ("archived = true".into(), vec![4]),
        ("archived = false and tag = smoke".into(), vec![1]),
        ("filter = \"Smoke\"".into(), vec![1]),
        ("not filter = Smoke".into(), vec![2, 3, 5]),
        ("filter != Smoke and automated = true".into(), vec![2, 3]),
        (
            "(status = active or automated = true) and not tag = smoke".into(),
            vec![2, 3, 5],
        ),
        ("".into(), vec![1, 2, 3, 5]),
    ];
    for (q, want) in &cases {
        let got = numbers(&store, pid, q, "alice")
            .await
            .unwrap_or_else(|e| panic!("{q}: {e}"));
        assert_eq!(&got, want, "{q}");
    }

    for (q, at, len) in [
        ("folder = \"Nope/Cards\"", 9, 12),
        ("field[nope] = x", 0, 11),
        ("field[points] = abc", 16, 3),
        ("field[prio] > 3", 0, 15),
        ("filter = \"Missing\"", 9, 9),
        ("tag = a and createdBy = me()", 24, 4),
    ] {
        let me = if q.contains("me()") { "" } else { "alice" };
        let e = query_error(numbers(&store, pid, q, me).await.unwrap_err());
        assert_eq!((e.position, e.length), (at, len), "{q}: {e:?}");
    }
    let e = query_error(numbers(&store, pid, "tag = ", "alice").await.unwrap_err());
    assert_eq!(e.position, 6);
    let many = format!("folder in [{}]", vec!["\"Payments\""; 257].join(", "));
    let e = query_error(numbers(&store, pid, &many, "alice").await.unwrap_err());
    assert!(e.message.contains("256 folder names"), "{e:?}");

    // The query is ANDed with the structured filter and the trash switch.
    let l = store
        .list_cases(
            pid,
            &CaseQuery {
                filter: CaseFilter {
                    folder_ids: Some(vec![s.payments]),
                    recursive: Some(true),
                    ..Default::default()
                },
                query: Some("automated = false".into()),
                limit: 10,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(
        l.items.iter().map(|c| c.number).collect::<Vec<_>>(),
        vec![1]
    );
    let l = store
        .list_cases(
            pid,
            &CaseQuery {
                archived: true,
                query: Some("tag = smoke".into()),
                limit: 10,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(
        l.items.iter().map(|c| c.number).collect::<Vec<_>>(),
        vec![4]
    );

    // A folder or plan of another project is unknown here.
    let t = seed(&store, &db.pool, "fq2").await;
    let e = query_error(
        numbers(&store, pid, &format!("folder = \"{}\"", t.other), "alice")
            .await
            .unwrap_err(),
    );
    assert_eq!(e.message, "unknown folder");
    assert_eq!(
        numbers(&store, pid, &format!("plan = \"{}\"", t.plan), "alice")
            .await
            .unwrap(),
        Vec::<i64>::new()
    );
}

#[tokio::test]
async fn structured_filters_and_their_fpql_match_the_same_cases() {
    let Some((db, store)) = TestDb::migrated().await else {
        return;
    };
    let s = seed(&store, &db.pool, "eq").await;
    let pid = s.p.id;
    let fields = |k: &str, v: &[&str]| -> Option<BTreeMap<String, Vec<String>>> {
        Some(
            [(k.to_string(), v.iter().map(|x| x.to_string()).collect())]
                .into_iter()
                .collect(),
        )
    };
    let filters = vec![
        CaseFilter {
            statuses: Some(vec![CaseStatus::Active]),
            ..Default::default()
        },
        CaseFilter {
            automation: Some(ResultSource::Automated),
            ..Default::default()
        },
        CaseFilter {
            automation: Some(ResultSource::Manual),
            tags: Some(vec!["smoke".into(), "ui".into()]),
            ..Default::default()
        },
        CaseFilter {
            folder_ids: Some(vec![s.payments]),
            recursive: Some(true),
            ..Default::default()
        },
        CaseFilter {
            folder_ids: Some(vec![s.payments, s.other]),
            ..Default::default()
        },
        CaseFilter {
            fields: fields("prio", &["high", "low"]),
            ..Default::default()
        },
        CaseFilter {
            fields: fields("points", &[]),
            ..Default::default()
        },
        CaseFilter {
            fields: fields("points", &["12"]),
            ..Default::default()
        },
        CaseFilter {
            q: Some("password".into()),
            ..Default::default()
        },
        CaseFilter {
            q: Some("TC-2".into()),
            ..Default::default()
        },
        CaseFilter {
            q: Some("flow".into()),
            statuses: Some(vec![CaseStatus::Outdated, CaseStatus::Draft]),
            folder_ids: Some(vec![s.other]),
            recursive: Some(false),
            ..Default::default()
        },
    ];
    for f in filters {
        let by_filter: Vec<i64> = store
            .list_cases(
                pid,
                &CaseQuery {
                    filter: f.clone(),
                    limit: 100,
                    ..Default::default()
                },
            )
            .await
            .unwrap()
            .items
            .iter()
            .map(|c| c.number)
            .collect();
        let fpql = tm::query::from_filter(&f).normalized();
        let by_query = numbers(&store, pid, &fpql, "alice").await.unwrap();
        assert_eq!(by_filter, by_query, "{f:?} as {fpql}");
        assert!(!by_filter.is_empty(), "{fpql} should match something");
    }
}

#[tokio::test]
async fn queries_stop_after_five_seconds() {
    let Some((db, store)) = TestDb::migrated().await else {
        return;
    };
    let p = project(&store, "slow").await;
    let mut locker = db.pool.begin().await.unwrap();
    sqlx::query("lock table cases in access exclusive mode")
        .execute(&mut *locker)
        .await
        .unwrap();
    let t = Instant::now();
    let r = tokio::time::timeout(
        Duration::from_secs(60),
        numbers(&store, p.id, "name ~ \"slow\"", "alice"),
    )
    .await
    .expect("the statement timeout ends the query");
    let took = t.elapsed();
    let e = query_error(r.unwrap_err());
    assert!(e.message.contains("5s"), "{e:?}");
    assert!(
        took >= Duration::from_millis(4500) && took < Duration::from_secs(30),
        "{took:?}"
    );
    let r = store
        .validate_query(p.id, QueryEntity::Cases, "name ~ \"slow\"", &who("alice"))
        .await
        .unwrap();
    assert!(!r.ok && r.error.unwrap().message.contains("5s"));
    locker.rollback().await.unwrap();
    assert_eq!(
        numbers(&store, p.id, "name ~ \"slow\"", "alice")
            .await
            .unwrap(),
        Vec::<i64>::new()
    );
}

#[tokio::test]
async fn validation_counts_up_to_the_cap() {
    let Some((db, store)) = TestDb::migrated().await else {
        return;
    };
    let p = project(&store, "cnt").await;
    exec(
        &db.pool,
        "insert into cases (id, project_id, number, name, origin, body) \
         select gen_random_uuid(), $1, g, 'bulk ' || g, 'ui', '{\"steps\": []}'::jsonb from generate_series(1, 10005) g",
        p.id,
    )
    .await;
    let v = store
        .validate_query(p.id, QueryEntity::Cases, "Name ~ 'bulk'", &who("alice"))
        .await
        .unwrap();
    assert!(v.ok);
    assert_eq!(v.normalized.as_deref(), Some("name ~ \"bulk\""));
    assert_eq!((v.count, v.count_capped), (Some(10_000), Some(true)));
    let v = store
        .validate_query(p.id, QueryEntity::Cases, "number <= 12", &who("alice"))
        .await
        .unwrap();
    assert_eq!((v.count, v.count_capped), (Some(12), Some(false)));
    let v = store
        .validate_query(p.id, QueryEntity::Cases, "number <= ", &who("alice"))
        .await
        .unwrap();
    assert!(!v.ok && v.normalized.is_none());
    assert_eq!(v.error.unwrap().position, 10);
    let v = store
        .validate_query(p.id, QueryEntity::Cases, "folder = \"x\"", &who("alice"))
        .await
        .unwrap();
    assert_eq!(v.error.unwrap().message, "unknown folder \"x\"");
    let v = store
        .validate_query(
            p.id,
            QueryEntity::Requirements,
            "title ~ \"Log\"",
            &who("alice"),
        )
        .await
        .unwrap();
    assert_eq!(
        (v.ok, v.count),
        (true, Some(0)),
        "requirement queries count requirements"
    );
    let schema = store.query_schema(p.id, QueryEntity::Cases).await.unwrap();
    assert_eq!(schema.attributes.len(), 26);
    let status = schema
        .attributes
        .iter()
        .find(|a| a.name == "status")
        .unwrap();
    assert_eq!(
        status.values.as_deref(),
        Some(&["draft".to_string(), "active".into(), "outdated".into()][..])
    );
    assert!(schema.keywords.contains(&"and".to_string()));
    assert_eq!(
        store
            .query_schema(p.id, QueryEntity::Requirements)
            .await
            .unwrap()
            .attributes
            .len(),
        8
    );
}

#[tokio::test]
async fn saved_queries_visibility_sharing_cycles_and_limits() {
    let Some((db, store)) = TestDb::migrated().await else {
        return;
    };
    let s = seed(&store, &db.pool, "sq").await;
    let pid = s.p.id;
    let limits = TmLimits {
        max_saved_queries: 3,
        ..Default::default()
    };
    let w = |name: &str, query: &str, shared: bool| SavedQueryWrite {
        entity: QueryEntity::Cases,
        name: name.into(),
        query: query.into(),
        shared: Some(shared),
        position: None,
    };
    let mine = store
        .create_saved_query(
            pid,
            &who("alice"),
            false,
            &w("Mine", "automated = true", false),
            &limits,
        )
        .await
        .unwrap();
    assert!(!mine.shared && mine.mine && mine.position == 0);
    let e = store
        .create_saved_query(
            pid,
            &who("alice"),
            false,
            &w("Team", "tag = smoke", true),
            &limits,
        )
        .await
        .unwrap_err();
    assert!(matches!(e, TmError::Forbidden { .. }), "{e}");
    let team = store
        .create_saved_query(
            pid,
            &who("alice"),
            true,
            &w("Team", "tag = smoke", true),
            &limits,
        )
        .await
        .unwrap();
    assert_eq!(team.position, 1);
    let e = store
        .create_saved_query(
            pid,
            &who("alice"),
            true,
            &w("Team", "tag = ui", false),
            &limits,
        )
        .await
        .unwrap_err();
    assert!(
        matches!(
            e,
            TmError::Conflict {
                code: "saved_query_name_taken",
                ..
            }
        ),
        "{e}"
    );
    let e = store
        .create_saved_query(
            pid,
            &who("alice"),
            true,
            &w("Bad", "tag ~ a", false),
            &limits,
        )
        .await
        .unwrap_err();
    assert!(
        matches!(
            e,
            TmError::Invalid {
                code: "invalid_query",
                ..
            }
        ),
        "{e}"
    );

    // Bob sees the shared one only; Alice's personal one is 404 for him.
    let names = |l: tm::SavedQueryList| l.items.iter().map(|q| q.name.clone()).collect::<Vec<_>>();
    assert_eq!(
        names(
            store
                .list_saved_queries(pid, QueryEntity::Cases, &who("bob"))
                .await
                .unwrap()
        ),
        vec!["Team"]
    );
    assert_eq!(
        names(
            store
                .list_saved_queries(pid, QueryEntity::Cases, &who("alice"))
                .await
                .unwrap()
        ),
        vec!["Mine", "Team"]
    );
    for can_write in [false, true] {
        let e = store
            .update_saved_query(
                pid,
                mine.id,
                &who("bob"),
                can_write,
                &SavedQueryPatch::default(),
                &limits,
            )
            .await
            .unwrap_err();
        assert!(matches!(e, TmError::NotFound(_)), "{e}");
        let e = store
            .delete_saved_query(pid, mine.id, &who("bob"), can_write)
            .await
            .unwrap_err();
        assert!(matches!(e, TmError::NotFound(_)), "{e}");
    }
    let e = store
        .update_saved_query(
            pid,
            mine.id,
            &who("alice"),
            false,
            &SavedQueryPatch::default(),
            &limits,
        )
        .await;
    assert!(
        e.is_ok(),
        "the owner changes a personal query without cases:write"
    );
    assert_eq!(
        numbers(&store, pid, "filter = Mine", "bob")
            .await
            .unwrap_err()
            .to_string(),
        "invalid_query: unknown saved query \"Mine\""
    );
    let patch = SavedQueryPatch {
        query: Some("tag in [smoke, api]".into()),
        ..Default::default()
    };
    let e = store
        .update_saved_query(pid, team.id, &who("bob"), false, &patch, &limits)
        .await
        .unwrap_err();
    assert!(matches!(e, TmError::Forbidden { .. }), "{e}");
    let e = store
        .delete_saved_query(pid, team.id, &who("bob"), false)
        .await
        .unwrap_err();
    assert!(matches!(e, TmError::Forbidden { .. }), "{e}");
    let (before, after) = store
        .update_saved_query(pid, team.id, &who("bob"), true, &patch, &limits)
        .await
        .unwrap();
    assert_eq!(
        (before.query.as_str(), after.query.as_str()),
        ("tag = smoke", "tag in [smoke, api]")
    );
    assert!(!after.mine && after.owner == "alice");
    let e = store
        .update_saved_query(
            pid,
            team.id,
            &who("bob"),
            true,
            &SavedQueryPatch {
                shared: Some(false),
                ..Default::default()
            },
            &limits,
        )
        .await
        .unwrap_err();
    assert!(
        matches!(e, TmError::Forbidden { .. }),
        "only the owner unshares: {e}"
    );
    assert_eq!(
        numbers(&store, pid, "filter = Team", "bob").await.unwrap(),
        vec![1, 2]
    );

    // Saved queries inside saved queries, and cycles.
    let a = store
        .create_saved_query(
            pid,
            &who("carol"),
            false,
            &w("A", "filter = Team and automated = true", false),
            &limits,
        )
        .await
        .unwrap();
    assert_eq!(
        numbers(&store, pid, "filter = A", "carol").await.unwrap(),
        vec![2]
    );
    let b = store
        .create_saved_query(
            pid,
            &who("carol"),
            false,
            &w("B", "filter = A or number = 5", false),
            &limits,
        )
        .await
        .unwrap();
    assert_eq!(
        numbers(&store, pid, "filter = B", "carol").await.unwrap(),
        vec![2, 5]
    );
    let e = store
        .update_saved_query(
            pid,
            a.id,
            &who("carol"),
            false,
            &SavedQueryPatch {
                query: Some("filter = B".into()),
                ..Default::default()
            },
            &limits,
        )
        .await
        .unwrap_err();
    let qe = query_error(e);
    assert!(qe.message.contains("refer to each other"), "{qe:?}");
    assert_eq!((qe.position, qe.length), (9, 1));
    sqlx::query("update saved_queries set query = 'filter = B' where id = $1")
        .bind(a.id)
        .execute(&db.pool)
        .await
        .unwrap();
    let qe = query_error(
        numbers(&store, pid, "filter = B", "carol")
            .await
            .unwrap_err(),
    );
    assert!(qe.message.contains("refer to each other"), "{qe:?}");
    let _ = b;
    let wide = (1..=700)
        .map(|n| n.to_string())
        .collect::<Vec<_>>()
        .join(", ");
    store
        .create_saved_query(
            pid,
            &who("carol"),
            false,
            &w("Wide", &format!("number in [{wide}]"), false),
            &TmLimits::default(),
        )
        .await
        .unwrap();
    assert_eq!(
        numbers(&store, pid, "filter = Wide or filter = Wide", "carol")
            .await
            .unwrap(),
        vec![1, 2, 3, 5]
    );
    let qe = query_error(
        numbers(
            &store,
            pid,
            "filter = Wide or filter = Wide or filter = Wide",
            "carol",
        )
        .await
        .unwrap_err(),
    );
    assert!(qe.message.contains("2000 values"), "{qe:?}");

    // Limits: per owner and entity, and shared ones per project.
    store
        .create_saved_query(
            pid,
            &who("alice"),
            false,
            &w("Third", "number = 1", false),
            &limits,
        )
        .await
        .unwrap();
    let e = store
        .create_saved_query(
            pid,
            &who("alice"),
            false,
            &w("Fourth", "number = 1", false),
            &limits,
        )
        .await
        .unwrap_err();
    assert!(
        matches!(
            e,
            TmError::Conflict {
                code: "limit_reached",
                ..
            }
        ),
        "{e}"
    );
    let one = TmLimits {
        max_saved_queries: 1,
        ..Default::default()
    };
    store
        .create_saved_query(pid, &who("dave"), true, &w("S2", "number = 1", true), &one)
        .await
        .unwrap();
    let e = store
        .create_saved_query(pid, &who("erin"), true, &w("S3", "number = 1", true), &one)
        .await
        .unwrap_err();
    assert!(
        matches!(
            e,
            TmError::Conflict {
                code: "limit_reached",
                ..
            }
        ),
        "{e}"
    );

    // Another project's query is not found here.
    let other = project(&store, "sq2").await;
    let e = store
        .delete_saved_query(other.id, team.id, &who("alice"), true)
        .await
        .unwrap_err();
    assert!(matches!(e, TmError::NotFound(_)), "{e}");
    store
        .delete_saved_query(pid, team.id, &who("bob"), true)
        .await
        .unwrap();
}

#[tokio::test]
async fn bulk_export_and_plans_by_query() {
    let Some((db, store)) = TestDb::migrated().await else {
        return;
    };
    let s = seed(&store, &db.pool, "bx").await;
    let pid = s.p.id;
    let limits = TmLimits::default();
    let ctx = CaseCtx {
        project_id: pid,
        project_key: "bx",
        by: "alice",
        limits: &limits,
        can_review: false,
    };
    let bulk = |query: &str, action: CaseBulkAction, value: Option<serde_json::Value>| CaseBulk {
        numbers: None,
        filter: None,
        query: Some(query.into()),
        action,
        value,
    };
    let r = store
        .bulk_cases(
            &ctx,
            &bulk(
                "createdBy = me() or number = 5",
                CaseBulkAction::AddTags,
                Some(serde_json::json!(["q"])),
            ),
        )
        .await
        .unwrap();
    assert_eq!(r.updated, 2, "{r:?}");
    assert_eq!(
        numbers(&store, pid, "tag = q", "alice").await.unwrap(),
        vec![1, 5]
    );
    let r = store
        .bulk_cases(
            &ctx,
            &bulk("archived = true", CaseBulkAction::Restore, None),
        )
        .await
        .unwrap();
    assert_eq!(r.updated, 1, "{r:?}");
    let mut both = bulk("tag = q", CaseBulkAction::Archive, None);
    both.numbers = Some(vec![1]);
    assert!(matches!(
        store.bulk_cases(&ctx, &both).await.unwrap_err(),
        TmError::Invalid { .. }
    ));
    let e = store
        .bulk_cases(&ctx, &bulk("tag ~ q", CaseBulkAction::Archive, None))
        .await
        .unwrap_err();
    assert!(
        matches!(
            e,
            TmError::Invalid {
                code: "invalid_query",
                ..
            }
        ),
        "{e}"
    );

    let src = store
        .export_source_query(
            pid,
            &CaseFilter::default(),
            Some("automated = true"),
            &who("alice"),
            100,
        )
        .await
        .unwrap();
    assert_eq!(src.numbers, vec![2, 3]);
    let e = store
        .export_source_query(
            pid,
            &CaseFilter::default(),
            Some("number > 0"),
            &who("alice"),
            2,
        )
        .await
        .unwrap_err();
    assert!(
        matches!(
            e,
            TmError::Conflict {
                code: "limit_reached",
                ..
            }
        ),
        "{e}"
    );

    // Plans keep a query (exclusive with a filter); sync shows the drift.
    let plan = store
        .create_managed_plan(
            pid,
            &TestPlanWrite {
                name: "By query".into(),
                query: Some(Some("tag = smoke".into())),
                ..Default::default()
            },
            "alice",
            &limits,
        )
        .await
        .unwrap();
    assert_eq!(plan.query.as_deref(), Some("tag = smoke"));
    assert_eq!(
        (plan.counts.total, plan.filter.is_none()),
        (2, true),
        "TC-1 and the restored TC-4"
    );
    exec(
        &db.pool,
        "update cases set tags = '{smoke}' where project_id = $1 and number = 2",
        pid,
    )
    .await;
    exec(
        &db.pool,
        "update cases set tags = '{}' where project_id = $1 and number = 4",
        pid,
    )
    .await;
    let d = store
        .sync_plan(pid, plan.id, true, "alice", &limits)
        .await
        .unwrap();
    assert_eq!((d.added, d.removed), (vec![2], vec![4]));
    let e = store
        .create_managed_plan(
            pid,
            &TestPlanWrite {
                name: "Both".into(),
                query: Some(Some("tag = smoke".into())),
                filter: Some(Some(CaseFilter::default())),
                ..Default::default()
            },
            "alice",
            &limits,
        )
        .await
        .unwrap_err();
    assert!(matches!(e, TmError::Invalid { .. }), "{e}");
    let e = store
        .create_managed_plan(
            pid,
            &TestPlanWrite {
                name: "Bad".into(),
                query: Some(Some("tag ~ x".into())),
                ..Default::default()
            },
            "alice",
            &limits,
        )
        .await
        .unwrap_err();
    assert!(
        matches!(
            e,
            TmError::Invalid {
                code: "invalid_query",
                ..
            }
        ),
        "{e}"
    );
    let switched = store
        .update_managed_plan(
            pid,
            plan.id,
            plan.revision,
            &tm::plans::TestPlanPatch {
                filter: Some(Some(CaseFilter {
                    tags: Some(vec!["api".into()]),
                    ..Default::default()
                })),
                ..Default::default()
            },
            "alice",
            &limits,
        )
        .await
        .unwrap();
    assert!(switched.query.is_none() && switched.filter.is_some());
    let back = store
        .update_managed_plan(
            pid,
            plan.id,
            switched.revision,
            &tm::plans::TestPlanPatch {
                query: Some(Some("automated = true".into())),
                ..Default::default()
            },
            "alice",
            &limits,
        )
        .await
        .unwrap();
    assert_eq!(
        (back.query.as_deref(), back.filter.is_none()),
        (Some("automated = true"), true)
    );
    let d = store
        .sync_plan(pid, plan.id, true, "alice", &limits)
        .await
        .unwrap();
    assert_eq!((d.added, d.removed), (vec![2, 3], vec![1, 4]));
}

/// §14: a list of 50 with a 4–6 condition query and a validation with count at 20 000 cases.
/// `cargo test --release -p fastproof-store --features test-util --test tm2_query -- --ignored`
#[tokio::test]
#[ignore = "performance; run with --release"]
async fn perf_list_and_validate_at_20k_cases() {
    let Some((db, store)) = TestDb::migrated().await else {
        return;
    };
    let s = seed(&store, &db.pool, "perf").await;
    let pid = s.p.id;
    exec(
        &db.pool,
        "insert into cases (id, project_id, number, name, status, automated, origin, folder_id, body, fields, tags, \
           search_text, last_status, last_status_at, last_source, created_by) \
         select gen_random_uuid(), $1, 100 + g, 'Case ' || g || ' checkout step', \
           (array['draft','active','active','outdated'])[1 + g % 4], g % 3 = 0, 'ui', \
           (select id from case_folders where project_id = $1 order by id offset (g % 3) limit 1), '{\"steps\": []}'::jsonb, \
           jsonb_build_object('prio', (array['high','low','critical'])[1 + g % 3], 'points', g % 20), \
           case when g % 5 = 0 then array['smoke'] else array['api', 'nightly'] end, \
           'Case ' || g || ' checkout payment card refund login', \
           (array['passed','failed','broken','skipped'])[1 + g % 4], now() - make_interval(hours => g % 500), \
           'automated', (array['alice','bob','carol'])[1 + g % 3] \
         from generate_series(1, 20000) g",
        pid,
    )
    .await;
    sqlx::query("analyze cases")
        .execute(&db.pool)
        .await
        .unwrap();
    let queries = [
        "status = active and automated = false and field[prio] in [high, critical] and tag != legacy",
        "folder in subtree(\"Payments\") and lastStatus in [failed, broken] and lastRunAt > now() - 7d and createdBy = me()",
        "(status = active or status = draft) and tag = smoke and field[points] >= 10 and name ~ \"checkout\" and not lastStatus = passed",
        "text ~ refund and automated = true and lastSource = automated and origin = ui and number > 5000",
    ];
    let mut report = Vec::new();
    for q in queries {
        let mut list = Vec::new();
        let mut val = Vec::new();
        for _ in 0..25 {
            let t = Instant::now();
            let l = store
                .list_cases(
                    pid,
                    &CaseQuery {
                        query: Some(q.into()),
                        me: "alice".into(),
                        limit: 50,
                        ..Default::default()
                    },
                )
                .await
                .unwrap();
            list.push(t.elapsed());
            assert!(!l.items.is_empty(), "{q}");
            let t = Instant::now();
            let v = store
                .validate_query(pid, QueryEntity::Cases, q, &who("alice"))
                .await
                .unwrap();
            val.push(t.elapsed());
            assert!(v.ok, "{q}");
        }
        list.sort();
        val.sort();
        let p95 = |v: &[Duration]| v[(v.len() * 95 / 100).min(v.len() - 1)];
        report.push(format!(
            "{q}\n  list p50 {:?} p95 {:?}; validate p50 {:?} p95 {:?}",
            list[list.len() / 2],
            p95(&list),
            val[val.len() / 2],
            p95(&val)
        ));
    }
    println!("{}", report.join("\n"));
}

/// Personal saved queries belong to the owner key, not the name (a token named like a user is
/// not that user); a caller without an owner key sees shared ones only; exports leave archived
/// cases out even when the query names `archived`.
#[tokio::test]
async fn saved_queries_follow_the_owner_key_and_exports_skip_the_trash() {
    let Some((db, store)) = TestDb::migrated().await else {
        return;
    };
    let s = seed(&store, &db.pool, "fk").await;
    let pid = s.p.id;
    let limits = TmLimits::default();
    let user = who("alice");
    let token = QueryCtx::caller("alice", "token:7f1d3c1e-0000-4000-8000-000000000001");
    let w = SavedQueryWrite {
        entity: QueryEntity::Cases,
        name: "Mine".into(),
        query: "number = 1".into(),
        shared: None,
        position: None,
    };
    let mine = store
        .create_saved_query(pid, &user, false, &w, &limits)
        .await
        .unwrap();
    assert!(mine.mine && mine.owner == "alice");

    let l = store
        .list_saved_queries(pid, QueryEntity::Cases, &token)
        .await
        .unwrap();
    assert!(l.items.is_empty(), "{l:?}");
    let patch = SavedQueryPatch {
        query: Some("number = 2".into()),
        ..Default::default()
    };
    assert!(matches!(
        store
            .update_saved_query(pid, mine.id, &token, true, &patch, &limits)
            .await
            .unwrap_err(),
        TmError::NotFound(_)
    ));
    assert!(matches!(
        store
            .delete_saved_query(pid, mine.id, &token, true)
            .await
            .unwrap_err(),
        TmError::NotFound(_)
    ));
    let list = |ctx: &QueryCtx, q: &str| CaseQuery {
        query: Some(q.to_string()),
        me: ctx.me.clone(),
        owner: ctx.owner.clone(),
        limit: 100,
        ..Default::default()
    };
    query_error(
        store
            .list_cases(pid, &list(&token, "filter = Mine"))
            .await
            .unwrap_err(),
    );
    let v = store
        .validate_query(pid, QueryEntity::Cases, "filter = Mine", &token)
        .await
        .unwrap();
    assert!(!v.ok);
    let nobody = QueryCtx::new("alice");
    query_error(
        store
            .list_cases(pid, &list(&nobody, "filter = Mine"))
            .await
            .unwrap_err(),
    );
    assert!(matches!(
        store
            .create_saved_query(pid, &nobody, false, &w, &limits)
            .await
            .unwrap_err(),
        TmError::Invalid { .. }
    ));
    let theirs = store
        .create_saved_query(pid, &token, false, &w, &limits)
        .await
        .expect("the same name is free for another owner");
    assert!(theirs.mine && theirs.owner == "alice");
    let got = store
        .list_cases(pid, &list(&user, "filter = Mine"))
        .await
        .unwrap();
    assert_eq!(
        got.items.iter().map(|c| c.number).collect::<Vec<_>>(),
        vec![1]
    );

    let ctx = CaseCtx {
        project_id: pid,
        project_key: "fk",
        by: "alice",
        limits: &limits,
        can_review: false,
    };
    let by_query = |q: &str, action: CaseBulkAction| CaseBulk {
        numbers: None,
        filter: None,
        query: Some(q.into()),
        action,
        value: None,
    };
    assert!(matches!(
        store
            .bulk_cases(&ctx, &by_query("filter = Mine", CaseBulkAction::Archive))
            .await
            .unwrap_err(),
        TmError::Invalid {
            code: "invalid_query",
            ..
        }
    ));
    let r = store
        .bulk_cases_as(
            &ctx,
            &user.owner,
            &by_query("filter = Mine", CaseBulkAction::Archive),
        )
        .await
        .unwrap();
    assert_eq!(r.updated, 1, "{r:?}");

    let src = store
        .export_source_query(
            pid,
            &CaseFilter::default(),
            Some("archived = true"),
            &user,
            100,
        )
        .await
        .unwrap();
    assert!(src.numbers.is_empty(), "{:?}", src.numbers);
    let src = store
        .export_source_query(pid, &CaseFilter::default(), Some("number = 1"), &user, 100)
        .await
        .unwrap();
    assert!(src.numbers.is_empty(), "{:?}", src.numbers);
}
