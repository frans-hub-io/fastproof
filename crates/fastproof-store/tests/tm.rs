//! Test-management foundation on a real PostgreSQL: the upgrade from stage 1, folders, field
//! definitions, keys, manual identities, launch counters, partitions, retention and
//! attachment GC with manual runs. They need `FASTPROOF_TEST_DATABASE_URL`; without it they
//! pass without doing anything.

use std::time::Duration;

use fastproof_store::projects::CreateProject;
use fastproof_store::tm::{
    self, CaseBody, CaseFieldPatch, CaseFieldWrite, CaseFolderPatch, CaseFolderWrite, CaseKeyKind,
    CaseStep, FieldOption, FieldType, FieldValue, FolderDeleteMode, TmError, TmLimits,
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
    async fn new() -> Option<Self> {
        let admin = testdb::admin()?;
        let name = testdb::create(&admin).await;
        let pool = PgPoolOptions::new()
            .max_connections(6)
            .connect_with(admin.clone().database(&name))
            .await
            .unwrap();
        Some(Self { admin, name, pool })
    }

    async fn migrated() -> Option<(Self, Store)> {
        let db = Self::new().await?;
        fastproof_store::migrate(&db.pool).await.unwrap();
        let store = Store::new(db.pool.clone());
        Some((db, store))
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

fn body(action: &str) -> CaseBody {
    CaseBody {
        description: "d".into(),
        preconditions: String::new(),
        steps: vec![CaseStep {
            id: "s_1".into(),
            action: action.into(),
            expected: "ok".into(),
            ..Default::default()
        }],
        links: vec![],
    }
}

async fn case_in(store: &Store, p: &Project, name: &str, folder: Option<Uuid>) -> tm::CaseRow {
    let mut conn = store.pool().acquire().await.unwrap();
    tm::testutil::insert_case(&mut conn, p.id, name, &body(name), folder)
        .await
        .unwrap()
}

/// A launch inserted directly, `days_ago` days old, with an optional manual state.
async fn launch_at(
    db: &TestDb,
    p: &Project,
    days_ago: i32,
    manual: Option<&str>,
    rolled_up: bool,
) -> (Uuid, chrono::DateTime<chrono::Utc>) {
    let id = Uuid::now_v7();
    let at: chrono::DateTime<chrono::Utc> = sqlx::query_scalar(
        "insert into launches (id, project_id, number, name, status, created_at, started_at, manual_state, rolled_up_at) \
         values ($1, $2, (select coalesce(max(number), 0) + 1 from launches where project_id = $2), 'l', 'finished', \
           now() - make_interval(days => $3), now() - make_interval(days => $3), $4, \
           case when $5 then now() end) returning created_at",
    )
    .bind(id)
    .bind(p.id)
    .bind(days_ago)
    .bind(manual)
    .bind(rolled_up)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    (id, at)
}

async fn result_in(
    db: &TestDb,
    p: &Project,
    launch: (Uuid, chrono::DateTime<chrono::Utc>),
    status: &str,
) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        "insert into results (id, launch_created_at, project_id, launch_id, test_case_id, series_id, name, full_name, status) \
         values ($1, $2, $3, $4, $5, $6, 'r', 'r', $7)",
    )
    .bind(id)
    .bind(launch.1)
    .bind(p.id)
    .bind(launch.0)
    .bind(Uuid::now_v7())
    .bind(Uuid::now_v7())
    .bind(status)
    .execute(&db.pool)
    .await
    .unwrap();
    id
}

async fn run_item(
    db: &TestDb,
    p: &Project,
    launch: (Uuid, chrono::DateTime<chrono::Utc>),
    case: &tm::CaseRow,
    status: &str,
    result_id: Option<Uuid>,
) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        "insert into run_items (id, project_id, launch_id, launch_created_at, case_id, case_version, series_id, status, result_id) \
         values ($1, $2, $3, $4, $5, 1, $6, $7, $8)",
    )
    .bind(id)
    .bind(p.id)
    .bind(launch.0)
    .bind(launch.1)
    .bind(case.id)
    .bind(Uuid::now_v7())
    .bind(status)
    .bind(result_id)
    .execute(&db.pool)
    .await
    .unwrap();
    id
}

fn code(e: &TmError) -> &str {
    match e {
        TmError::NotFound(_) => "not_found",
        TmError::Invalid { code, .. }
        | TmError::Conflict { code, .. }
        | TmError::Forbidden { code, .. } => code,
        TmError::Stale { .. } => "precondition_failed",
        TmError::Other(_) => "other",
    }
}

