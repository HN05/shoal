//! Best-effort trust entries in the config files of the tools Shoal launches.
use anyhow::Context as _;

use super::AgentEnvironment;
use crate::{
    agent::trust::TrustConfig,
    cli::{client, commands::workspaces, context::Context},
    env,
    model::{Repository, Workspace},
};

/// A tool's config file that records the directories it trusts, so a
/// launch never stops at the tool's trust dialog. Updates are best effort and
/// only warn.
pub(in crate::cli) struct TrustFile {
    entries: &'static TrustConfig,
    /// The tool's name in warnings.
    tool: &'static str,
}

/// Whose agents ran with a config location: a workspace's, or a chat's in
/// the repository's checkout.
enum Holder<'a> {
    Workspace(&'a Workspace),
    Repository(&'a Repository),
}

pub(super) static CLAUDE_CODE: TrustFile = TrustFile {
    entries: &crate::agent::trust::CLAUDE_CODE,
    tool: crate::agent::trust::CLAUDE_CODE.tool,
};

pub(super) static CODEX: TrustFile = TrustFile {
    entries: &crate::agent::trust::CODEX,
    tool: crate::agent::trust::CODEX.tool,
};

impl TrustFile {
    /// Trust the workspace's repository directories for the tool running
    /// with `environment`.
    pub(super) async fn trust_workspace(
        &self,
        ctx: &Context,
        workspace: &Workspace,
        environment: &AgentEnvironment<'_>,
    ) {
        let paths = workspace_trust_paths(ctx, workspace, self.tool).await;
        self.trust(environment, &paths);
    }

    /// [`Self::trust_workspace`] for a tracked agent, which runs with the
    /// workspace's `[env]` changed by `profile`, after the daemon keeps
    /// where that puts the tool's config for repository removal.
    pub(super) async fn trust_tracked(
        &self,
        ctx: &Context,
        workspace: &Workspace,
        profile: &crate::ai::Environment,
    ) {
        let mut configured = workspaces::agent_environment(ctx, workspace).await;
        profile.layer(&mut configured, &ctx.paths.home);
        self.keep_config_location(ctx, Holder::Workspace(workspace), &configured)
            .await;
        let environment = AgentEnvironment {
            home: &ctx.paths.home,
            configured: &configured,
        };
        self.trust_workspace(ctx, workspace, &environment).await;
    }

    /// Have the daemon keep where `configured` puts the tool's config for
    /// the removal of `holder`'s repository. Failure only warns.
    async fn keep_config_location(
        &self,
        ctx: &Context,
        holder: Holder<'_>,
        configured: &std::collections::BTreeMap<String, std::ffi::OsString>,
    ) {
        let environment = env::agent_config_environment(configured);
        if environment.is_empty() {
            return;
        }
        let kept = match holder {
            Holder::Workspace(workspace) => {
                client::keep_agent_environment(&ctx.paths, &workspace.id, environment).await
            }
            Holder::Repository(repository) => {
                client::keep_repository_agent_environment(&ctx.paths, &repository.id, environment)
                    .await
            }
        };
        if let Err(error) = kept {
            eprintln!(
                "warning: repository removal may leave {} trust entries behind: {error:#}",
                self.tool
            );
        }
    }

    /// Trust a repository's checkout for a chat, which runs with this
    /// process's environment changed by `profile`, as workspace launches
    /// trust theirs; repository removal clears the entries.
    pub(super) async fn trust_checkout(
        &self,
        ctx: &Context,
        repository: &Repository,
        profile: &crate::ai::Environment,
    ) {
        let mut configured = std::collections::BTreeMap::new();
        profile.layer(&mut configured, &ctx.paths.home);
        self.keep_config_location(ctx, Holder::Repository(repository), &configured)
            .await;
        let environment = AgentEnvironment {
            home: &ctx.paths.home,
            configured: &configured,
        };
        self.trust(
            &environment,
            &repository_trust_paths(repository, &repository.path),
        );
    }

    fn trust(&self, environment: &AgentEnvironment, paths: &[std::path::PathBuf]) {
        let config = match self
            .entries
            .config(environment.home, environment.configured)
        {
            Ok(config) => config,
            Err(error) => {
                eprintln!(
                    "warning: could not mark directories as trusted for {}: {error:#}",
                    self.tool
                );
                return;
            }
        };
        for path in paths {
            if let Err(error) = self.entries.add(&config, path) {
                eprintln!(
                    "warning: could not mark {} as trusted for {}: {error:#}",
                    path.display(),
                    self.tool
                );
            }
        }
    }

    /// Remove the entries for `paths` from the config file of each of
    /// `environments`, while preserving other project settings.
    pub(super) fn forget(
        &self,
        environments: &[AgentEnvironment],
        paths: &[std::path::PathBuf],
    ) -> Vec<String> {
        let (configs, mut warnings) = self.configs(environments);
        for config in configs {
            if let Err(error) = self.entries.remove(&config, paths) {
                warnings.push(format!(
                    "could not clean {} trust config {}: {error:#}",
                    self.tool,
                    config.display()
                ));
            }
        }
        warnings
    }

    /// The distinct config files the tool reads with `environments`, and a
    /// warning for each environment that names none.
    fn configs(&self, environments: &[AgentEnvironment]) -> (Vec<std::path::PathBuf>, Vec<String>) {
        let mut configs = Vec::new();
        let mut warnings = Vec::new();
        for environment in environments {
            match self
                .entries
                .config(environment.home, environment.configured)
            {
                Ok(config) if !configs.contains(&config) => configs.push(config),
                Ok(_) => {}
                Err(error) => warnings.push(format!(
                    "could not clean {} trust config: {error:#}",
                    self.tool
                )),
            }
        }
        (configs, warnings)
    }
}

/// The directories a repository's removal stops trusting: its checkout, its
/// workspaces directory and its workspaces.
pub(super) fn repository_paths(
    repository: &Repository,
    workspace_paths: &[std::path::PathBuf],
) -> Vec<std::path::PathBuf> {
    let mut paths = Vec::new();
    for path in std::iter::once(&repository.path)
        .chain(repository.workspaces_dir.iter())
        .chain(workspace_paths.iter())
    {
        let path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_owned());
        if !paths.contains(&path) {
            paths.push(path);
        }
    }
    paths
}

