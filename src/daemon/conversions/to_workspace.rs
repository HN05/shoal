//! A chat becoming a workspace: the refusals, the issue it takes, the
//! worktree and its setup, after which its agent restarts there when its
//! turn ends.
use crate::client::space_ids::SpaceId;
use std::{collections::BTreeMap, sync::Arc};

use anyhow::{Context as _, Result, bail, ensure};

use super::Converting;
use crate::client::space_names::SpaceSelector;
use crate::{
    config::modes::ModeName,
    daemon::{
        links::issue::OpenIssue, operations::starts::Preparing, scope::Caller, workspace::Manager,
    },
    model::{Chat, OpenedWorkspace, PresentSpace},
    protocol::{CreationOptions, operations::StartedWorkspace},
};

/// What a chat becomes: a workspace on `branch`, which the issue `issue`
/// links when given, from `base` when the branch is new, leaving out what
/// `options` say, with its agent in `mode`.
pub(crate) struct WorkspaceConversion {
    pub branch: String,
    pub issue: Option<String>,
    pub base: Option<String>,
    pub options: CreationOptions,
    pub mode: ModeName,
}

impl Manager {
    /// Turn the chat `selector` names into the workspace `conversion`
    /// describes, for the space `caller` runs in, and start preparing it in
    /// the caller's environment `env`. Its agent restarts there, resuming its
    /// session, once its turn ends and the workspace is ready.
    pub(crate) async fn convert_to_workspace(
        self: &Arc<Self>,
        selector: &SpaceSelector,
        conversion: WorkspaceConversion,
        env: Option<BTreeMap<String, String>>,
        caller: Option<&Caller>,
    ) -> Result<StartedWorkspace> {
        let chat = self.convertible_chat(selector, caller).await?;
        let repository = (chat.repository_id.clone()).context("the chat has no repository")?;
        let settings = self.repository_settings_with_templates(&repository).await?;
        super::ensure_mode(&settings, &conversion.mode)?;
        ensure!(
            !conversion.options.disposable,
            "a converted chat's workspace keeps its work; leave out --disposable"
        );
        if let Some(input) = &conversion.issue {
            self.link_converted_issue(&chat, input).await?;
        }
        let converting = Converting {
            chat,
            mode: conversion.mode.clone(),
        };
        // Held until the preparation is recorded, which the relocation then
        // waits for.
        let making = self.make_converted(converting, conversion, env, caller);
        let started = self.conversion_handovers.hold(making).await?;
        // The relocation waits for the agent's turn to end, which it may
        // have already.
        self.signals.agent_changed();
        Ok(started)
    }

    /// Make the workspace `converting` turns its chat into as `conversion`
    /// describes, and start preparing it in `env` for `caller`, recording
    /// the preparation the chat's agent waits for.
    async fn make_converted(
        self: &Arc<Self>,
        converting: Converting,
        conversion: WorkspaceConversion,
        env: Option<BTreeMap<String, String>>,
        caller: Option<&Caller>,
    ) -> Result<StartedWorkspace> {
        let WorkspaceConversion {
            branch,
            base,
            options,
            ..
        } = conversion;
        let workspace =
            (self.create_converted_workspace(converting, branch, base, options)).await?;
        let opened = OpenedWorkspace {
            workspace,
            reused: false,
        };
        let preparing = Preparing {
            issue: None,
            pr: None,
            agent: None,
            env,
        };
        let started = self.prepare_new(opened, None, preparing, caller).await?;
        let (id, operation) = (started.workspace.id.clone(), started.operation.clone());
        self.store
            .run(move |db| super::record::record_preparation(db, &id, &operation))
            .await?;
        Ok(started)
    }

    /// The chat `selector` names, refusing a workspace, a chat that cannot
    /// become one, and a space other than the one `caller` runs in.
    async fn convertible_chat(
        &self,
        selector: &SpaceSelector,
        caller: Option<&Caller>,
    ) -> Result<Chat> {
        let chat = match self.space(selector).await? {
            PresentSpace::Chat(chat) => chat,
            PresentSpace::Workspace(workspace) => {
                bail!("{} is a workspace already", workspace.name)
            }
        };
        self.ensure_own_space(&chat.id, &chat.name, caller)?;
        self.ensure_not_converting(&chat.id, &chat.name).await?;
        ensure!(
            chat.restore.is_none(),
            "chat {} is stopped; resume it before converting it",
            chat.name
        );
        if let Some(workspace) = &chat.workspace {
            bail!(
                "chat {} runs in the worktree of {}, which stays that workspace's; start a chat in the checkout to convert",
                chat.name,
                workspace.name
            );
        }
        ensure!(
            chat.repository_id.is_some(),
            "chat {} has no repository to make a workspace in",
            chat.name
        );
        ensure!(
            !self.chat_removals.reserved(&chat.id),
            "chat {} is being removed",
            chat.name
        );
        Ok(chat)
    }

    /// Assign `chat` the open issue `input` names in its repository, which
    /// the workspace it becomes keeps.
    async fn link_converted_issue(&self, chat: &Chat, input: &str) -> Result<()> {
        let checkout = self.chat_checkout_path(chat).await?;
        let OpenIssue { url, title } = self.read_open_issue(&checkout, input).await?;
        self.set_issue(&SpaceSelector::id(&chat.id), &url, Some(title))
            .await
    }

    /// Refuse a conversion of the space `id`, named `name`, by the process of
    /// a space other than it: an agent converts only its own space.
    pub(super) fn ensure_own_space(
        &self,
        id: &SpaceId,
        name: &str,
        caller: Option<&Caller>,
    ) -> Result<()> {
        if let Some(caller) = caller {
            ensure!(
                caller.space_id == *id,
                "an agent converts only its own space; ask the user to convert {name}"
            );
        }
        Ok(())
    }

    /// Refuse a conversion of the space `id`, named `name`, whose agent has
    /// yet to restart after another.
    pub(super) async fn ensure_not_converting(&self, id: &SpaceId, name: &str) -> Result<()> {
        ensure!(
            !self.converting(id).await?,
            "{name} is converting already; its agent restarts in the new place once its turn ends"
        );
        Ok(())
    }

    /// Whether the agent of the converted space `id` has yet to restart in
    /// the new place.
    pub(crate) async fn converting(&self, id: &SpaceId) -> Result<bool> {
        let space = id.clone();
        let pending = self.store.run(move |db| super::pending(db, &space)).await?;
        Ok(pending.is_some())
    }
}

#[cfg(test)]
mod tests;
