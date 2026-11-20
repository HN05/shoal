//! Launches on another joined machine, which `shoal start --machine` names:
//! this machine forwards the launch through the hub to that machine's
//! daemon, which starts it as its own caller's, so the new space's queue,
//! admission, cleanup and leases are that machine's. What this machine
//! reads for the launch goes along in a form the other machine reads
//! alike: the space the new one goes below by its ID, the scoped caller's
//! own when it names none, the repository by the URL of its origin, which
//! that machine clones where it has none of it, and the base as a branch
//! or commit of that origin, or else as the commit this machine carries
//! there, with the history only it has.
use anyhow::{Context as _, Result, bail, ensure};
use shoal_protocol::hub::Member;
use shoal_protocol::launches::{Carried, Launch, StartTarget};
use shoal_protocol::protocol::{LaunchMethod, Launched, Operation};
use shoal_protocol::space_names::SpaceSelector;
use shoal_refs::CommitId;

use super::ManagerReads;
use crate::{
    daemon::{hub::Named, manager::Manager, scope::Caller},
    git::clone_base::CloneBase,
    launches,
    model::Repository,
};

impl Manager {
    /// Start `launch` for `caller` on the other joined machine it names;
    /// none when it names no machine, or this one, where it starts here.
    pub(super) async fn launch_on_machine(
        &self,
        launch: &mut Launch,
        caller: Option<&Caller>,
    ) -> Result<Option<Launched>> {
        let Some(named) = launch.machine.take() else {
            return Ok(None);
        };
        let Named { machine, own } = self.hub.named_machine(self, &named).await?;
        if own {
            return Ok(None);
        }
        ensure_carried(launch, &machine.name)?;
        let mut launch = std::mem::take(launch);
        self.select_launch_spaces(&mut launch).await?;
        if let Some(caller) = caller {
            self.parent_below(caller, &mut launch.parent).await?;
        }
        launch.forwarded = Some(self.carried(&mut launch, &machine, caller).await?);
        self.forward_launch(&machine, launch, caller)
            .await
            .map(Some)
    }

    /// Whether `launch` names another joined machine to start on.
    pub(super) async fn launches_elsewhere(&self, launch: &Launch) -> Result<bool> {
        match &launch.machine {
            Some(named) => Ok(!self.hub.named_machine(self, named).await?.own),
            None => Ok(false),
        }
    }

    /// What `machine` answers `launch` from `caller` with, through the hub.
    async fn forward_launch(
        &self,
        machine: &Member,
        launch: Launch,
        caller: Option<&Caller>,
    ) -> Result<Launched> {
        let launch = LaunchMethod::Launch {
            launch: Box::new(launch),
        };
        let body = (self
            .hub
            .ask_machine(self, machine, Operation::Launch(launch), caller))
        .await?;
        Launched::try_from(body)
    }

    /// Take what `launch`, which another joined machine forwarded here,
    /// carries: the repository its origin names, registered as a URL clone
    /// where this machine has none of it, which the launch then starts in,
    /// with the origin's branch that holds its base fetched.
    pub(super) async fn take_carried(&self, launch: &mut Launch) -> Result<()> {
        let Some(carried) = launch.forwarded.take() else {
            return Ok(());
        };
        ensure!(
            !launch.here,
            "--here runs an agent in the caller's terminal, which another machine has none of"
        );
        if let Some(origin) = carried.origin {
            let repository = (self.register_repository(origin.into(), None, None)).await?;
            if let Some(branch) = &carried.base_branch {
                crate::git::clone_base::fetch_branch(&repository.path, branch).await?;
            }
            launch.repository = Some(repository.id.to_string());
        }
        Ok(())
    }

    /// What `launch` carries to `machine` for `caller`: the URL of the
    /// origin of the repository it starts in, which then names that
    /// repository in place of how this machine names it, and its base as
    /// that machine fetches it, or with the commits it carries there; a
    /// parent that takes no space of that repository refuses it first.
    async fn carried(
        &self,
        launch: &mut Launch,
        machine: &Member,
        caller: Option<&Caller>,
    ) -> Result<Carried> {
        let Some(repository) = self.launch_repository(launch, caller).await? else {
            return Ok(Carried::default());
        };
        // Refused before anything goes there, as that machine would refuse it.
        let parent = (launch.parent.as_ref()).and_then(SpaceSelector::given_id);
        let selector = (&repository.id).into();
        self.ensure_parent_takes(Some(&selector), parent).await?;
        let origin = crate::repository::remote_url_from_source(&repository.source).await?;
        let origin = origin.with_context(|| {
            format!(
                "repository {} has no origin that machine {} could clone",
                repository.display_name(),
                machine.name
            )
        })?;
        let (mut base_branch, mut commit) = (None, None);
        if let Some(base) = &launch.base {
            let carrying = (machine, &repository, origin.as_str());
            let (found, carried) = self.clone_base(carrying, base, caller).await?;
            (launch.base, base_branch) = (Some(found.base), Some(found.branch));
            commit = carried;
        }
        if matches!(launch.target, Some(StartTarget::Repository(_))) {
            launch.target = None;
        }
        launch.repository = None;
        Ok(Carried {
            origin: Some(origin),
            base_branch,
            commit,
        })
    }

