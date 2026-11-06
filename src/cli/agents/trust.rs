//! The trust entries a CLI launch adds for the directories its agent works
//! in, in the config files [`TrustFile`] edits.
use std::{collections::BTreeMap, ffi::OsString, path::PathBuf};

use anyhow::Context as _;

use crate::{
    agents::trust::{TrustFile, repository_trust_paths},
    cli::{commands::workspaces, context::Context},
    client::transcripts::AgentEnvironment,
    env,
    model::{ChatHome, Repository, Workspace},
};

/// Whose agents ran with a config location: a workspace's, or a chat's in
/// the repository's checkout.
enum Holder<'a> {
    Workspace(&'a Workspace),
    Repository(&'a Repository),
}

/// Trust the workspace's repository directories for the tool of `file`
/// running with `environment`.
pub(super) async fn workspace(
    file: &TrustFile,
    ctx: &Context,
    workspace: &Workspace,
    environment: &AgentEnvironment<'_>,
) {
    let paths = workspace_trust_paths(ctx, workspace, file.tool()).await;
    file.trust(environment, &paths);
}

/// [`workspace`] for a tracked agent, which runs with the workspace's
/// `[env]` changed by `profile`, after the daemon keeps where that puts the
/// tool's config for repository removal.
pub(super) async fn tracked(
    file: &TrustFile,
    ctx: &Context,
    workspace: &Workspace,
    profile: &crate::ai::Environment,
) {
    let mut configured = workspaces::agent_environment(ctx, workspace).await;
    profile.layer(&mut configured, &ctx.paths.home);
    keep_config_location(file, ctx, Holder::Workspace(workspace), &configured).await;
    let environment = AgentEnvironment::new(&ctx.paths.home, &configured);
    self::workspace(file, ctx, workspace, &environment).await;
}

/// Have the daemon keep where `configured` puts the config of `file`'s tool
/// for the removal of `holder`'s repository. Failure only warns.
async fn keep_config_location(
    file: &TrustFile,
    ctx: &Context,
    holder: Holder<'_>,
    configured: &BTreeMap<String, OsString>,
) {
    let environment = env::agent_config_environment(configured);
    if environment.is_empty() {
        return;
    }
    let kept = match holder {
        Holder::Workspace(workspace) => {
            crate::client::workspaces::keep_agent_environment(
                &ctx.paths,
                &workspace.id,
                environment,
            )
            .await
        }
        Holder::Repository(repository) => {
            crate::client::repositories::keep_agent_environment(
                &ctx.paths,
                &repository.id,
                environment,
            )
            .await
        }
    };
    if let Err(error) = kept {
        eprintln!(
            "warning: repository removal may leave {} trust entries behind: {error:#}",
            file.tool()
        );
    }
}

/// Trust the directory of a chat at `home`, which runs with this process's
/// environment changed by `profile`, as workspace launches trust theirs;
/// repository removal clears the entries. The `root_dir` of a chat without
/// a repository stays trusted.
pub(super) async fn chat(
    file: &TrustFile,
    ctx: &Context,
    repository: Option<&Repository>,
    home: ChatHome<'_>,
    profile: &crate::ai::Environment,
) {
    let mut configured = BTreeMap::new();
    profile.layer(&mut configured, &ctx.paths.home);
    if let Some(repository) = repository {
        keep_config_location(file, ctx, Holder::Repository(repository), &configured).await;
    }
    let environment = AgentEnvironment::new(&ctx.paths.home, &configured);
    let paths = match repository {
        Some(repository) => repository_trust_paths(repository, home.path()),
        None => vec![home.path().to_owned()],
    };
    file.trust(&environment, &paths);
}

/// The workspace's repository trust paths, or the workspace alone when its
/// repository cannot be resolved.
async fn workspace_trust_paths(ctx: &Context, workspace: &Workspace, agent: &str) -> Vec<PathBuf> {
    match crate::client::repositories::list(&ctx.paths)
        .await
        .and_then(|repos| {
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
