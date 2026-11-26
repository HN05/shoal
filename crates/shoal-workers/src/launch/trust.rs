//! The trust entries a launch adds for the directories its agent works in.
use std::path::{Path, PathBuf};

use anyhow::Context as _;

use crate::{
    agents::{AgentHome, HomeRecords, trust::TrustFile},
    client::connection::CommandPaths,
    launch::environment,
    model::{Chat, ChatHome, Repository, Space, Workspace},
};
use shoal_protocol::transcripts::AgentEnvironment;

/// Whose agents ran with a config location: a workspace's, or a chat's in
/// the repository's checkout.
enum Holder<'a> {
    Workspace(&'a Workspace),
    Repository(&'a Repository),
}

/// Trust the workspace's repository directories and `added`, the other
/// working directories its agent gets, for the tool of `file` running with
/// `environment`.
async fn workspace(
    file: &TrustFile,
    paths: &CommandPaths,
    workspace: &Workspace,
    environment: &AgentEnvironment<'_>,
    added: &[PathBuf],
) {
    let paths = workspace_trust_paths(paths, workspace, file, added).await;
    warn(file.trust(environment, &paths));
}

/// [`workspace`] for a tracked agent, which runs with the workspace's
/// `[env]` changed by `profile`, after the daemon keeps where that puts the
/// tool's config for repository removal.
pub(super) async fn tracked(
    file: &TrustFile,
    paths: &CommandPaths,
    workspace: &Workspace,
    profile: &crate::ai::Environment,
    added: &[PathBuf],
) {
    let mut configured =
        environment::agent_environment(paths, &Space::from(workspace.clone())).await;
    profile.layer(&mut configured, &paths.home);
    let environment = AgentEnvironment::new(&paths.home, &configured);
    keep_config_location(file, paths, Holder::Workspace(workspace), &environment).await;
    self::workspace(file, paths, workspace, &environment, added).await;
}

/// Trust the directories of `space`, a workspace or a chat, and `added`,
/// the other working directories its agent gets, for a tracked agent of
/// `file`'s tool running with `profile`, as [`tracked`] and [`chat`] trust
/// each kind's.
pub(super) async fn space(
    file: &TrustFile,
    paths: &CommandPaths,
    space: &HomeRecords,
    profile: &crate::ai::Environment,
    added: &[PathBuf],
) {
    match (space, space.home()) {
        (HomeRecords::Workspace(workspace), _) => {
            tracked(file, paths, workspace, profile, added).await;
        }
        (HomeRecords::Chat { repository, .. }, Ok(AgentHome::Chat { chat, home, .. })) => {
            let chat = (chat, repository.as_ref());
            self::chat(file, paths, chat, home, profile, added).await;
        }
        // A chat without a home runs nowhere to trust.
        (HomeRecords::Chat { .. }, _) => {}
    }
}

/// Have the daemon keep where the config of `file`'s tool is for an agent
/// running with `environment`, inherited variables included, for the
/// removal or move of `holder`'s repository. Failure only warns.
async fn keep_config_location(
    file: &TrustFile,
    paths: &CommandPaths,
    holder: Holder<'_>,
    environment: &AgentEnvironment<'_>,
) {
    let environment = environment.config_variables();
    // The tools' default locations are always cleaned.
    if environment.is_empty() {
        return;
    }
    let kept = match holder {
        Holder::Workspace(workspace) => {
            crate::client::workspaces::keep_agent_environment(paths, &workspace.id, environment)
                .await
        }
        Holder::Repository(repository) => {
            crate::client::repositories::keep_agent_environment(paths, &repository.id, environment)
                .await
        }
    };
    if let Err(error) = kept {
        eprintln!(
            "warning: removing or moving the repository may leave {} trust entries behind: {error:#}",
            file.tool()
        );
    }
}

/// Trust the directory of `chat` of `repository` at `home` and `added`,
/// for an agent that runs with its `[env]` changed by `profile`, as
/// workspace launches trust theirs.
async fn chat(
    file: &TrustFile,
    paths: &CommandPaths,
    (chat, repository): (&Chat, Option<&Repository>),
    home: ChatHome<'_>,
    profile: &crate::ai::Environment,
    added: &[PathBuf],
) {
    let mut configured = environment::agent_environment(paths, &Space::from(chat.clone())).await;
    profile.layer(&mut configured, &paths.home);
    let environment = AgentEnvironment::new(&paths.home, &configured);
    directory(file, paths, &environment, repository, (home.path(), added)).await;
}

/// Trust `directory` and `added`, the other working directories its agent
/// gets, for the tool of `file` running with `environment`, as
/// [`TrustFile::launch_paths`] gives them for `repository`, whose removal or
/// move cleans the entries for its paths. Those outside it stay trusted,
/// such as the `root_dir` of a chat without a repository.
pub(super) async fn directory(
    file: &TrustFile,
    paths: &CommandPaths,
    environment: &AgentEnvironment<'_>,
    repository: Option<&Repository>,
    (directory, added): (&Path, &[PathBuf]),
) {
    if let Some(repository) = repository {
        keep_config_location(file, paths, Holder::Repository(repository), environment).await;
    }
    let paths = file.launch_paths(repository, directory, added);
    warn(file.trust(environment, &paths));
}

/// The trust paths of the workspace's repository and `added` for the tool
/// of `file`, or without the repository, when it cannot be resolved, of the
/// workspace and `added`.
async fn workspace_trust_paths(
    paths: &CommandPaths,
    workspace: &Workspace,
    file: &TrustFile,
    added: &[PathBuf],
) -> Vec<PathBuf> {
    match crate::client::repositories::list(paths)
        .await
        .and_then(|repos| {
            repos
                .into_iter()
                .find(|repo| repo.id == *workspace.repository())
                .context("workspace repository is missing")
        }) {
        Ok(repo) => file.launch_paths(Some(&repo), &workspace.path, added),
        Err(error) => {
            let tool = file.tool();
            eprintln!("warning: could not resolve repository trust paths for {tool}: {error:#}");
            file.launch_paths(None, &workspace.path, added)
        }
    }
}

fn warn(warnings: Vec<String>) {
    for warning in warnings {
        eprintln!("warning: {warning}");
    }
}
