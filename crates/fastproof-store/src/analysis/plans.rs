//! Test plans for selective runs, served as Allure `testplan.json`
//! (`{"version": "1.0", "tests": [{"id": "...", "selector": "..."}]}`): Allure adapters run
//! only the tests whose `ALLURE_ID` equals an `id` or whose full name equals a `selector`.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{Store, json_no_nul, no_nul};

/// Tests per plan, at most.
pub const MAX_PLAN_TESTS: usize = 50_000;
/// Test case ids listed in a request, at most.
pub const MAX_PLAN_IDS: usize = 10_000;
/// Test plans kept per project, at most; creating more is refused until old ones expire.
pub const MAX_PLANS_PER_PROJECT: i64 = 1000;
/// Test plans are deleted this many days after they were created (by the retention job).
pub const TEST_PLAN_DAYS: i64 = 90;

/// Body of `POST .../test-plans`.
#[derive(Clone, Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NewTestPlan {
    pub name: String,
    /// Take the tests of this launch with `statuses`.
    pub from_launch_id: Option<Uuid>,
    /// Statuses picked from the launch (default `failed`, `broken`).
    pub statuses: Option<Vec<String>>,
    /// Test cases added to the plan.
    pub test_case_ids: Option<Vec<Uuid>>,
    /// `managed` creates a managed plan (needs `plans:write`) from `name`, `description`,
    /// `caseNumbers` and `filter`; absent or `adhoc` is the stage-1 plan above.
    #[serde(default)]
    pub kind: Option<crate::tm::TestPlanKind>,
    /// Managed plans: Markdown description.
    #[serde(default)]
    pub description: Option<String>,
    /// Managed plans: cases added explicitly.
    #[serde(default)]
    pub case_numbers: Option<Vec<i64>>,
    /// Managed plans: cases added (and kept in sync) by a filter.
    #[serde(default)]
    pub filter: Option<crate::tm::CaseFilter>,
}

/// A created plan.
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TestPlanCreated {
    pub id: Uuid,
    pub test_count: i64,
}

/// One test of an Allure test plan.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct TestPlanTest {
    /// `ALLURE_ID` of the test, when it has one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Full name as the Allure adapter reports it.
    pub selector: String,
}

/// Allure `testplan.json` (version 1.0).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct TestPlanJson {
    pub version: String,
    pub tests: Vec<TestPlanTest>,
    /// Muted tests of a managed plan (read by allure-js; also listed in `tests`).
    #[serde(
        rename = "mutedTests",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub muted_tests: Option<Vec<TestPlanTest>>,
}

impl NewTestPlan {
    /// A managed plan was asked for.
    pub fn is_managed(&self) -> bool {
        self.kind == Some(crate::tm::TestPlanKind::Managed)
    }

    /// The body of a managed plan; the adhoc fields are refused.
    pub fn managed(&self) -> Result<crate::tm::TestPlanWrite, String> {
        if self.from_launch_id.is_some() || self.statuses.is_some() || self.test_case_ids.is_some()
        {
            return Err("managed plans take caseNumbers and filter, not fromLaunchId, statuses or testCaseIds".into());
        }
        Ok(crate::tm::TestPlanWrite {
            name: self.name.clone(),
            description: self.description.clone(),
            filter: self.filter.clone().map(Some),
            case_numbers: self.case_numbers.clone(),
        })
    }

    pub fn validate(&self) -> Result<Vec<String>, String> {
        if self.description.is_some() || self.case_numbers.is_some() || self.filter.is_some() {
            return Err("description, caseNumbers and filter need kind managed".into());
        }
        if self.name.trim().is_empty() || self.name.len() > 200 {
            return Err("name must have 1 to 200 bytes".into());
        }
        if self.from_launch_id.is_none() && self.test_case_ids.is_none() {
            return Err("give fromLaunchId or testCaseIds".into());
        }
        if let Some(ids) = &self.test_case_ids
            && ids.len() > MAX_PLAN_IDS
        {
            return Err(format!("at most {MAX_PLAN_IDS} testCaseIds"));
        }
        let statuses = self
            .statuses
            .clone()
            .unwrap_or_else(|| vec!["failed".into(), "broken".into()]);
        if statuses.is_empty() || statuses.len() > 5 {
            return Err("statuses must list 1 to 5 statuses".into());
        }
        for s in &statuses {
            if !matches!(
                s.as_str(),
                "passed" | "failed" | "broken" | "skipped" | "unknown"
            ) {
                return Err("statuses: passed, failed, broken, skipped or unknown".into());
            }
        }
        Ok(statuses)
    }
}

/// Why a plan could not be made.
#[derive(Debug)]
pub enum PlanRefusal {
    UnknownLaunch,
    TooManyTests,
    /// The project has [`MAX_PLANS_PER_PROJECT`] plans.
    TooManyPlans,
}

