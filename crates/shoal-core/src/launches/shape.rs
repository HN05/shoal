//! The kind of space a launch starts in, and whether it has a repository,
//! from what the launch names and the modes the settings that apply to it
//! define, which the CLI's start and the daemon's launch decide alike from
//! what they read first.
use anyhow::Result;
use shoal_ids::repository_ids::RepositoryId;
use shoal_protocol::launches::{Launch, Setup, SpaceChoice};
use shoal_protocol::space_names::SpaceSelector;

use super::{
    LaunchState, SpaceRepository, linked_items, positional_repository, workspace::owning_workspace,
};
use crate::{
    config::modes::{self, ModeName, ModeShapes},
    model::{Repository, SpaceKind, Workspace},
    protocol::ConfigTarget,
};

/// What decides the kind of space a launch creates in a mode, and whether
/// the mode starts it in a repository.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KindSource<'a> {
    /// The launch alone: a setup starts where [`setup_kind`] says, and the
    /// default mode always exists and always starts in a workspace, so
    /// neither reads settings.
    Decided(SpaceKind, bool),
    /// The modes of the existing workspace the launch names, which
    /// [`kind_in_workspace`] reads.
    Workspace(&'a SpaceSelector),
    /// The modes the launch may start in, as the machine it starts on
    /// defines them, which [`mode_kind`] reads.
    Modes,
}

/// What decides the kind of space `launch` creates in `mode`.
pub fn kind_source<'a>(launch: &'a Launch, mode: &ModeName) -> KindSource<'a> {
    if let Some(setup) = launch.setup {
        let (kind, repository) = setup_kind(setup);
        return KindSource::Decided(kind, repository);
    }
    if let Some(workspace) = launch_workspace(launch) {
        return KindSource::Workspace(workspace);
    }
    match *mode == ModeName::DEFAULT {
        true => KindSource::Decided(SpaceKind::Workspace, true),
        false => KindSource::Modes,
    }
}

/// The kind of space `mode`, which `modes` must define, starts, and
/// whether it starts it in a repository.
pub fn mode_kind(modes: &ModeShapes, mode: &ModeName) -> Result<(SpaceKind, bool)> {
    let found = modes::get(modes, mode)?;
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

/// The role of the agent a launch starts in `mode` in the existing
/// workspace it names, whose settings define `modes`: its worker for a
/// mode that starts in a workspace, else a chat in its worktree. A mode
/// without a repository is refused.
pub fn kind_in_workspace(modes: &ModeShapes, mode: &ModeName) -> Result<SpaceKind> {
    let found = modes::get(modes, mode)?;
    anyhow::ensure!(
        found.repository,
        "mode {mode} starts a chat without a repository, which --in cannot"
    );
    Ok(found.kind)
}

/// The target whose settings' modes decide the kind of `launch`, from
/// what `state` holds: that of [`workspace_target`], else of
/// [`repository_target`] in the repository [`launch_repository`] finds.
pub async fn settings_target(state: &LaunchState, launch: &Launch) -> Result<ConfigTarget> {
    if let Some(target) = workspace_target(launch) {
        return Ok(target);
    }
    let repository = launch_repository(state, launch).await?;
    repository_target(launch, repository, &state.workspaces).await
}

/// The target of the existing workspace `launch` starts in, whose
/// settings' modes decide its kind, if it names one.
pub fn workspace_target(launch: &Launch) -> Option<ConfigTarget> {
    launch_workspace(launch).map(|workspace| ConfigTarget::Space(workspace.clone()))
}

/// The target whose settings' modes decide the kind of `launch`, which
/// names no existing workspace, in `repository`: the one of `workspaces`
/// that already has the branch it names, which it goes into as `--in`
/// does, else `repository`, else the global config. Only a launch that
/// names a branch reads `workspaces`.
pub async fn repository_target(
    launch: &Launch,
    repository: Option<Repository>,
    workspaces: &[Workspace],
) -> Result<ConfigTarget> {
    let Some(repository) = repository else {
        return Ok(ConfigTarget::Global);
    };
    Ok(
        match branch_workspace(workspaces, launch, &repository).await? {
            Some(workspace) => ConfigTarget::Space(SpaceSelector::Id(workspace.id)),
            None => ConfigTarget::Repository(repository.id.into()),
        },
    )
}

/// The registered repository `launch` starts in, from what `state` holds:
/// the one it names or links, else the one it inherits.
pub async fn launch_repository(state: &LaunchState, launch: &Launch) -> Result<Option<Repository>> {
    match named_repository(&state.repositories, launch).await {
        Some(repo) => Ok(Some(repo)),
        None => inherited_repository(state, launch).await,
    }
}

/// The repository among `repos` that `launch` names, else the one an issue
/// or item URL it names belongs to.
pub async fn named_repository(repos: &[Repository], launch: &Launch) -> Option<Repository> {
    let named = (launch.repository.as_deref()).or(positional_repository(launch));
    let directory = launch.directory.as_deref();
    if let Some(Ok(selector)) =
        named.map(|named| crate::repository::named_in(named.to_owned(), directory))
        && let Ok(repo) = crate::repository::select(repos, selector).await
    {
        return Some(repo.clone());
    }
    for item in linked_items(launch) {
        if let Ok(Some(repo)) = crate::repository::registered_for_item(repos, item).await {
            return Some(repo.clone());
        }
    }
    None
}

/// The repository among those `state` holds that `launch`, naming none,
/// inherits: that of the space it goes below, which a child shares, else
/// that of the checkout or worktree the start was made in; none below
/// another machine's space in a repository this machine has not
/// registered, which no other repository stands in for.
pub async fn inherited_repository(
    state: &LaunchState,
    launch: &Launch,
) -> Result<Option<Repository>> {
    let repos = &state.repositories;
    let id = match &state.parent {
        SpaceRepository::Registered(id) => Some(id.clone()),
        SpaceRepository::Unregistered => None,
        SpaceRepository::Without => directory_repository(launch, repos, &state.workspaces).await?,
    };
    Ok(id.and_then(|id| repos.iter().find(|repo| repo.id == id).cloned()))
}

/// The ID of the repository among `repos` of the checkout or worktree the
/// start of `launch` was made in, or of one of `workspaces` there.
pub async fn directory_repository(
    launch: &Launch,
    repos: &[Repository],
    workspaces: &[Workspace],
) -> Result<Option<RepositoryId>> {
    let Some(directory) = &launch.directory else {
        return Ok(None);
    };
    let Some(root) = crate::repository::worktree_root(directory).await? else {
        return Ok(None);
    };
    Ok(crate::repository::at_root(&root, repos, workspaces).cloned())
}

/// The one of `workspaces` in `repo` that already has the branch `launch`
/// names, if any.
async fn branch_workspace(
    workspaces: &[Workspace],
    launch: &Launch,
    repo: &Repository,
) -> Result<Option<Workspace>> {
    let Some(branch) = &launch.branch else {
        return Ok(None);
    };
    owning_workspace(workspaces, repo, branch).await
}

/// The existing workspace `launch` starts in, once named by ID.
fn launch_workspace(launch: &Launch) -> Option<&SpaceSelector> {
    launch.in_workspace.as_ref().and_then(SpaceChoice::named)
}
