//! A chat becoming a workspace: the refusals, the issue it takes, the
//! worktree and its setup, after which its agent restarts there when its
//! turn ends.
use crate::Spaces;
use crate::daemon::Daemon;
use crate::daemon::OpenIssue;
use crate::forge::items::reference::ItemInput;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::model::ByKind;
use std::{collections::BTreeMap, sync::Arc};

use anyhow::{Context as _, Result, bail, ensure};

use super::{Carrying, Converting, to_agent::switching};
use crate::{
    config::modes::ModeName,
    model::{Chat, OpenedWorkspace, SpaceKind},
    protocol::{AgentChange, CreationOptions, operations::StartedWorkspace},
    {
        scope::Caller,
        starts::{Preparing, Recorded},
    },
};
use shoal_protocol::space_names::SpaceSelector;

/// What a chat becomes: a workspace on `branch`, which the issue `issue`
/// links when given, from `base` when the branch is new, leaving out what
/// `options` say, with its agent in `mode`, carrying on as `agent` asks.
pub struct WorkspaceConversion {
    pub branch: String,
    pub issue: Option<crate::forge::items::reference::ItemInput>,
    pub base: Option<String>,
    pub options: CreationOptions,
    pub mode: ModeName,
    pub agent: AgentChange,
}

impl Spaces {
    /// Turn the chat `selector` names into the workspace `conversion`
    /// describes, for the space `caller` runs in, and start preparing it in
    /// the caller's environment `env`. Its agent restarts there, resuming its
    /// session, once its turn ends and the workspace is ready, or another
    /// agent tool takes its work over then, also from a stopped agent.
    pub async fn convert_to_workspace(
        &self,
        daemon: &Arc<dyn Daemon>,
        selector: &SpaceSelector,
        conversion: WorkspaceConversion,
        env: Option<BTreeMap<String, String>>,
        caller: Option<&Caller>,
    ) -> Result<StartedWorkspace> {
        let chat = self.convertible_chat(selector).await?;
        let repository = (chat.repository_id.clone()).context("the chat has no repository")?;
        let settings = daemon
            .repository_settings_with_templates(&repository)
            .await?;
        super::ensure_mode(&settings, &conversion.mode)?;
        ensure!(
            !conversion.options.disposable,
            "a converted chat's workspace keeps its work; leave out --disposable"
        );
        let converting = self.converting_chat(&**daemon, chat, &settings, &conversion, caller);
        let converting = converting.await?;
        let warnings = match &conversion.issue {
            Some(input) => {
                let chat = &converting.chat;
                (self.link_converted_issue(&**daemon, chat, input, caller)).await?
            }
            None => Vec::new(),
        };
        // Held until the preparation is recorded, which the relocation then
        // waits for.
        let making = self.make_converted(daemon, converting, conversion, env, caller);
        let mut started = self.conversion_handovers.hold(making).await?;
        // The relocation waits for the agent's turn to end, which it may
        // have already.
        self.signals.agent_changed();
        started.warnings.extend(warnings);
        Ok(started)
    }

    /// How `chat` becomes a workspace as `conversion` asks for `caller`,
    /// under its repository's `settings`: its running agent carries on there
    /// with the permissions `conversion` gives it, or another agent tool
    /// takes the work over, also from a stopped agent.
    async fn converting_chat(
        &self,
        daemon: &dyn Daemon,
        chat: Chat,
        settings: &crate::config::Effective,
        conversion: &WorkspaceConversion,
        caller: Option<&Caller>,
    ) -> Result<Converting> {
        let current = self.agent_of(&chat.id).await?;
        let place = (settings, (SpaceKind::Workspace, true));
        let (mode, change) = (&conversion.mode, &conversion.agent);
        let taking = switching(chat.narrowing, place, mode, change, current.as_ref())?;
        ensure!(
            chat.running || taking.is_some(),
            "chat {} is stopped; restore it before converting it, or hand its work to another agent tool with --agent",
            chat.name
        );
        let carried = (taking.as_ref(), change);
        (self.ensure_carrying_held(daemon, &chat.id, carried, caller)).await?;
        if !chat.running {
            // Not an agent that waits in the agent queue, which the handoff
            // could not replace once the chat is a workspace.
            let space = self.space(&SpaceSelector::id(&chat.id)).await?;
            daemon.ensure_replaceable(&space).await?;
        }
        let narrowing = change.permissions.applied_to(chat.narrowing);
        Ok(Converting {
            chat: Chat { narrowing, ..chat },
            mode: mode.clone(),
            carrying: Carrying {
                model: change.model.clone(),
                taking: taking.map(Box::new),
            },
        })
    }

