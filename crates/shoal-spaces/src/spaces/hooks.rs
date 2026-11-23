//! The hooks that follow a space's agent, a workspace's or a chat's, which
//! run where that agent works.
use crate::Spaces;
use crate::daemon::Daemon;
use anyhow::Result;
use std::path::{Path, PathBuf};

use shoal_ids::space_ids::SpaceId;

use crate::{
    config::{Effective, hooks::HookCommand},
    gates::GuardMode,
    hooks::{self, Hook, HookKind},
    model::Space,
    state::WorkspaceState,
};

impl Spaces {
    /// Run `hooks.post_agent_exit` where the agent of `space` worked, unless
    /// a removal has the space, which runs its own hooks.
    pub async fn run_agent_exit_hook(
        &self,
        daemon: &dyn Daemon,
        space: &crate::model::Space,
        agent: &crate::agent::Agent,
        exit_code: Option<i32>,
        complete: bool,
    ) -> anyhow::Result<()> {
        if !self.agent_exit_space_ready(&space.id).await? {
            return Ok(());
        }
        let settings = daemon.space_settings(space).await?;
        let Some(command) = Self::space_hook(space, HookKind::PostAgentExit, &settings) else {
            return Ok(());
        };
        let _resources = self.gates.resource_guard(&space.id, GuardMode::Exclusive)?;
        // Recheck after excluding new transitions.
        if !self.agent_exit_space_ready(&space.id).await? {
            return Ok(());
        }
        self.verify_space(daemon, space).await?;
        let hook = Hook::PostAgentExit {
            agent,
            exit_code,
            complete,
        };
        (self.run_space_hook(daemon, hook, space, &settings, &command)).await
    }

    /// Whether the space `id` is a chat or a ready workspace that no
    /// removal holds.
    async fn agent_exit_space_ready(&self, id: &SpaceId) -> anyhow::Result<bool> {
        if self.space_removals.reserved(id) {
            return Ok(false);
        }
        let id = id.to_owned();
        self.store
            .run(move |db| {
                crate::store::exists(
                    db,
                    "SELECT 1 FROM spaces LEFT JOIN workspaces USING (id)
                     WHERE spaces.id=?1 AND (spaces.kind='chat' OR workspaces.state=?2)",
                    rusqlite::params![id, WorkspaceState::Ready],
                )
            })
            .await
    }

    /// The hook of `kind` that `settings` configure, in where the agent of
    /// `space` works.
    pub fn space_hook(space: &Space, kind: HookKind, settings: &Effective) -> Option<PathBuf> {
        (kind.command(settings)).map(|path| space.workdir.path().join(path))
    }

    /// Run `hook`'s `command` in `space` with what `[env]` adds: its
    /// repository's, or the global config's for a space without one.
    pub async fn run_space_hook(
        &self,
        daemon: &dyn Daemon,
        hook: Hook<'_>,
        space: &Space,
        settings: &Effective,
        command: &Path,
    ) -> Result<()> {
        let repository = space.repository_id.as_ref();
        let configured_env = (self.hook_environment(daemon, space, repository, settings)).await?;
        (hooks::run_detached(hook, space, command, &self.paths, &configured_env)).await
    }
}