#[tokio::test]
async fn upgrade_from_stage_one_keeps_data_and_queries() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    fastproof_store::migrator()
        .run_to(402, &db.pool)
        .await
        .unwrap();
    let pid = Uuid::now_v7();
    sqlx::query("insert into projects (id, key, name) values ($1, 'old', 'Old')")
        .bind(pid)
        .execute(&db.pool)
        .await
        .unwrap();
    let lid = Uuid::now_v7();
    let created: chrono::DateTime<chrono::Utc> = sqlx::query_scalar(
        "insert into launches (id, project_id, number, name, branch, count_total, count_passed) \
         values ($1, $2, 1, 'nightly', 'main', 1, 1) returning created_at",
    )
    .bind(lid)
    .bind(pid)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    let tc = Uuid::now_v7();
    sqlx::query(
        "insert into test_cases (id, project_id, test_case_hash, full_name, display_name) \
         values ($1, $2, 'h1', 'shop.Checkout#pays', 'pays')",
    )
    .bind(tc)
    .bind(pid)
    .execute(&db.pool)
    .await
    .unwrap();
    let se = Uuid::now_v7();
    sqlx::query(
        "insert into series (id, project_id, test_case_id, series_key, parameters_hash) values ($1, $2, $3, 'h1.p', 'p')",
    )
    .bind(se)
    .bind(pid)
    .bind(tc)
    .execute(&db.pool)
    .await
    .unwrap();
    let rid = Uuid::now_v7();
    sqlx::query(
        "insert into results (id, launch_created_at, project_id, launch_id, test_case_id, series_id, name, full_name, status) \
         values ($1, $2, $3, $4, $5, $6, 'pays', 'shop.Checkout#pays', 'passed')",
    )
    .bind(rid)
    .bind(created)
    .bind(pid)
    .bind(lid)
    .bind(tc)
    .bind(se)
    .execute(&db.pool)
    .await
    .unwrap();
    sqlx::query("insert into test_plans (id, project_id, name) values ($1, $2, 'failed of 1')")
        .bind(Uuid::now_v7())
        .bind(pid)
        .execute(&db.pool)
        .await
        .unwrap();

    let mut applied = fastproof_store::migrate(&db.pool).await.unwrap();
    applied.sort();
    assert_eq!(
        applied,
        vec![
            500, 501, 502, 503, 504, 505, 506, 507, 600, 700, 710, 711, 712, 720, 721, 722, 730,
            731, 740, 741, 750, 751, 760, 770, 780, 781, 782, 790, 791, 800, 801, 810, 811, 815,
            816, 817, 818, 819, 820, 830
        ]
    );
    fastproof_store::check_schema(&db.pool).await.unwrap();
    let (kind, case_id): (String, Option<Uuid>) =
        sqlx::query_as("select kind, case_id from test_cases where id = $1")
            .bind(tc)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!((kind.as_str(), case_id), ("code", None));
    let (sync, manual, untested, plan_kind): (bool, Option<String>, i32, String) = sqlx::query_as(
        "select l.created_by_can_sync, l.manual_state, l.count_untested, \
         (select kind from test_plans where project_id = l.project_id) from launches l where l.id = $1",
    )
    .bind(lid)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(
        (sync, manual, untested, plan_kind.as_str()),
        (false, None, 0, "adhoc")
    );
    let invalid: Vec<String> = sqlx::query_scalar(
        "select conname::text from pg_constraint where conname in \
         ('test_cases_case_fk', 'test_cases_kind_check', 'launches_manual_state_check') and not convalidated",
    )
    .fetch_all(&db.pool)
    .await
    .unwrap();
    assert!(invalid.is_empty(), "{invalid:?}");
    let indexes: Vec<(String, bool)> = sqlx::query_as(
        "select c.relname::text, i.indisvalid from pg_index i join pg_class c on c.oid = i.indexrelid \
         where c.relname in ('test_cases_case', 'test_cases_unlinked') order by 1",
    )
    .fetch_all(&db.pool)
    .await
    .unwrap();
    assert_eq!(
        indexes,
        vec![
            ("test_cases_case".to_string(), true),
            ("test_cases_unlinked".to_string(), true)
        ]
    );
    let store = Store::new(db.pool.clone());
    let l = store.launch(pid, lid).await.unwrap().unwrap();
    assert_eq!(l.counts.total, 1);
    assert!(l.manual.is_none() && l.test_plan_id.is_none());
    let json = serde_json::to_value(&l).unwrap();
    assert!(json.get("manual").is_none() && json.get("testPlanId").is_none());
    let mut conn = db.pool.acquire().await.unwrap();
    Store::recount_launch(&mut conn, lid).await.unwrap();
    let l = store.launch(pid, lid).await.unwrap().unwrap();
    assert_eq!((l.counts.total, l.counts.passed), (1, 1));
    let s = store.project_settings(pid).await.unwrap();
    assert!(s.case_sync.enabled);
    assert_eq!(s.retention.manual_days, 0);
}

