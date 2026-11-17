//! The hooks that follow a space's agent, a workspace's or a chat's, which
//! run where that agent works.
use anyhow::Result;
use std::path::{Path, PathBuf};

use crate::{
    config::{Effective, hooks::HookCommand},
    daemon::workspace::Manager,
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

    /// Run `hook`'s `command` in `space` with what its repository's `[env]`
    /// adds, which a space without a repository goes without.
    pub(crate) async fn run_space_hook(
        &self,
        hook: Hook<'_>,
        space: &Space,
        settings: &Effective,
        command: &Path,
    ) -> Result<()> {
        let configured_env = match &space.repository_id {
            Some(repository) => (self.hook_environment(space, repository, settings)).await?,
            None => Default::default(),
        };
        (hooks::run_detached(hook, space, command, &self.paths, &configured_env)).await
    }
}
