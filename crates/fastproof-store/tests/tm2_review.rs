//! Workflow and review in the store (TM next §7.2, §7.3): the `requiredFields` guard and
//! states of cases written before the workflow, `resetOn: any`, the shared step save hook, and
//! pruning that keeps the approved version. They need `FASTPROOF_TEST_DATABASE_URL`; without
//! it they pass without doing anything.

use fastproof_store::analysis::settings::ProjectSettings;
use fastproof_store::projects::CreateProject;
use fastproof_store::tm::cases::CaseCtx;
use fastproof_store::tm::{
    self, CaseFieldWrite, CasePatch, CaseReviewSettings, CaseWorkflowSettings, CaseWrite,
    FieldType, ReviewDecision, ReviewImports, ReviewKind, ReviewMode, ReviewResetOn, ReviewState,
    ReviewVoteWrite, SharedStepOrigin, TmError, TmLimits, VersionSource, WorkflowGuard,
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

fn ctx<'a>(p: &'a Project, by: &'a str, limits: &'a TmLimits, can_review: bool) -> CaseCtx<'a> {
    CaseCtx {
        project_id: p.id,
        project_key: &p.key,
        by,
        limits,
        can_review,
    }
}

fn policy(reset_on: ReviewResetOn) -> CaseReviewSettings {
    CaseReviewSettings {
        mode: ReviewMode::Manual,
        approvals: 1,
        reset_on,
        allow_self_approval: false,
        imports: ReviewImports::Review,
    }
}