impl Store {
    /// Builds and stores a plan (validated request, `statuses` from [`NewTestPlan::validate`]).
    pub async fn create_test_plan(
        &self,
        project_id: Uuid,
        req: &NewTestPlan,
        statuses: &[String],
        by: &str,
    ) -> anyhow::Result<Result<TestPlanCreated, PlanRefusal>> {
        let plans: i64 = sqlx::query_scalar(
            "select count(*) from (select 1 from test_plans where project_id = $1 and kind = 'adhoc' limit $2) x",
        )
        .bind(project_id)
        .bind(MAX_PLANS_PER_PROJECT)
        .fetch_one(&self.pool)
        .await?;
        if plans >= MAX_PLANS_PER_PROJECT {
            return Ok(Err(PlanRefusal::TooManyPlans));
        }
        let mut tc_ids: Vec<Uuid> = req.test_case_ids.clone().unwrap_or_default();
        if let Some(l) = req.from_launch_id {
            let created: Option<DateTime<Utc>> = sqlx::query_scalar(
                "select created_at from launches where project_id = $1 and id = $2",
            )
            .bind(project_id)
            .bind(l)
            .fetch_optional(&self.pool)
            .await?;
            let Some(created) = created else {
                return Ok(Err(PlanRefusal::UnknownLaunch));
            };
            let ids: Vec<Uuid> = sqlx::query_scalar(
                "select distinct test_case_id from results where launch_id = $1 and launch_created_at = $2 \
                 and status = any($3) limit $4",
            )
            .bind(l)
            .bind(created)
            .bind(statuses)
            .bind(MAX_PLAN_TESTS as i64 + 1)
            .fetch_all(&self.pool)
            .await?;
            tc_ids.extend(ids);
        }
        tc_ids.sort();
        tc_ids.dedup();
        if tc_ids.len() > MAX_PLAN_TESTS {
            return Ok(Err(PlanRefusal::TooManyTests));
        }
        let rows: Vec<(String, Option<String>, Option<String>)> = sqlx::query_as(
            "select full_name, explicit_id, explicit_id_source from test_cases \
             where project_id = $1 and id = any($2)",
        )
        .bind(project_id)
        .bind(&tc_ids)
        .fetch_all(&self.pool)
        .await?;
        let mut tests: BTreeMap<String, TestPlanTest> = BTreeMap::new();
        for (full_name, explicit, source) in rows {
            let id = explicit.filter(|_| source.as_deref() == Some("allure_id"));
            tests
                .entry(format!("{full_name}\n{}", id.clone().unwrap_or_default()))
                .or_insert(TestPlanTest {
                    id,
                    selector: full_name,
                });
        }
        let tests: Vec<TestPlanTest> = tests.into_values().collect();
        let id = Uuid::now_v7();
        sqlx::query(
            "insert into test_plans (id, project_id, name, from_launch_id, statuses, tests, test_count, created_by) \
             values ($1, $2, $3, $4, $5, $6, $7, $8)",
        )
        .bind(id)
        .bind(project_id)
        .bind(no_nul(req.name.trim()))
        .bind(req.from_launch_id)
        .bind(statuses)
        .bind(json_no_nul(serde_json::to_value(&tests)?))
        .bind(tests.len() as i32)
        .bind(no_nul(by))
        .execute(&self.pool)
        .await?;
        Ok(Ok(TestPlanCreated {
            id,
            test_count: tests.len() as i64,
        }))
    }

    /// Deletes up to `batch` adhoc plans older than [`TEST_PLAN_DAYS`]; returns how many.
    /// Managed plans never expire.
    pub async fn delete_old_test_plans(&self, batch: i64) -> anyhow::Result<u64> {
        Ok(sqlx::query(
            "delete from test_plans where id in (select id from test_plans where kind = 'adhoc' \
             and created_at < now() - make_interval(days => $1) order by created_at limit $2)",
        )
        .bind(TEST_PLAN_DAYS as i32)
        .bind(batch)
        .execute(&self.pool)
        .await?
        .rows_affected())
    }

    /// A plan as Allure `testplan.json`.
    pub async fn test_plan_json(
        &self,
        project_id: Uuid,
        id: Uuid,
    ) -> anyhow::Result<Option<TestPlanJson>> {
        let v: Option<serde_json::Value> =
            sqlx::query_scalar("select tests from test_plans where project_id = $1 and id = $2")
                .bind(project_id)
                .bind(id)
                .fetch_optional(&self.pool)
                .await?;
        Ok(v.map(|v| TestPlanJson {
            version: "1.0".into(),
            tests: serde_json::from_value(v).unwrap_or_default(),
            muted_tests: None,
        }))
    }
}