    /// `base`, a ref of `repository`, whose origin's URL is `origin`, as a
    /// clone of that origin on `machine` fetches it, or else, where only
    /// this machine has its commits, its commit, which this machine
    /// carries there for `caller` with the history the origin's default
    /// branch lacks, and which it returns as carried.
    async fn clone_base(
        &self,
        (machine, repository, origin): (&Member, &Repository, &str),
        base: &str,
        caller: Option<&Caller>,
    ) -> Result<(CloneBase, Option<CommitId>)> {
        let path = &repository.path;
        if let Some(base) = crate::git::clone_base::for_clone(path, base).await? {
            return Ok((base, None));
        }
        let commit = crate::git::clone_base::carried_commit(path, base).await?;
        let lookup = crate::git::default_branch::DefaultBranchLookup::Cached;
        let branch = crate::git::default_branch::resolve(path, lookup).await?;
        let carrying = (repository, origin);
        let carried = (&commit, branch.as_str());
        (self.carry_commit(machine, carrying, carried, caller)).await?;
        let base = CloneBase {
            base: commit.to_string(),
            branch: branch.to_string(),
        };
        Ok((base, Some(commit)))
    }

    /// The registered repository `launch` starts in for `caller`: the one
    /// it names, refused when this machine has not registered it, else the
    /// one an item it names, its parent or its directory belongs to; none
    /// for one whose mode starts it without one, or that this machine
    /// finds none for, as for a parent elsewhere in a repository it has
    /// not registered, which the other machine finds on its own.
    async fn launch_repository(
        &self,
        launch: &Launch,
        caller: Option<&Caller>,
    ) -> Result<Option<Repository>> {
        if !self.launch_shape(launch, caller).await?.repository {
            return Ok(None);
        }
        if let Some(named) = named_repository(launch) {
            let repositories = self.repositories().await?;
            let directory = launch.directory.as_deref();
            let selector = crate::repository::named_in(named.to_owned(), directory)?;
            let repository = crate::repository::select(&repositories, selector).await?;
            return Ok(Some(repository.clone()));
        }
        if self.parent_unregistered(launch).await? {
            return Ok(None);
        }
        let reads = ManagerReads::new(self, caller);
        launches::shape::launch_repository(&reads, launch).await
    }

    /// Whether the space `launch` goes below runs on another machine in a
    /// repository this machine has not registered.
    async fn parent_unregistered(&self, launch: &Launch) -> Result<bool> {
        let Some(SpaceSelector::Id(parent)) = &launch.parent else {
            return Ok(false);
        };
        if self.find_space(parent).await?.is_some() {
            return Ok(false);
        }
        self.hub.unregistered_elsewhere(self, parent).await
    }
}

/// The repository `launch` names, with `--repo` or as its positional
/// argument.
fn named_repository(launch: &Launch) -> Option<&str> {
    let positional = match &launch.target {
        Some(StartTarget::Repository(named)) => Some(named.as_str()),
        _ => None,
    };
    launch.repository.as_deref().or(positional)
}

/// Refuse what `launch` asks of this machine that the machine `machine`
/// starts it without: this terminal, files and agent sessions, or the
/// existing workspace `--in` names, whose own machine starts its agents.
fn ensure_carried(launch: &Launch, machine: &str) -> Result<()> {
    let local = [
        ("--here", launch.here, "it runs the agent in this terminal"),
        (
            "--in",
            launch.in_workspace.is_some(),
            "a workspace's own machine starts its agents",
        ),
        (
            "--copy",
            launch.copy.is_some() || launch.checkpoint.is_some(),
            "it copies this machine's files",
        ),
        (
            "--fork",
            launch.fork.is_some(),
            "it forks a session this machine keeps",
        ),
    ];
    match local.iter().find(|(_, set, _)| *set) {
        Some((flag, _, why)) => bail!("a start on machine {machine} takes no {flag}: {why}"),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests;
