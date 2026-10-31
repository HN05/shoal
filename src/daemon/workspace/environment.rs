//! Persistent scope for processes whose lifetime belongs to an external caller.
use super::Manager;
use crate::{
    daemon::{scope::Caller, store},
    env,
};
use anyhow::{Context, Result, ensure};
use rusqlite::{OptionalExtension, params};
use std::collections::BTreeMap;
use uuid::Uuid;

impl Manager {
    pub(crate) async fn caller(&self, token: &str) -> Result<Option<Caller>> {
        if let Some(caller) = self.executions.scope(token).await {
            return Ok(Some(caller));
        }
        if let Some(caller) = self.operations.caller(token) {
            return Ok(Some(caller));
        }
        let token = token.to_owned();
        self.store
            .run(move |db| {
                Ok(db
                    .query_row(
                        "SELECT workspace_id FROM workspace_scopes WHERE token=?1",
                        [token],
                        |row| {
                            Ok(Caller {
                                workspace_id: row.get(0)?,
                                execution: None,
                            })
                        },
                    )
                    .optional()?)
            })
            .await
    }

    pub(crate) async fn workspace_environment(
        &self,
        selector: &str,
    ) -> Result<BTreeMap<String, String>> {
        let workspace = self.workspace(selector).await?;
        self.reserve_repository_directory(&workspace.repository_id)
            .await?;
        let _guard = self.gates.lock_git(&workspace.repository_id).await;
        let workspace = self.workspace(&workspace.id).await?;
        self.verify_worktree(&workspace).await?;
        let settings = self.workspace_settings(&workspace).await?.into_settings();
        let repository = self.repository(&workspace.repository_id).await?;
        let caches = crate::caches::directories(&repository, &settings.caches)?;
        let tmp_dir = crate::tmp_dirs::directory(&repository, &workspace.id)?;
        let configured = settings.env;
        let repository_name = crate::repository::name(&repository).to_owned();
        let workspace_id = workspace.id.clone();
        let token = Uuid::new_v4().to_string();
        let paths = self.paths.clone();
        self.store
            .run(move |db| {
                let tx = db.transaction()?;
                store::require_ready(&tx, &workspace_id)?;
                let ports = store::ports(&tx, Some(&workspace_id))?;
                let mut values = env::render_configured_environment(
                    &configured,
                    &workspace,
                    &repository_name,
                    &ports,
                    &caches,
                    tmp_dir.as_deref(),
                );
                values.extend(env::workspace_environment(
                    &workspace, &paths, &ports, &token,
                ));
                let values = values
                    .into_iter()
                    .map(|(name, value)| {
                        let value = value.into_string().ok().with_context(|| {
                            format!("{name} cannot be exported as JSON: invalid UTF-8")
                        })?;
                        Ok((name, value))
                    })
                    .collect::<Result<BTreeMap<_, _>>>()?;
                tx.execute(
                    "INSERT INTO workspace_scopes(token,workspace_id) VALUES (?1,?2)",
                    params![token, workspace_id],
                )?;
                tx.commit()?;
                Ok(values)
            })
            .await
    }

    pub(crate) async fn revoke_workspace_environment(
        &self,
        selector: &str,
        token: String,
    ) -> Result<()> {
        let workspace_id = self.workspace(selector).await?.id;
        self.store
            .run(move |db| {
                let deleted = db.execute(
                    "DELETE FROM workspace_scopes WHERE workspace_id=?1 AND token=?2",
                    params![workspace_id, token],
                )?;
                ensure!(deleted == 1, "unknown workspace environment token");
                Ok(())
            })
            .await
    }
}

#[cfg(test)]
mod tests;