    /// Make the workspace `converting` turns its chat into as `conversion`
    /// describes, and start preparing it in `env` for `caller`, recording
    /// the preparation the chat's agent waits for.
    async fn make_converted(
        &self,
        daemon: &Arc<dyn Daemon>,
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
            (self.create_converted_workspace(&**daemon, converting, branch, base, options)).await?;
        let recorded = Recorded::from(OpenedWorkspace {
            workspace,
            reused: false,
        });
        let preparing = Preparing {
            issue: None,
            pr: None,
            agent: None,
            waiting: None,
            queue_entry: None,
            blockers: Vec::new(),
            unfollowed: false,
            env,
        };
        let started = self.begin_recorded(daemon, recorded, None, preparing, caller);
        let started = started.await?.into_workspace()?;
        // Its worktree exists, so nothing holds its preparation.
        let operation =
            (started.operation.clone()).context("the conversion's preparation waits")?;
        let id = started.workspace.id.clone();
        self.store
            .run(move |db| super::record::record_preparation(db, &id, &operation))
            .await?;
        Ok(started)
    }

    /// The chat `selector` names, refusing a workspace and a chat that
    /// cannot become one wherever its agent stands.
    async fn convertible_chat(&self, selector: &SpaceSelector) -> Result<Chat> {
        let chat = match self.space(selector).await?.by_kind() {
            ByKind::Chat(chat) => chat,
            ByKind::Workspace(workspace) => {
                bail!("{} is a workspace already", workspace.name)
            }
        };
        self.ensure_not_converting(&chat.id, &chat.name).await?;
        if let Some(workspace) = chat.workdir.workspace() {
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
            !self.space_removals.reserved(&chat.id),
            "chat {} is being removed",
            chat.name
        );
        Ok(chat)
    }

    /// Assign `chat` the open issue `input` names in its repository, which
    /// the workspace it becomes keeps, as `caller`'s account on the forge,
    /// returning what that warns of.
    async fn link_converted_issue(
        &self,
        daemon: &dyn Daemon,
        chat: &Chat,
        input: &ItemInput,
        caller: Option<&Caller>,
    ) -> Result<Vec<String>> {
        let checkout = self.chat_checkout_path(daemon, chat).await?;
        let OpenIssue { url, title, .. } = daemon.read_open_issue(&checkout, input).await?;
        let selector = SpaceSelector::id(&chat.id);
        (daemon.set_issue(&selector, &url.into(), Some(title), caller)).await
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
    pub async fn converting(&self, id: &SpaceId) -> Result<bool> {
        let space = id.clone();
        let pending = self.store.run(move |db| super::pending(db, &space)).await?;
        Ok(pending.is_some())
    }

    /// Whether a conversion moves the agent of the space `id` to the new
    /// place: one that restarts it there waits for its turn to end, or a
    /// relocation is moving it now.
    pub async fn moves_agent(&self, id: &SpaceId) -> Result<bool> {
        if self.relocating.contains(id) {
            return Ok(true);
        }
        let space = id.clone();
        let pending = self.store.run(move |db| super::pending(db, &space)).await?;
        Ok(pending.is_some_and(|pending| pending.restarts_agent()))
    }
}

#[cfg(test)]
mod tests;
