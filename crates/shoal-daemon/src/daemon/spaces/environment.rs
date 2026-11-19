//! The variables a space gives the processes an external caller starts in
//! it, without the scope token, which [`super::scopes`] issues on its own.
use crate::{
    daemon::{manager::Manager, store},
    env,
    model::{ByKind, Chat, Workspace},
};
use anyhow::{Context, Result};
use shoal_protocol::space_names::SpaceSelector;
use std::{collections::BTreeMap, ffi::OsString};

impl Manager {
    /// The variables a process of the space `selector` names runs with:
    /// the space, the state directory and its temporary directory, and for
    /// a workspace, whose worktree must be ready and verified, its
    /// worktree's, ports and configured `[env]`.
    pub async fn space_environment(
        &self,
        selector: &SpaceSelector,
    ) -> Result<BTreeMap<String, String>> {
        let values = match self.space(selector).await?.by_kind() {
            ByKind::Workspace(workspace) => self.workspace_environment(workspace).await?,
            ByKind::Chat(chat) => self.chat_environment(&chat).await?,
        };
        exported(values)
    }

    async fn workspace_environment(
        &self,
        workspace: Workspace,
    ) -> Result<BTreeMap<String, OsString>> {
        let _guard = self.gates.lock_git(workspace.repository()).await;
        let workspace = self.workspace_by_id(&workspace.id).await?;
        self.verify_worktree(&workspace).await?;
        let settings = self.workspace_settings(&workspace).await?.into_settings();
        let repository = self.repository(workspace.repository()).await?;
        let caches = crate::caches::directories(&repository, &settings.caches)?;
        let tmp_dir = crate::tmp_dirs::directory(&repository, &workspace.id)?;
        let configured = settings.env;
        let repository_name = repository.display_name().to_owned();
        let workspace_id = workspace.id.clone();
        let paths = self.paths.clone();
        self.store
            .read(move |tx| {
                store::require_ready(tx, &workspace_id)?;
                let ports = store::ports(tx, Some(&workspace_id))?;
                let mut values = env::render_configured_environment(
                    &configured,
                    &workspace,
                    &repository_name,
                    &ports,
                    &caches,
                    Some(&tmp_dir),
                );
                values.extend(env::workspace_environment(
                    &(&workspace).into(),
                    &paths,
                    &ports,
                ));
                Ok(values)
            })
            .await
    }

    async fn chat_environment(&self, chat: &Chat) -> Result<BTreeMap<String, OsString>> {
        let repository = self.chat_repository(chat).await?;
        let tmp_dir = self.chat_tmp_dir(repository.as_ref(), chat)?;
        Ok(env::chat_environment(
            &chat.id,
            tmp_dir.as_deref(),
            &self.paths,
        ))
    }
}

/// `values` as JSON carries them, refusing one that is not UTF-8.
fn exported(values: BTreeMap<String, OsString>) -> Result<BTreeMap<String, String>> {
    values
        .into_iter()
        .map(|(name, value)| {
            let value = value
                .into_string()
                .ok()
                .with_context(|| format!("{name} cannot be exported as JSON: invalid UTF-8"))?;
            Ok((name, value))
        })
        .collect()
}

#[cfg(test)]
mod tests;
