//! People who signed in through the identity provider (`tm_people`): the principal name, the
//! display name from the provider's claims and the groups at the last sign-in. The assignee
//! picker (`GET /people`) uses them to list members of bound groups and to show display names.
//! Rows not seen for [`PEOPLE_KEEP_DAYS`] are deleted.

use std::collections::HashMap;

use crate::{Store, no_nul};

/// Days a person is remembered after the last request.
pub const PEOPLE_KEEP_DAYS: i32 = 180;
/// Groups kept per person.
pub const MAX_PERSON_GROUPS: usize = 256;
/// Bytes of a display name.
pub const MAX_DISPLAY_BYTES: usize = 200;

/// A remembered person.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KnownPerson {
    pub principal: String,
    pub display: Option<String>,
    pub groups: Vec<String>,
}

impl Store {
    /// Records a person's request: the groups replace the stored ones, a display name replaces
    /// the stored one only when given.
    pub async fn note_person(
        &self,
        principal: &str,
        display: Option<&str>,
        groups: &[String],
    ) -> anyhow::Result<()> {
        let groups: Vec<String> = groups
            .iter()
            .take(MAX_PERSON_GROUPS)
            .map(|g| no_nul(g))
            .collect();
        sqlx::query(
            "insert into tm_people (principal, display, groups, last_seen) values ($1, $2, $3, now()) \
             on conflict (principal) do update set display = coalesce(excluded.display, tm_people.display), \
               groups = excluded.groups, last_seen = excluded.last_seen",
        )
        .bind(no_nul(principal))
        .bind(display.map(no_nul))
        .bind(&groups)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// People seen within [`PEOPLE_KEEP_DAYS`], most recent first, at most `limit`.
    pub async fn known_people(&self, limit: i64) -> anyhow::Result<Vec<KnownPerson>> {
        let rows: Vec<(String, Option<String>, Vec<String>)> = sqlx::query_as(
            "select principal, display, groups from tm_people \
             where last_seen > now() - make_interval(days => $1) order by last_seen desc limit $2",
        )
        .bind(PEOPLE_KEEP_DAYS)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|(principal, display, groups)| KnownPerson {
                principal,
                display,
                groups,
            })
            .collect())
    }

    /// Display names of the given principals that have one.
    pub async fn people_displays(
        &self,
        names: &[String],
    ) -> anyhow::Result<HashMap<String, String>> {
        let rows: Vec<(String, String)> = sqlx::query_as(
            "select principal, display from tm_people where principal = any($1) and display is not null",
        )
        .bind(names)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().collect())
    }

    /// Deletes people not seen for [`PEOPLE_KEEP_DAYS`].
    pub async fn prune_people(&self) -> anyhow::Result<u64> {
        Ok(
            sqlx::query(
                "delete from tm_people where last_seen < now() - make_interval(days => $1)",
            )
            .bind(PEOPLE_KEEP_DAYS)
            .execute(&self.pool)
            .await?
            .rows_affected(),
        )
    }
}