#[tokio::test]
async fn folders_tree_move_and_delete() {
    let Some((_db, store)) = TestDb::migrated().await else {
        return;
    };
    let p = project(&store, "fold").await;
    let other = project(&store, "other").await;
    let lim = TmLimits::default();
    let w = |name: &str, parent: Option<Uuid>| CaseFolderWrite {
        name: name.into(),
        parent_id: parent,
        position: None,
    };
    let a = store
        .create_case_folder(p.id, &w("A", None), &lim)
        .await
        .unwrap();
    let b = store
        .create_case_folder(p.id, &w("B", None), &lim)
        .await
        .unwrap();
    let a1 = store
        .create_case_folder(p.id, &w("A1", Some(a.id)), &lim)
        .await
        .unwrap();
    let a11 = store
        .create_case_folder(p.id, &w("A11", Some(a1.id)), &lim)
        .await
        .unwrap();
    assert_eq!((a.depth, a1.depth, a11.depth), (1, 2, 3));
    assert_eq!((a.position, b.position), (0, 1));
    let e = store
        .create_case_folder(p.id, &w("a", None), &lim)
        .await
        .unwrap_err();
    assert_eq!(code(&e), "folder_name_taken");
    let e = store
        .create_case_folder(other.id, &w("X", Some(a.id)), &lim)
        .await
        .unwrap_err();
    assert_eq!(code(&e), "not_found");
    assert_eq!(
        code(
            &store
                .create_case_folder(p.id, &w(" ", None), &lim)
                .await
                .unwrap_err()
        ),
        "bad_request"
    );

    case_in(&store, &p, "c1", Some(a.id)).await;
    let c2 = case_in(&store, &p, "c2", Some(a11.id)).await;
    sqlx::query("update cases set automated = true where id = $1")
        .bind(c2.id)
        .execute(store.pool())
        .await
        .unwrap();
    case_in(&store, &p, "c3", Some(b.id)).await;
    let top = store.case_folder_level(p.id, None).await.unwrap();
    assert_eq!(
        top.iter()
            .map(|f| (
                f.name.as_str(),
                f.counts.total,
                f.counts.automated,
                f.counts.manual,
                f.has_children
            ))
            .collect::<Vec<_>>(),
        vec![("A", 2, 1, 1, true), ("B", 1, 0, 1, false)]
    );
    let lvl = store.case_folder_level(p.id, Some(a.id)).await.unwrap();
    assert_eq!(lvl.len(), 1);
    assert_eq!((lvl[0].name.as_str(), lvl[0].counts.total), ("A1", 1));
    assert_eq!(
        code(
            &store
                .case_folder_level(other.id, Some(a.id))
                .await
                .unwrap_err()
        ),
        "not_found"
    );
    assert!(
        store
            .case_folder_level(other.id, None)
            .await
            .unwrap()
            .is_empty()
    );

    let mv = |parent: Option<Option<Uuid>>, name: Option<&str>| CaseFolderPatch {
        name: name.map(str::to_string),
        parent_id: parent,
        position: None,
    };
    let e = store
        .update_case_folder(p.id, a.id, &mv(Some(Some(a11.id)), None), &lim)
        .await
        .unwrap_err();
    assert_eq!(code(&e), "bad_request");
    let e = store
        .update_case_folder(p.id, a.id, &mv(Some(Some(a.id)), None), &lim)
        .await
        .unwrap_err();
    assert_eq!(code(&e), "bad_request");
    let moved = store
        .update_case_folder(p.id, a1.id, &mv(Some(Some(b.id)), Some("Moved")), &lim)
        .await
        .unwrap();
    assert_eq!(
        (moved.parent_id, moved.depth, moved.name.as_str()),
        (Some(b.id), 2, "Moved")
    );
    let path = store.case_folder_path(p.id, a11.id).await.unwrap();
    assert_eq!(
        path.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(),
        vec!["B", "Moved", "A11"]
    );
    let deep: (Vec<Uuid>, i16) =
        sqlx::query_as("select path_ids, depth from case_folders where id = $1")
            .bind(a11.id)
            .fetch_one(store.pool())
            .await
            .unwrap();
    assert_eq!(deep, (vec![b.id, a1.id, a11.id], 3));
    let top = store.case_folder_level(p.id, None).await.unwrap();
    assert_eq!(
        top.iter().map(|f| f.counts.total).collect::<Vec<_>>(),
        vec![1, 2]
    );
    let to_root = store
        .update_case_folder(p.id, a1.id, &mv(Some(None), None), &lim)
        .await
        .unwrap();
    assert_eq!((to_root.parent_id, to_root.depth), (None, 1));
    let deep: (Vec<Uuid>, i16) =
        sqlx::query_as("select path_ids, depth from case_folders where id = $1")
            .bind(a11.id)
            .fetch_one(store.pool())
            .await
            .unwrap();
    assert_eq!(deep, (vec![a1.id, a11.id], 2));
    let e = store
        .update_case_folder(p.id, a1.id, &mv(None, Some("b")), &lim)
        .await
        .unwrap_err();
    assert_eq!(code(&e), "folder_name_taken");
    assert_eq!(
        code(
            &store
                .update_case_folder(other.id, a1.id, &mv(None, Some("z")), &lim)
                .await
                .unwrap_err()
        ),
        "not_found"
    );

    let shallow = TmLimits {
        max_folder_depth: 2,
        ..lim
    };
    let e = store
        .update_case_folder(p.id, a1.id, &mv(Some(Some(b.id)), None), &shallow)
        .await
        .unwrap_err();
    assert_eq!(code(&e), "limit_reached");
    let e = store
        .create_case_folder(p.id, &w("deep", Some(a11.id)), &shallow)
        .await
        .unwrap_err();
    assert_eq!(code(&e), "limit_reached");
    let few = TmLimits {
        max_folders: 4,
        ..lim
    };
    let e = store
        .create_case_folder(p.id, &w("fifth", None), &few)
        .await
        .unwrap_err();
    match e {
        TmError::Conflict { code, details, .. } => {
            assert_eq!(code, "limit_reached");
            assert_eq!(
                details.unwrap(),
                serde_json::json!({"limit": "maxFolders", "max": 4})
            );
        }
        other => panic!("{other:?}"),
    }

    let e = store
        .delete_case_folder(p.id, a1.id, None, "alice")
        .await
        .unwrap_err();
    assert_eq!(code(&e), "folder_not_empty");
    assert_eq!(
        code(
            &store
                .delete_case_folder(other.id, a1.id, None, "x")
                .await
                .unwrap_err()
        ),
        "not_found"
    );
    let d = store
        .delete_case_folder(p.id, a1.id, Some(FolderDeleteMode::MoveUp), "alice")
        .await
        .unwrap();
    assert_eq!((d.folders, d.cases_moved, d.cases_archived), (1, 0, 0));
    let a11v = store.case_folder(p.id, a11.id).await.unwrap().unwrap();
    assert_eq!((a11v.parent_id, a11v.depth), (None, 1));
    let rev: (i64, Option<String>) =
        sqlx::query_as("select revision, updated_by from cases where id = $1")
            .bind(c2.id)
            .fetch_one(store.pool())
            .await
            .unwrap();
    assert_eq!(rev.0, 1);
    let sub = store
        .create_case_folder(p.id, &w("Sub", Some(a11.id)), &lim)
        .await
        .unwrap();
    let c4 = case_in(&store, &p, "c4", Some(sub.id)).await;
    let d = store
        .delete_case_folder(p.id, a11.id, Some(FolderDeleteMode::ArchiveCases), "bob")
        .await
        .unwrap();
    assert_eq!((d.folders, d.cases_archived), (2, 2));
    let (archived, folder, revision, by): (bool, Option<Uuid>, i64, Option<String>) = sqlx::query_as(
        "select archived_at is not null, folder_id, revision, updated_by from cases where id = $1",
    )
    .bind(c4.id)
    .fetch_one(store.pool())
    .await
    .unwrap();
    assert_eq!(
        (archived, folder, revision, by.as_deref()),
        (true, None, 2, Some("bob"))
    );
    assert!(store.case_folder(p.id, sub.id).await.unwrap().is_none());
    let d = store
        .delete_case_folder(p.id, a.id, Some(FolderDeleteMode::MoveUp), "carol")
        .await
        .unwrap();
    assert_eq!(d.cases_moved, 1);
    let empty = store
        .create_case_folder(p.id, &w("Empty", None), &lim)
        .await
        .unwrap();
    assert_eq!(
        store
            .delete_case_folder(p.id, empty.id, None, "x")
            .await
            .unwrap()
            .folders,
        1
    );

    let mut conn = store.pool().acquire().await.unwrap();
    let mut tx = sqlx::Acquire::begin(&mut *conn).await.unwrap();
    let f = tm::folders::ensure_folder_path(
        &mut tx,
        p.id,
        None,
        &["Automated", "shop", "checkout"],
        &lim,
    )
    .await
    .unwrap()
    .unwrap();
    let again = tm::folders::ensure_folder_path(
        &mut tx,
        p.id,
        None,
        &["automated", "SHOP", "checkout"],
        &lim,
    )
    .await
    .unwrap();
    assert_eq!(again, Some(f));
    tx.commit().await.unwrap();
    let path = store.case_folder_path(p.id, f).await.unwrap();
    assert_eq!(path.len(), 3);
}