/// The workspace's repository trust paths, or the workspace alone when its
/// repository cannot be resolved.
async fn workspace_trust_paths(
    ctx: &Context,
    workspace: &Workspace,
    agent: &str,
) -> Vec<std::path::PathBuf> {
    match client::repositories(&ctx.paths).await.and_then(|repos| {
        repos
            .into_iter()
            .find(|repo| repo.id == workspace.repository_id)
            .context("workspace repository is missing")
    }) {
        Ok(repo) => repository_trust_paths(&repo, &workspace.path),
        Err(error) => {
            eprintln!("warning: could not resolve repository trust paths for {agent}: {error:#}");
            vec![workspace.path.clone()]
        }
    }
}

/// Trust the repository's checkout and the shared workspaces directory for
/// normal Shoal worktrees. Claude Code keys every worktree's trust by the
/// checkout and searches parents only up to the worktree root; the directory
/// covers versions that search every parent. An explicitly placed worktree
/// outside it needs its own project entry.
fn repository_trust_paths(
    repository: &Repository,
    workspace: &std::path::Path,
) -> Vec<std::path::PathBuf> {
    let mut paths = vec![repository.path.clone()];
    paths.extend(repository.workspaces_dir.iter().cloned());
    let shared = repository
        .workspaces_dir
        .as_ref()
        .is_some_and(|directory| workspace.starts_with(directory));
    if !shared && !paths.iter().any(|path| path == workspace) {
        paths.push(workspace.to_owned());
    }
    paths
}
