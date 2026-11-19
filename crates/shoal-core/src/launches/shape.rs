//! The kind of space a launch starts in, and whether it has a repository,
//! from what the launch names and the modes the settings that apply to it
//! define, which the CLI's start and the daemon's launch decide alike.
use anyhow::Result;
use shoal_ids::repository_ids::RepositoryId;
use shoal_protocol::launches::{Launch, Setup, SpaceChoice};
use shoal_protocol::space_names::SpaceSelector;

use super::{LaunchReads, linked_items, positional_repository, workspace::owning_workspace};
use crate::{
    config::{Effective, modes, modes::ModeName},
    model::{Repository, SpaceKind},
    protocol::ConfigTarget,
};

/// The kind of space `launch` creates in `mode`, which must exist unless
/// the launch asks for a chat, and whether `mode` starts it in a
/// repository. A setup starts where [`setup_kind`] says, and the default
/// mode always exists and always starts in a workspace, so neither reads
/// settings. In the existing workspace the launch names, the mode's kind
/// decides whether the agent is the workspace's worker or a chat in its
/// worktree.
pub async fn launch_kind(
    reads: &impl LaunchReads,
    launch: &Launch,
    mode: &ModeName,
) -> Result<(SpaceKind, bool)> {
    if let Some(setup) = launch.setup {
        return Ok(setup_kind(setup));
    }
    if let Some(workspace) = launch_workspace(launch) {
        let kind = kind_in_workspace(reads, launch, workspace, mode).await?;
        return Ok((kind, true));
    }
    if *mode == ModeName::DEFAULT && !launch.chat {
        return Ok((SpaceKind::Workspace, true));
    }
    let settings = launch_settings(reads, launch).await?;
    let found = modes::get(&settings.modes, mode);
    if launch.chat {
        return Ok((SpaceKind::Chat, found.map_or(true, |mode| mode.repository)));
    }
    let found = found?;
    Ok((found.kind, found.repository))
}

/// The kind of space `setup` starts in, and whether it has a repository: a
/// repository's setup works in a workspace on its setup branch, and the
/// global config's in a chat without a repository.
fn setup_kind(setup: Setup) -> (SpaceKind, bool) {
    match setup {
        Setup::Repository => (SpaceKind::Workspace, true),
        Setup::Config => (SpaceKind::Chat, false),
    }
}

/// The role of the agent `launch` starts in `mode` in `workspace`: its
/// worker for a mode that starts in a workspace, else a chat in its
/// worktree, as for a launch that asks for a chat whatever the mode. A mode
/// without a repository is refused.
async fn kind_in_workspace(
    reads: &impl LaunchReads,
    launch: &Launch,
    workspace: &SpaceSelector,
    mode: &ModeName,
) -> Result<SpaceKind> {
    let settings = reads
        .settings(ConfigTarget::Space(workspace.clone()))
        .await?;
    let found = modes::get(&settings.modes, mode)?;
    anyhow::ensure!(
        found.repository,
        "mode {mode} starts a chat without a repository, which --in cannot"
    );
    Ok(match launch.chat {
        true => SpaceKind::Chat,
        false => found.kind,
    })
}

/// The settings whose modes decide the kind of `launch`: those of the
/// workspace it starts in, or that already has the branch it names, which
/// it goes into as `--in` does, else those of the repository
/// [`launch_repository`] finds, else the global config's.
pub async fn launch_settings(reads: &impl LaunchReads, launch: &Launch) -> Result<Effective> {
    if let Some(workspace) = launch_workspace(launch) {
        return reads.settings(ConfigTarget::Space(workspace.clone())).await;
    }
    let target = match launch_repository(reads, launch).await? {
        Some(repository) => match branch_workspace(reads, launch, &repository).await? {
            Some(workspace) => ConfigTarget::Space(SpaceSelector::Id(workspace.id)),
            None => ConfigTarget::Repository(repository.id.into()),
        },
        None => ConfigTarget::Global,
    };
    reads.settings(target).await
}

/// The registered repository `launch` names, else the one an issue or item
/// URL it names belongs to, else that of the space it goes below, which a
/// child shares, else that of the checkout or worktree the start was made
/// in.
pub async fn launch_repository(
    reads: &impl LaunchReads,
    launch: &Launch,
) -> Result<Option<Repository>> {
    let repos = reads.repositories().await?;
    let named = (launch.repository.as_deref()).or(positional_repository(launch));
    let directory = launch.directory.as_deref();
    if let Some(Ok(selector)) =
        named.map(|named| crate::repository::named_in(named.to_owned(), directory))
        && let Ok(repo) = crate::repository::select(&repos, selector).await
    {
        return Ok(Some(repo.clone()));
    }
    for item in linked_items(launch) {
        if let Ok(Some(repo)) = crate::repository::registered_for_item(&repos, item).await {
            return Ok(Some(repo.clone()));
        }
    }
    let id = match parent_repository(reads, launch).await? {
        Some(id) => Some(id),
        None => directory_repository(reads, launch, &repos).await?,
    };
    Ok(id.and_then(|id| repos.into_iter().find(|repo| repo.id == id)))
}

/// The repository of the space `launch` names to go below, which a child
/// shares; none without a parent or for a chat without a repository.
pub async fn parent_repository(
    reads: &impl LaunchReads,
    launch: &Launch,
) -> Result<Option<RepositoryId>> {
    match &launch.parent {
        Some(parent) => reads.space_repository(parent).await,
        None => Ok(None),
    }
}

/// The ID of the repository among `repos` of the checkout or worktree the
/// start of `launch` was made in, or of a workspace there the caller sees.
pub async fn directory_repository(
    reads: &impl LaunchReads,
    launch: &Launch,
    repos: &[Repository],
) -> Result<Option<RepositoryId>> {
    let Some(directory) = &launch.directory else {
        return Ok(None);
    };
    let Some(root) = crate::repository::worktree_root(directory).await? else {
        return Ok(None);
    };
    let workspaces = reads.workspaces().await?;
    Ok(crate::repository::at_root(&root, repos, &workspaces).cloned())
}

/// The workspace of `repo` that already has the branch `launch` names, if
/// any.
async fn branch_workspace(
    reads: &impl LaunchReads,
    launch: &Launch,
    repo: &Repository,
) -> Result<Option<crate::model::Workspace>> {
    let Some(branch) = &launch.branch else {
        return Ok(None);
    };
    owning_workspace(reads, repo, branch).await
}

/// The existing workspace `launch` starts in, once named by ID.
fn launch_workspace(launch: &Launch) -> Option<&SpaceSelector> {
    launch.in_workspace.as_ref().and_then(SpaceChoice::named)
}