#[tokio::test]
async fn concurrent_moves_cannot_make_a_cycle() {
    let Some((_db, store)) = TestDb::migrated().await else {
        return;
    };
    let p = project(&store, "cyc").await;
    let lim = TmLimits::default();
    let mk = |name: &str| CaseFolderWrite {
        name: name.into(),
        parent_id: None,
        position: None,
    };
    let x = store
        .create_case_folder(p.id, &mk("X"), &lim)
        .await
        .unwrap();
    let y = store
        .create_case_folder(p.id, &mk("Y"), &lim)
        .await
        .unwrap();
    let into = |parent: Uuid| CaseFolderPatch {
        name: None,
        parent_id: Some(Some(parent)),
        position: None,
    };
    let (px, py) = (into(y.id), into(x.id));
    let (rx, ry) = tokio::join!(
        store.update_case_folder(p.id, x.id, &px, &lim),
        store.update_case_folder(p.id, y.id, &py, &lim)
    );
    assert!(rx.is_ok() != ry.is_ok(), "{rx:?} {ry:?}");
    let roots: i64 = sqlx::query_scalar(
        "select count(*) from case_folders where project_id = $1 and parent_id is null",
    )
    .bind(p.id)
    .fetch_one(store.pool())
    .await
    .unwrap();
    assert_eq!(roots, 1);
}