#[tokio::test]
async fn required_fields_guard_and_states_from_before_the_workflow() {
    let Some((_db, store)) = TestDb::migrated().await else {
        return;
    };
    let p = project(&store, "rf").await;
    let limits = TmLimits::default();
    let alice = ctx(&p, "alice", &limits, true);
    let old = store
        .create_case(
            &alice,
            &CaseWrite {
                name: "Before the workflow".into(),
                status: Some(tm::CaseStatus::Active),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert!(old.state.is_none());

    let mut w = CaseWorkflowSettings::default();
    w.manual.transitions[0].requires = Some(vec![WorkflowGuard::RequiredFields]);
    let s = ProjectSettings {
        case_workflow: Some(w),
        ..Default::default()
    };
    store.put_project_settings(p.id, &s).await.unwrap();
    store
        .create_case_field(
            p.id,
            &CaseFieldWrite {
                key: "approver".into(),
                name: "Approver".into(),
                field_type: FieldType::Text,
                options: None,
                required: Some(true),
                default_value: None,
                source_label: None,
                position: None,
            },
            &limits,
        )
        .await
        .unwrap();

    let c = store
        .create_case(
            &alice,
            &CaseWrite {
                name: "Draft".into(),
                status: Some(tm::CaseStatus::Draft),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(c.state.as_deref(), Some("draft"));
    let e = store
        .transition_case(&alice, c.number, c.revision, "ready_for_review")
        .await
        .unwrap_err();
    match e {
        TmError::Conflict { code, details, .. } => {
            assert_eq!(code, "invalid_transition");
            assert_eq!(
                details.unwrap()["missing"],
                serde_json::json!(["requiredFields"])
            );
        }
        e => panic!("{e:?}"),
    }
    let (_, c) = store
        .patch_case(
            &alice,
            c.number,
            c.revision,
            &CasePatch {
                fields: Some(
                    [(
                        "approver".to_string(),
                        serde_json::from_value(serde_json::json!("qa")).unwrap(),
                    )]
                    .into(),
                ),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let (_, c) = store
        .transition_case(&alice, c.number, c.revision, "ready_for_review")
        .await
        .unwrap();
    assert_eq!(c.state.as_deref(), Some("ready_for_review"));
    assert_eq!(c.status, tm::CaseStatus::Draft);

    let thread = store
        .review_thread(p.id, ReviewKind::Case, old.number, "bob", false)
        .await
        .unwrap();
    let to: Vec<(&str, bool)> = thread
        .transitions
        .iter()
        .map(|t| (t.to.as_str(), t.allowed))
        .collect();
    assert_eq!(
        to,
        vec![("outdated", false)],
        "a case without a state is in the state its category stands for (active)"
    );
    assert_eq!(
        thread.transitions[0].missing,
        vec!["permission", "requiredFields"]
    );
    let (_, moved) = store
        .patch_case(
            &alice,
            old.number,
            old.revision,
            &CasePatch {
                fields: Some(
                    [(
                        "approver".to_string(),
                        serde_json::from_value(serde_json::json!("qa")).unwrap(),
                    )]
                    .into(),
                ),
                status: Some(tm::CaseStatus::Outdated),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(moved.state.as_deref(), Some("outdated"));
}

#[tokio::test]
async fn reset_on_any_and_the_shared_step_hook() {
    let Some((db, store)) = TestDb::migrated().await else {
        return;
    };
    let p = project(&store, "ra").await;
    let limits = TmLimits::default();
    let alice = ctx(&p, "alice", &limits, true);
    let bob = ctx(&p, "bob", &limits, true);
    for (reset_on, tag_resets) in [(ReviewResetOn::Content, false), (ReviewResetOn::Any, true)] {
        let s = ProjectSettings {
            case_review: policy(reset_on),
            ..Default::default()
        };
        store.put_project_settings(p.id, &s).await.unwrap();
        let c = store
            .create_case(
                &alice,
                &CaseWrite {
                    name: format!("{reset_on:?}"),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(c.review_state, Some(ReviewState::Pending));
        let (t, _) = store
            .vote(
                &bob,
                ReviewKind::Case,
                c.number,
                c.revision,
                &ReviewVoteWrite {
                    version: 1,
                    decision: ReviewDecision::Approve,
                    comment: None,
                },
            )
            .await
            .unwrap();
        assert_eq!(t.review_state, Some(ReviewState::Approved));
        let cur = store.case_view(p.id, c.number).await.unwrap();
        let (_, after) = store
            .patch_case(
                &alice,
                c.number,
                cur.revision,
                &CasePatch {
                    tags: Some(vec!["smoke".into()]),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        if tag_resets {
            assert_eq!(
                after.review_state,
                Some(ReviewState::Pending),
                "resetOn: any"
            );
            assert_eq!(after.approved_version, Some(1));
        } else {
            assert_eq!(after.review_state, Some(ReviewState::Approved));
            assert_eq!(after.approved_version, Some(2));
        }
        let e = store
            .vote(
                &ctx(&p, "carol", &limits, false),
                ReviewKind::Case,
                c.number,
                after.revision,
                &ReviewVoteWrite {
                    version: after.version,
                    decision: ReviewDecision::Approve,
                    comment: None,
                },
            )
            .await
            .unwrap_err();
        assert!(
            matches!(e, TmError::Forbidden { .. }),
            "the store checks cases:review too"
        );
    }

    let id = Uuid::now_v7();
    sqlx::query(
        "insert into shared_steps (id, project_id, number, name, steps, origin, ai_unreviewed, created_by) \
         values ($1, $2, 1, 'Log in', '[]', 'ai', true, 'copilot')",
    )
    .bind(id)
    .bind(p.id)
    .execute(&db.pool)
    .await
    .unwrap();
    let row = |v: i32, state: Option<ReviewState>, approved: Option<i32>| tm::SharedStepRow {
        id,
        project_id: p.id,
        number: 1,
        name: "Log in".into(),
        description: String::new(),
        steps: Vec::new(),
        version: v,
        approved_version: approved,
        review_state: state,
        revision: 1,
        origin: SharedStepOrigin::Ai,
        ai_unreviewed: true,
        created_by: Some("copilot".into()),
        created_at: chrono::Utc::now(),
        updated_by: None,
        updated_at: chrono::Utc::now(),
        archived_at: None,
    };
    let read = || async {
        let r: (Option<String>, Option<i32>) =
            sqlx::query_as("select review_state, approved_version from shared_steps where id = $1")
                .bind(id)
                .fetch_one(&db.pool)
                .await
                .unwrap();
        r
    };
    let s = ProjectSettings {
        case_review: policy(ReviewResetOn::Content),
        ..Default::default()
    };
    store.put_project_settings(p.id, &s).await.unwrap();
    let mut conn = store.pool().acquire().await.unwrap();
    let copilot = ctx(&p, "copilot", &limits, false);
    tm::review::on_shared_step_save(
        &mut conn,
        &copilot,
        None,
        &row(1, None, None),
        &["name".into(), "steps".into()],
        VersionSource::Ai,
    )
    .await
    .unwrap();
    assert_eq!(read().await, (Some("pending".into()), None));
    sqlx::query(
        "update shared_steps set review_state = 'approved', approved_version = 1 where id = $1",
    )
    .bind(id)
    .execute(&db.pool)
    .await
    .unwrap();
    tm::review::on_shared_step_save(
        &mut conn,
        &alice,
        Some(&row(1, Some(ReviewState::Approved), Some(1))),
        &row(2, Some(ReviewState::Approved), Some(1)),
        &["description".into()],
        VersionSource::Ui,
    )
    .await
    .unwrap();
    assert_eq!(
        read().await,
        (Some("approved".into()), Some(2)),
        "description needs no review"
    );
    tm::review::on_shared_step_save(
        &mut conn,
        &alice,
        Some(&row(2, Some(ReviewState::Approved), Some(2))),
        &row(3, Some(ReviewState::Approved), Some(2)),
        &["steps".into()],
        VersionSource::Ui,
    )
    .await
    .unwrap();
    assert_eq!(
        read().await,
        (Some("pending".into()), Some(2)),
        "runs keep v2"
    );
    drop(conn);
    sqlx::query(
        "insert into shared_step_versions (shared_step_id, version, project_id, source, changed, snapshot_z, created_by) \
         values ($1, 3, $2, 'ui', '{steps}', '\\x00', 'alice')",
    )
    .bind(id)
    .bind(p.id)
    .execute(&db.pool)
    .await
    .unwrap();
    sqlx::query("update shared_steps set version = 3 where id = $1")
        .bind(id)
        .execute(&db.pool)
        .await
        .unwrap();
    let e = store
        .vote(
            &ctx(&p, "copilot", &limits, true),
            ReviewKind::SharedStep,
            1,
            1,
            &ReviewVoteWrite {
                version: 3,
                decision: ReviewDecision::Approve,
                comment: None,
            },
        )
        .await
        .unwrap_err();
    assert!(
        matches!(
            e,
            TmError::Forbidden {
                code: "ai_self_review",
                ..
            }
        ),
        "the creator of an AI shared step never approves it: {e:?}"
    );
    let (t, _) = store
        .vote(
            &bob,
            ReviewKind::SharedStep,
            1,
            1,
            &ReviewVoteWrite {
                version: 3,
                decision: ReviewDecision::Approve,
                comment: Some("fine".into()),
            },
        )
        .await
        .unwrap();
    assert_eq!(t.approved_version, Some(3));
    assert_eq!(t.votes[0].comment, "fine");
}

#[tokio::test]
async fn pruning_keeps_the_approved_version() {
    let Some((db, store)) = TestDb::migrated().await else {
        return;
    };
    let p = project(&store, "pr").await;
    let limits = TmLimits::default();
    let alice = ctx(&p, "alice", &limits, true);
    let mut s = ProjectSettings {
        case_review: policy(ReviewResetOn::Content),
        ..Default::default()
    };
    s.retention.case_versions_keep = 1;
    store.put_project_settings(p.id, &s).await.unwrap();
    let c = store
        .create_case(
            &alice,
            &CaseWrite {
                name: "v1".into(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    store
        .vote(
            &ctx(&p, "bob", &limits, true),
            ReviewKind::Case,
            c.number,
            c.revision,
            &ReviewVoteWrite {
                version: 1,
                decision: ReviewDecision::Approve,
                comment: None,
            },
        )
        .await
        .unwrap();
    for name in ["v2", "v3", "v4"] {
        let cur = store.case_view(p.id, c.number).await.unwrap();
        store
            .update_case(
                &alice,
                c.number,
                cur.revision,
                &CaseWrite {
                    name: name.into(),
                    ..Default::default()
                },
                true,
            )
            .await
            .unwrap();
    }
    let cur = store.case_view(p.id, c.number).await.unwrap();
    assert_eq!((cur.version, cur.approved_version), (4, Some(1)));
    sqlx::query(
        "update case_versions set created_at = now() - interval '2 days' where case_id = $1",
    )
    .bind(c.id)
    .execute(&db.pool)
    .await
    .unwrap();
    let prune = || async {
        sqlx::query(
            "update case_versions set created_at = now() - interval '2 days' where case_id = $1",
        )
        .bind(c.id)
        .execute(&db.pool)
        .await
        .unwrap();
        store
            .prune_case_versions(std::time::Duration::from_secs(30), 100, 0)
            .await
            .unwrap()
    };
    let left = || async {
        sqlx::query_scalar::<_, i32>(
            "select version from case_versions where case_id = $1 order by version",
        )
        .bind(c.id)
        .fetch_all(&db.pool)
        .await
        .unwrap()
    };
    assert_eq!(
        prune().await,
        0,
        "the versions of a change waiting for review stay (their authors may not approve it)"
    );
    assert_eq!(left().await, vec![1, 2, 3, 4]);
    store
        .vote(
            &ctx(&p, "bob", &limits, true),
            ReviewKind::Case,
            c.number,
            cur.revision,
            &ReviewVoteWrite {
                version: 4,
                decision: ReviewDecision::Approve,
                comment: None,
            },
        )
        .await
        .unwrap();
    let cur = store.case_view(p.id, c.number).await.unwrap();
    store
        .update_case(
            &alice,
            c.number,
            cur.revision,
            &CaseWrite {
                name: "v5".into(),
                ..Default::default()
            },
            true,
        )
        .await
        .unwrap();
    assert_eq!(prune().await, 3, "v1 to v3 go once v4 is approved");
    assert_eq!(
        left().await,
        vec![4, 5],
        "the approved version stays runnable"
    );
}

/// Four eyes (TM next §7.3, security review): whoever wrote part of the change under review
/// does not approve it, even when someone else wrote a later version on top; names compare
/// without case; an import with `imports: bypass` approves only its own change; a write that
/// changes content and status at once does not pass the `approved` guard with the old head's
/// approval.
#[tokio::test]
async fn four_eyes_hold_across_versions_names_imports_and_guards() {
    let Some((_db, store)) = TestDb::migrated().await else {
        return;
    };
    let p = project(&store, "fe").await;
    let limits = TmLimits::default();
    let alice = ctx(&p, "alice", &limits, true);
    let bob = ctx(&p, "bob", &limits, true);
    let carol = ctx(&p, "carol", &limits, true);
    let mut s = ProjectSettings {
        case_review: policy(ReviewResetOn::Content),
        ..Default::default()
    };
    store.put_project_settings(p.id, &s).await.unwrap();
    let approve = |version: i32| ReviewVoteWrite {
        version,
        decision: ReviewDecision::Approve,
        comment: None,
    };
    let code = |e: TmError| match e {
        TmError::Forbidden { code, .. } | TmError::Conflict { code, .. } => code,
        other => panic!("unexpected {other:?}"),
    };

    let c = store
        .create_case(
            &alice,
            &CaseWrite {
                name: "Pay".into(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let (_, tagged) = store
        .patch_case(
            &bob,
            c.number,
            c.revision,
            &CasePatch {
                tags: Some(vec!["smoke".into()]),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(
        (tagged.version, tagged.review_state),
        (2, Some(ReviewState::Pending))
    );
    let e = store
        .vote(
            &alice,
            ReviewKind::Case,
            c.number,
            tagged.revision,
            &approve(2),
        )
        .await
        .unwrap_err();
    assert_eq!(
        code(e),
        "self_approval",
        "alice wrote v1 under review although bob wrote the head"
    );
    let e = store
        .vote(
            &ctx(&p, "ALICE", &limits, true),
            ReviewKind::Case,
            c.number,
            tagged.revision,
            &approve(2),
        )
        .await
        .unwrap_err();
    assert_eq!(code(e), "self_approval", "names compare without case");
    let r = store
        .bulk_review(
            &alice,
            &tm::ReviewBulk {
                items: vec![tm::ReviewBulkItem {
                    kind: ReviewKind::Case,
                    number: c.number,
                    version: 2,
                }],
                decision: ReviewDecision::Approve,
                comment: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(
        (r.applied, r.skipped[0].code.as_str()),
        (0, "self_approval")
    );
    let q = tm::review::ReviewQueueQuery {
        reviewer_me: true,
        limit: 50,
        ..Default::default()
    };
    let mine = store.review_queue(p.id, &q, "Alice", true).await.unwrap();
    assert!(mine.items.is_empty(), "not in alice's queue: {mine:?}");
    let t = store
        .review_thread(p.id, ReviewKind::Case, c.number, "alice", true)
        .await
        .unwrap();
    assert_eq!(t.why_not.as_deref(), Some("self_approval"));
    let theirs = store.review_queue(p.id, &q, "carol", true).await.unwrap();
    assert_eq!(theirs.items.len(), 1);
    let e = store
        .vote(
            &bob,
            ReviewKind::Case,
            c.number,
            tagged.revision,
            &approve(2),
        )
        .await
        .unwrap_err();
    assert_eq!(code(e), "self_approval", "bob wrote the head");
    let (t, _) = store
        .vote(
            &carol,
            ReviewKind::Case,
            c.number,
            tagged.revision,
            &approve(2),
        )
        .await
        .unwrap();
    assert_eq!(t.review_state, Some(ReviewState::Approved));

    s.case_review.approvals = 2;
    store.put_project_settings(p.id, &s).await.unwrap();
    let d = store
        .create_case(
            &alice,
            &CaseWrite {
                name: "Refund".into(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let (_, rev) = store
        .vote(&carol, ReviewKind::Case, d.number, d.revision, &approve(1))
        .await
        .unwrap();
    let (t, _) = store
        .vote(
            &ctx(&p, "Carol", &limits, true),
            ReviewKind::Case,
            d.number,
            rev,
            &approve(1),
        )
        .await
        .unwrap();
    assert_eq!(
        (t.review_state, t.approvals),
        (Some(ReviewState::Pending), 1),
        "carol and Carol are one approver"
    );

    s.case_review.approvals = 1;
    s.case_review.imports = ReviewImports::Bypass;
    store.put_project_settings(p.id, &s).await.unwrap();
    let cur = store.case_view(p.id, c.number).await.unwrap();
    let (_, edited) = store
        .update_case(
            &alice,
            c.number,
            cur.revision,
            &CaseWrite {
                name: "Pay twice".into(),
                ..Default::default()
            },
            true,
        )
        .await
        .unwrap();
    assert_eq!(
        (edited.review_state, edited.approved_version),
        (Some(ReviewState::Pending), Some(2))
    );
    let import = ctx(&p, "importer", &limits, false);
    let imported = {
        let mut tx = store.pool().begin().await.unwrap();
        let cur = tm::cases::lock_case_by_number(&mut tx, p.id, c.number, None)
            .await
            .unwrap();
        let w = CaseWrite {
            name: cur.name.clone(),
            folder_id: cur.folder_id,
            status: Some(cur.status),
            body: Some(cur.body.clone()),
            fields: Some(cur.fields.clone()),
            tags: Some(vec!["imported".into()]),
        };
        let row = tm::cases::update_case_in(&mut tx, &import, &cur, &w, VersionSource::Import)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        row
    };
    assert_eq!(imported.version, edited.version + 1);
    assert_eq!(
        (imported.review_state, imported.approved_version),
        (Some(ReviewState::Pending), Some(2)),
        "the import does not approve alice's waiting change"
    );

    s.case_review.imports = ReviewImports::Review;
    s.case_workflow = Some(CaseWorkflowSettings::default());
    store.put_project_settings(p.id, &s).await.unwrap();
    let w = store
        .create_case(
            &alice,
            &CaseWrite {
                name: "Ship".into(),
                status: Some(tm::CaseStatus::Draft),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let (_, rev) = store
        .vote(&bob, ReviewKind::Case, w.number, w.revision, &approve(1))
        .await
        .unwrap();
    let (_, ready) = store
        .transition_case(&alice, w.number, rev, "ready_for_review")
        .await
        .unwrap();
    let e = store
        .update_case(
            &alice,
            w.number,
            ready.revision,
            &CaseWrite {
                name: "Ship anything".into(),
                status: Some(tm::CaseStatus::Active),
                ..Default::default()
            },
            true,
        )
        .await
        .unwrap_err();
    match e {
        TmError::Conflict { code, details, .. } => {
            assert_eq!(code, "invalid_transition");
            assert_eq!(details.unwrap()["missing"], serde_json::json!(["approved"]));
        }
        other => panic!("unexpected {other:?}"),
    }
    let cur = store.case_view(p.id, w.number).await.unwrap();
    let (_, active) = store
        .update_case(
            &alice,
            w.number,
            cur.revision,
            &CaseWrite {
                name: "Ship".into(),
                status: Some(tm::CaseStatus::Active),
                ..Default::default()
            },
            true,
        )
        .await
        .unwrap();
    assert_eq!(
        (active.state.as_deref(), active.review_state),
        (Some("active"), Some(ReviewState::Approved)),
        "the status alone passes with the approved head"
    );
}

/// §14 targets at 20k cases with 5k waiting for review: the queue of 50 < 150 ms p95 and a bulk
/// review of 500 < 2 s. Run with `--release -- --ignored`.
#[tokio::test]
#[ignore]
async fn review_queue_and_bulk_at_scale() {
    let Some((db, store)) = TestDb::migrated().await else {
        return;
    };
    let p = project(&store, "scale").await;
    let s = ProjectSettings {
        case_review: CaseReviewSettings {
            approvals: 1,
            ..policy(ReviewResetOn::Content)
        },
        ..Default::default()
    };
    store.put_project_settings(p.id, &s).await.unwrap();
    sqlx::query(
        "insert into cases (id, project_id, number, name, status, origin, body, created_by, updated_by, \
           review_state, updated_at) \
         select gen_random_uuid(), $1, g, 'Case ' || g, 'active', 'ui', '{}'::jsonb, 'alice', 'alice', \
           case when g % 4 = 0 then 'pending' end, now() - make_interval(secs => g) \
         from generate_series(1, 20000) g",
    )
    .bind(p.id)
    .execute(&db.pool)
    .await
    .unwrap();
    sqlx::query(
        "insert into case_versions (case_id, version, project_id, source, changed, snapshot_z, created_by) \
         select id, 1, project_id, 'ui', '{name,steps}', '\\x00', 'alice' from cases where project_id = $1",
    )
    .bind(p.id)
    .execute(&db.pool)
    .await
    .unwrap();
    sqlx::query("update projects set case_seq = 20000 where id = $1")
        .bind(p.id)
        .execute(&db.pool)
        .await
        .unwrap();
    sqlx::query("analyze cases")
        .execute(&db.pool)
        .await
        .unwrap();
    sqlx::query("analyze case_versions")
        .execute(&db.pool)
        .await
        .unwrap();
    let mut times = Vec::new();
    for i in 0..30 {
        let q = tm::review::ReviewQueueQuery {
            reviewer_me: i % 2 == 1,
            limit: 50,
            ..Default::default()
        };
        let t = std::time::Instant::now();
        let page = store.review_queue(p.id, &q, "bob", true).await.unwrap();
        times.push(t.elapsed());
        assert_eq!(page.items.len(), 50);
    }
    times.sort();
    let p95 = times[times.len() * 95 / 100];
    eprintln!(
        "review queue of 50 at 20k cases (5k pending): p95 {p95:?}, median {:?}",
        times[15]
    );
    assert!(p95 < std::time::Duration::from_millis(150), "{p95:?}");

    let items: Vec<tm::ReviewBulkItem> = (1..=500)
        .map(|k| tm::ReviewBulkItem {
            kind: ReviewKind::Case,
            number: k * 4,
            version: 1,
        })
        .collect();
    let limits = TmLimits::default();
    let t = std::time::Instant::now();
    let r = store
        .bulk_review(
            &ctx(&p, "bob", &limits, true),
            &tm::ReviewBulk {
                items,
                decision: ReviewDecision::Approve,
                comment: Some("one change".into()),
            },
        )
        .await
        .unwrap();
    let took = t.elapsed();
    eprintln!("bulk review of 500: {took:?}");
    assert_eq!(r.applied, 500, "{:?}", r.skipped.first());
    assert!(took < std::time::Duration::from_secs(2), "{took:?}");
}
