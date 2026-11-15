//! A workspace's exported environment, for processes whose lifetime belongs
//! to an external caller, with a scope token that lasts until it revokes it.
use super::Manager;
use crate::client::scope_tokens::ScopeToken;
use crate::client::space_names::SpaceSelector;
use crate::{
    daemon::{scope::tokens, store},
    env,
};
use anyhow::{Context, Result, ensure};
use std::collections::BTreeMap;

impl Manager {
    pub(crate) async fn workspace_environment(
        &self,
        selector: &SpaceSelector,
    ) -> Result<BTreeMap<String, String>> {
        let workspace = self.workspace(selector).await?;
        self.reserve_repository_directory(&workspace.repository_id)
            .await?;
        let _guard = self.gates.lock_git(&workspace.repository_id).await;
        let workspace = self.workspace_by_id(&workspace.id).await?;
        self.verify_worktree(&workspace).await?;
        let settings = self.workspace_settings(&workspace).await?.into_settings();
        let repository = self.repository(&workspace.repository_id).await?;
        let caches = crate::caches::directories(&repository, &settings.caches)?;
        let tmp_dir = crate::tmp_dirs::directory(&repository, &workspace.id)?;
        let configured = settings.env;
        let repository_name = crate::repository::name(&repository).to_owned();
        let workspace_id = workspace.id.clone();
        let paths = self.paths.clone();
        self.store
            .write(move |tx| {
                store::require_ready(tx, &workspace_id)?;
                let token = tokens::issue(tx, &workspace_id)?;
                let ports = store::ports(tx, Some(&workspace_id))?;
                let mut values = env::render_configured_environment(
                    &configured,
                    &workspace,
                    &repository_name,
                    &ports,
                    &caches,
                    tmp_dir.as_deref(),
                );
                values.extend(env::workspace_environment(
                    &(&workspace).into(),
                    &paths,
                    &ports,
                    &token,
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
                Ok(values)
            })
            .await
    }

    pub(crate) async fn revoke_workspace_environment(
        &self,
        selector: &SpaceSelector,
        token: ScopeToken,
    ) -> Result<()> {
        let workspace_id = self.workspace(selector).await?.id;
        self.store
            .run(move |db| {
                ensure!(
                    tokens::revoke(db, &workspace_id, &token)?,
                    "unknown workspace environment token"
                );
                Ok(())
            })
            .await
    }
}

#[cfg(test)]
mod tests;