#[tokio::test]
async fn field_definitions() {
    let Some((_db, store)) = TestDb::migrated().await else {
        return;
    };
    let p = project(&store, "flds").await;
    let other = project(&store, "flds2").await;
    let lim = TmLimits::default();
    let defs = store.case_fields(p.id, false).await.unwrap();
    assert_eq!(
        defs.iter().map(|d| d.key.as_str()).collect::<Vec<_>>(),
        vec![
            "priority",
            "severity",
            "layer",
            "owner",
            "epic",
            "feature",
            "story",
            "suite",
            "component"
        ]
    );
    assert!(defs.iter().all(|d| d.built_in));
    let sev = defs.iter().find(|d| d.key == "severity").unwrap();
    assert_eq!(
        (sev.field_type, sev.source_label.as_deref()),
        (FieldType::Select, Some("severity"))
    );
    assert_eq!(sev.options.len(), 5);
    assert_eq!(store.case_fields(p.id, false).await.unwrap().len(), 9);
    let opt = |v: &str| FieldOption {
        value: v.into(),
        color: Some("#00aa00".into()),
        archived: false,
    };
    let w = CaseFieldWrite {
        key: "team".into(),
        name: "Team".into(),
        field_type: FieldType::Select,
        options: Some(vec![opt("core"), opt("web")]),
        required: Some(true),
        default_value: Some(FieldValue::Text("core".into())),
        source_label: Some(Some("team".into())),
        position: Some(20),
    };
    let team = store.create_case_field(p.id, &w, &lim).await.unwrap();
    assert!(!team.built_in && team.required);
    assert_eq!(
        code(&store.create_case_field(p.id, &w, &lim).await.unwrap_err()),
        "conflict"
    );
    let mut clash = w.clone();
    clash.key = "team2".into();
    assert_eq!(
        code(
            &store
                .create_case_field(p.id, &clash, &lim)
                .await
                .unwrap_err()
        ),
        "conflict"
    );
    let mut bad = w.clone();
    bad.key = "Bad Key".into();
    assert_eq!(
        code(&store.create_case_field(p.id, &bad, &lim).await.unwrap_err()),
        "bad_request"
    );
    let mut bad = w.clone();
    bad.key = "t3".into();
    bad.source_label = None;
    bad.default_value = Some(FieldValue::Text("nope".into()));
    assert_eq!(
        code(&store.create_case_field(p.id, &bad, &lim).await.unwrap_err()),
        "bad_request"
    );
    let mut bad = w.clone();
    bad.key = "t4".into();
    bad.source_label = None;
    bad.field_type = FieldType::Number;
    bad.default_value = None;
    assert_eq!(
        code(&store.create_case_field(p.id, &bad, &lim).await.unwrap_err()),
        "bad_request"
    );
    let few = TmLimits {
        max_fields: 10,
        ..lim
    };
    let mut more = w.clone();
    more.key = "t5".into();
    more.source_label = None;
    assert_eq!(
        code(
            &store
                .create_case_field(p.id, &more, &few)
                .await
                .unwrap_err()
        ),
        "limit_reached"
    );

    let e = store
        .update_case_field(
            p.id,
            team.id,
            &CaseFieldPatch {
                field_type: Some(FieldType::Text),
                ..Default::default()
            },
            &lim,
        )
        .await
        .unwrap_err();
    assert_eq!(code(&e), "bad_request");
    let same_type = store
        .update_case_field(
            p.id,
            team.id,
            &CaseFieldPatch {
                field_type: Some(FieldType::Select),
                name: Some("Squad".into()),
                options: Some(vec![opt("core"), opt("web"), opt("api")]),
                ..Default::default()
            },
            &lim,
        )
        .await
        .unwrap();
    assert_eq!(
        (same_type.name.as_str(), same_type.options.len()),
        ("Squad", 3)
    );
    let e = store
        .update_case_field(
            p.id,
            sev.id,
            &CaseFieldPatch {
                key: Some("sev".into()),
                ..Default::default()
            },
            &lim,
        )
        .await
        .unwrap_err();
    assert_eq!(code(&e), "bad_request");
    let e = store
        .update_case_field(
            other.id,
            team.id,
            &CaseFieldPatch {
                name: Some("x".into()),
                ..Default::default()
            },
            &lim,
        )
        .await
        .unwrap_err();
    assert_eq!(code(&e), "not_found");
    assert_eq!(
        code(
            &store
                .delete_case_field(other.id, team.id)
                .await
                .unwrap_err()
        ),
        "not_found"
    );
    let gone = store.delete_case_field(p.id, team.id).await.unwrap();
    assert!(gone.archived);
    assert_eq!(store.case_fields(p.id, false).await.unwrap().len(), 9);
    assert_eq!(store.case_fields(p.id, true).await.unwrap().len(), 10);
    let back = store
        .update_case_field(
            p.id,
            team.id,
            &CaseFieldPatch {
                archived: Some(false),
                ..Default::default()
            },
            &lim,
        )
        .await
        .unwrap();
    assert!(!back.archived);
    assert_eq!(store.case_fields(other.id, false).await.unwrap().len(), 9);
}

