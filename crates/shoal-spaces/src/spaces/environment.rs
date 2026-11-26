//! The variables a space gives the processes an external caller starts in
//! it. The scope token is left out: [`super::scopes`] issues it on its own.
use crate::Spaces;
use crate::daemon::Daemon;
use crate::{
    env,
    model::{ByKind, Chat, Workspace},
    store,
};
use anyhow::{Context, Result};
use shoal_protocol::space_names::SpaceSelector;
use std::{collections::BTreeMap, ffi::OsString};

impl Spaces {
    /// The space, the state directory, its temporary directory and its
    /// configured `[env]`. A workspace, whose worktree must be ready and
    /// verified, adds its worktree's variables and ports, as its executions
    /// get them.
    pub async fn space_environment(
        &self,
        daemon: &dyn Daemon,
        selector: &SpaceSelector,
    ) -> Result<BTreeMap<String, String>> {
        let values = match self.space(selector).await?.by_kind() {
            ByKind::Workspace(workspace) => self.workspace_environment(daemon, workspace).await?,
            ByKind::Chat(chat) => self.chat_environment(daemon, chat).await?,
        };
        exported(values)
    }

    async fn workspace_environment(
        &self,
        daemon: &dyn Daemon,
        workspace: Workspace,
    ) -> Result<BTreeMap<String, OsString>> {
        let _guard = self.gates.lock_git(workspace.repository()).await;
        let workspace = self.workspace_by_id(&workspace.id).await?;
        self.verify_worktree(daemon, &workspace).await?;
        let settings = daemon.workspace_settings(&workspace).await?.into_settings();
        let repository = daemon
            .repository(crate::repository::Selector::from(workspace.repository()))
            .await?;
        let caches = crate::caches::directories(&repository, &settings.caches)?;
        let tmp_dir = shoal_core::tmp_dirs::directory(&repository, &workspace.id)?;
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
                    Some(&repository_name),
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

    async fn chat_environment(
        &self,
        daemon: &dyn Daemon,
        chat: Chat,
    ) -> Result<BTreeMap<String, OsString>> {
        let id = chat.id.clone();
        let mut values = daemon.chat_environment(chat).await?;
        values.extend(env::chat_environment(&id, &self.paths));
        Ok(values)
    }
}

/// Refuses a value that is not UTF-8.
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
