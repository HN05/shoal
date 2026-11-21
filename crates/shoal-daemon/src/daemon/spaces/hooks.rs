//! The hooks that follow a space's agent, a workspace's or a chat's, which
//! run where that agent works.
use anyhow::Result;
use std::path::{Path, PathBuf};

use crate::{
    config::{Effective, hooks::HookCommand},
    daemon::manager::Manager,
    hooks::{self, Hook, HookKind},
    model::Space,
};

impl Manager {
    /// The hook of `kind` that `settings` configure, in where the agent of
    /// `space` works.
    pub(crate) fn space_hook(
        space: &Space,
        kind: HookKind,
        settings: &Effective,
    ) -> Option<PathBuf> {
        (kind.command(settings)).map(|path| space.workdir.path().join(path))
    }

    /// Run `hook`'s `command` in `space` with what `[env]` adds: its
    /// repository's, or the global config's for a space without one.
    pub(crate) async fn run_space_hook(
        &self,
        hook: Hook<'_>,
        space: &Space,
        settings: &Effective,
        command: &Path,
    ) -> Result<()> {
        let repository = space.repository_id.as_ref();
        let configured_env = (self.hook_environment(space, repository, settings)).await?;
        (hooks::run_detached(hook, space, command, &self.paths, &configured_env)).await
    }
}