#[tokio::test]
async fn keys_identity_lookup_and_counters() {
    let Some((db, store)) = TestDb::migrated().await else {
        return;
    };
    let p = project(&store, "keys").await;
    let other = project(&store, "keys2").await;
    let c1 = case_in(&store, &p, "first", None).await;
    let c2 = case_in(&store, &p, "second", None).await;
    assert_eq!((c1.number, c2.number), (1, 2));
    assert_eq!(c1.key(), "TC-1");
    let mut conn = db.pool.acquire().await.unwrap();
    assert!(
        tm::keys::set_case_key(&mut conn, p.id, c1.id, CaseKeyKind::AllureId, "1")
            .await
            .unwrap()
    );
    assert!(
        tm::keys::set_case_key(&mut conn, p.id, c1.id, CaseKeyKind::AllureId, "1")
            .await
            .unwrap()
    );
    assert!(
        !tm::keys::set_case_key(&mut conn, p.id, c2.id, CaseKeyKind::AllureId, "1")
            .await
            .unwrap()
    );
    assert!(
        tm::keys::set_case_key(&mut conn, p.id, c2.id, CaseKeyKind::Testrail, "C42")
            .await
            .unwrap()
    );
    assert!(matches!(
        tm::keys::set_case_key(&mut conn, other.id, c1.id, CaseKeyKind::Qase, "Q-1").await,
        Err(TmError::NotFound(_))
    ));
    assert_eq!(
        tm::keys::case_by_key(&mut conn, p.id, CaseKeyKind::AllureId, "1")
            .await
            .unwrap(),
        Some(c1.id)
    );
    assert_eq!(
        tm::keys::case_by_key(&mut conn, other.id, CaseKeyKind::AllureId, "1")
            .await
            .unwrap(),
        None
    );
    let keys = tm::keys::case_keys(&mut conn, c2.id).await.unwrap();
    assert_eq!(keys.len(), 1);
    assert_eq!(
        (keys[0].kind, keys[0].value.as_str()),
        (CaseKeyKind::Testrail, "C42")
    );
    assert!(
        tm::keys::remove_case_key(&mut conn, p.id, c2.id, CaseKeyKind::Testrail, "C42")
            .await
            .unwrap()
    );
    assert!(
        !tm::keys::remove_case_key(&mut conn, p.id, c2.id, CaseKeyKind::Testrail, "C42")
            .await
            .unwrap()
    );

    let (tc, series) = tm::identity::manual_identity(&mut conn, p.id, c1.id)
        .await
        .unwrap();
    let again = tm::identity::manual_identity(&mut conn, p.id, c1.id)
        .await
        .unwrap();
    assert_eq!(again, (tc, series));
    let (hash, kind, case_id, full, display): (String, String, Option<Uuid>, String, String) = sqlx::query_as(
        "select test_case_hash, kind, case_id, full_name, display_name from test_cases where id = $1",
    )
    .bind(tc)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    let md5: String = sqlx::query_scalar("select md5('fastproof:manual:' || $1::text)")
        .bind(c1.id)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(hash, md5);
    assert_eq!(
        (kind.as_str(), case_id, full.as_str(), display.as_str()),
        ("manual", Some(c1.id), "TC-1", "first")
    );
    let st: Uuid = sqlx::query_scalar("select test_case_id from series where id = $1")
        .bind(series)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(st, tc);
    assert!(
        tm::identity::manual_identity(&mut conn, other.id, c1.id)
            .await
            .is_err()
    );

    let row = store.case_by_number(p.id, 2).await.unwrap().unwrap();
    assert_eq!(
        (row.id, row.name.as_str(), row.version),
        (c2.id, "second", 1)
    );
    assert!(store.case_by_number(other.id, 2).await.unwrap().is_none());
    assert!(store.case_by_number(p.id, 3).await.unwrap().is_none());
    let snap = store
        .case_version_snapshot(c2.id, 1)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(snap.name, "second");
    assert_eq!(snap.body.steps[0].action, "second");
    assert!(
        store
            .case_version_snapshot(c2.id, 2)
            .await
            .unwrap()
            .is_none()
    );

    let l = launch_at(&db, &p, 0, Some("open"), false).await;
    let r1 = result_in(&db, &p, l, "passed").await;
    run_item(&db, &p, l, &c1, "passed", Some(r1)).await;
    run_item(&db, &p, l, &c2, "untested", None).await;
    let c3 = case_in(&store, &p, "third", None).await;
    run_item(&db, &p, l, &c3, "in_progress", None).await;
    let c4 = case_in(&store, &p, "fourth", None).await;
    run_item(&db, &p, l, &c4, "blocked", None).await;
    Store::recount_launch(&mut conn, l.0).await.unwrap();
    let (untested, in_progress, total, passed): (i32, i32, i32, i32) = sqlx::query_as(
        "select count_untested, count_in_progress, count_total, count_passed from launches where id = $1",
    )
    .bind(l.0)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!((untested, in_progress, total, passed), (1, 1, 1, 1));
    let launch = store.launch(p.id, l.0).await.unwrap().unwrap();
    let m = launch.manual.unwrap();
    assert_eq!(m.state, tm::ManualState::Open);
    assert_eq!(
        (
            m.total,
            m.untested,
            m.in_progress,
            m.passed,
            m.blocked,
            m.failed
        ),
        (4, 1, 1, 1, 1, 0)
    );
    let json = serde_json::to_value(&launch).unwrap();
    assert_eq!(json["manual"]["inProgress"], 1);
    sqlx::query("update results set series_id = $1 where id = $2")
        .bind(series)
        .bind(r1)
        .execute(&db.pool)
        .await
        .unwrap();
    let page = store
        .results(
            &launch,
            &fastproof_store::results::ResultFilter {
                limit: 10,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].source, tm::ResultSource::Automated);
    sqlx::query("update results set run_item_id = $1, test_case_id = $2 where id = $3")
        .bind(Uuid::now_v7())
        .bind(tc)
        .bind(r1)
        .execute(&db.pool)
        .await
        .unwrap();
    let page = store
        .results(
            &launch,
            &fastproof_store::results::ResultFilter {
                limit: 10,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(page.items[0].source, tm::ResultSource::Manual);
    assert_eq!(page.items[0].case_number, Some(1));
    assert!(page.items[0].run_item_id.is_some());
}

#[tokio::test]
async fn launches_record_whether_the_creator_may_sync() {
    let Some((db, store)) = TestDb::migrated().await else {
        return;
    };
    let p = project(&store, "sync").await;
    let start = fastproof_store::launches::LaunchStart {
        name: "a".into(),
        ..Default::default()
    };
    let (a, _) = store.start_launch(&p, &start, "ci").await.unwrap();
    let (b, _) = store.start_launch_as(&p, &start, "ci", true).await.unwrap();
    let flags: Vec<bool> = sqlx::query_scalar(
        "select created_by_can_sync from launches where id = any($1) order by number",
    )
    .bind(vec![a.id, b.id])
    .fetch_all(&db.pool)
    .await
    .unwrap();
    assert_eq!(flags, vec![false, true]);
}

#[tokio::test]
async fn partitions_keep_manual_runs() {
    let Some((db, store)) = TestDb::migrated().await else {
        return;
    };
    store.ensure_partitions(8, 1).await.unwrap();
    let p = project(&store, "parts").await;
    let mut s = store.project_settings(p.id).await.unwrap();
    s.retention.raw_days = 60;
    store.put_project_settings(p.id, &s).await.unwrap();
    let c1 = case_in(&store, &p, "m1", None).await;
    let c2 = case_in(&store, &p, "m2", None).await;
    let month = |at: chrono::DateTime<chrono::Utc>| format!("results_p{}", at.format("%Y%m"));
    // Month A: a rolled-up automated launch and a closed manual run.
    let auto = launch_at(&db, &p, 125, None, true).await;
    result_in(&db, &p, auto, "passed").await;
    let closed = launch_at(&db, &p, 125, Some("closed"), true).await;
    let closed_result = result_in(&db, &p, closed, "failed").await;
    let closed_item = run_item(&db, &p, closed, &c1, "failed", Some(closed_result)).await;
    // Month B: an open manual run (rolled up or not, it blocks).
    let open = launch_at(&db, &p, 160, Some("open"), true).await;
    let open_result = result_in(&db, &p, open, "passed").await;
    let open_item = run_item(&db, &p, open, &c2, "passed", Some(open_result)).await;
    let (part_a, part_b) = (month(closed.1), month(open.1));
    assert_ne!(part_a, part_b);
    let projects = store.all_project_settings().await.unwrap();
    let dropped = store.drop_rolled_partitions(&projects).await.unwrap();
    assert!(dropped.contains(&part_a), "{dropped:?}");
    assert!(!dropped.contains(&part_b), "{dropped:?}");
    let exists = |id: Uuid| {
        let pool = db.pool.clone();
        async move {
            sqlx::query_scalar::<_, bool>("select exists (select 1 from launches where id = $1)")
                .bind(id)
                .fetch_one(&pool)
                .await
                .unwrap()
        }
    };
    assert!(!exists(auto.0).await);
    assert!(exists(closed.0).await);
    assert!(exists(open.0).await);
    let (result_id, status): (Option<Uuid>, String) =
        sqlx::query_as("select result_id, status from run_items where id = $1")
            .bind(closed_item)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!((result_id, status.as_str()), (None, "failed"));
    let open_ref: Option<Uuid> =
        sqlx::query_scalar("select result_id from run_items where id = $1")
            .bind(open_item)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(open_ref, Some(open_result));
    let rolled: bool =
        sqlx::query_scalar("select rolled_up_at is not null from launches where id = $1")
            .bind(closed.0)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert!(rolled);
    let l = store.launch(p.id, closed.0).await.unwrap().unwrap();
    assert_eq!(l.manual.unwrap().failed, 1);
}

#[tokio::test]
async fn retention_deletes_manual_runs_only_with_manual_days() {
    let Some((db, store)) = TestDb::migrated().await else {
        return;
    };
    let p = project(&store, "ret").await;
    let mut s = store.project_settings(p.id).await.unwrap();
    s.retention.raw_days = 30;
    store.put_project_settings(p.id, &s).await.unwrap();
    let c = case_in(&store, &p, "m", None).await;
    let auto = launch_at(&db, &p, 45, None, true).await;
    let closed = launch_at(&db, &p, 45, Some("closed"), true).await;
    run_item(&db, &p, closed, &c, "passed", None).await;
    let open = launch_at(&db, &p, 45, Some("open"), false).await;
    let recent = launch_at(&db, &p, 5, Some("closed"), false).await;
    let preview = store.retention_preview(p.id, &s).await.unwrap();
    assert_eq!(preview.launch_ids, vec![auto.0]);
    store
        .retention_apply(Duration::from_secs(60))
        .await
        .unwrap();
    let left: Vec<Uuid> =
        sqlx::query_scalar("select id from launches where project_id = $1 order by id")
            .bind(p.id)
            .fetch_all(&db.pool)
            .await
            .unwrap();
    let mut want = vec![closed.0, open.0, recent.0];
    want.sort();
    assert_eq!(left, want);
    s.retention.manual_days = 20;
    store.put_project_settings(p.id, &s).await.unwrap();
    store
        .retention_apply(Duration::from_secs(60))
        .await
        .unwrap();
    let left: Vec<Uuid> =
        sqlx::query_scalar("select id from launches where project_id = $1 order by id")
            .bind(p.id)
            .fetch_all(&db.pool)
            .await
            .unwrap();
    let mut want = vec![open.0, recent.0];
    want.sort();
    assert_eq!(left, want);
}

#[tokio::test]
async fn attachment_gc_keeps_case_and_step_attachments() {
    let Some((db, store)) = TestDb::migrated().await else {
        return;
    };
    let p = project(&store, "gc").await;
    let c = case_in(&store, &p, "with files", None).await;
    let l = launch_at(&db, &p, 0, Some("open"), false).await;
    let item = run_item(&db, &p, l, &c, "in_progress", None).await;
    for round in 0..2 {
        let mk = |n: &str| {
            let pool = db.pool.clone();
            let n = format!("{n}{round}");
            async move {
                let id = Uuid::now_v7();
                sqlx::query(
                    "insert into attachments (id, project_id, sha256, size, content_type, blob_key, last_seen_at) \
                     values ($1, $2, $3, 1, 'image/png', $4, now() - interval '30 days')",
                )
                .bind(id)
                .bind(p.id)
                .bind(&n)
                .bind(format!("blobs/{n}"))
                .execute(&pool)
                .await
                .unwrap();
                id
            }
        };
        let of_case = mk("case").await;
        let of_step = mk("step").await;
        let loose = mk("loose").await;
        sqlx::query(
            "insert into case_attachments (case_id, attachment_id, name, content_type) values ($1, $2, 'a.png', 'image/png')",
        )
        .bind(c.id)
        .bind(of_case)
        .execute(&db.pool)
        .await
        .unwrap();
        sqlx::query(
            "insert into run_step_results (run_item_id, step_path, status, attachment_ids, updated_by) \
             values ($1, $2, 'failed', $3, 'tester')",
        )
        .bind(item)
        .bind(format!("{}", round + 1))
        .bind(vec![of_step])
        .execute(&db.pool)
        .await
        .unwrap();
        let n = if round == 0 {
            store
                .collect_attachments(Duration::ZERO, 100)
                .await
                .unwrap()
        } else {
            store
                .queue_unreferenced_attachments(p.id, Duration::ZERO, 100)
                .await
                .unwrap()
        };
        assert_eq!(n, 1, "round {round}");
        let left: Vec<Uuid> = sqlx::query_scalar("select id from attachments where id = any($1)")
            .bind(vec![of_case, of_step, loose])
            .fetch_all(&db.pool)
            .await
            .unwrap();
        assert!(
            left.contains(&of_case) && left.contains(&of_step) && !left.contains(&loose),
            "round {round}"
        );
    }
}
