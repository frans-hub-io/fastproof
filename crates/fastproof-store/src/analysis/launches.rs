//! Launch changes after the fact: the important flag (kept by retention), name, attributes.

use serde::Deserialize;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{Attribute, Launch, Store, json_no_nul, no_nul};

/// Body of `PATCH .../launches/{id}`.
#[derive(Clone, Debug, Default, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LaunchPatch {
    pub important: Option<bool>,
    pub name: Option<String>,
    pub attributes: Option<Vec<Attribute>>,
}

impl Store {
    /// Applies a validated patch; `None` when the launch does not exist.
    pub async fn patch_launch(
        &self,
        project_id: Uuid,
        project_key: &str,
        id: Uuid,
        p: &LaunchPatch,
    ) -> anyhow::Result<Option<Launch>> {
        let mut tx = self.pool.begin().await?;
        let attrs = p
            .attributes
            .as_ref()
            .map(serde_json::to_value)
            .transpose()?
            .map(json_no_nul);
        let updated: Option<Uuid> = sqlx::query_scalar(
            "update launches set important = coalesce($3, important), name = coalesce($4, name), \
             attributes = coalesce($5, attributes), updated_at = now() \
             where project_id = $1 and id = $2 returning id",
        )
        .bind(project_id)
        .bind(id)
        .bind(p.important)
        .bind(p.name.as_deref().map(|n| no_nul(n.trim())))
        .bind(attrs)
        .fetch_optional(&mut *tx)
        .await?;
        if updated.is_none() {
            return Ok(None);
        }
        crate::events::emit(
            &mut tx,
            "launch.updated",
            Some(project_key),
            serde_json::json!({"id": id, "important": p.important}),
        )
        .await?;
        crate::events::notify_launch(&mut tx, project_key, id).await?;
        tx.commit().await?;
        self.launch(project_id, id).await
    }
}
