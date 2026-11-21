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
use shoal_ids::agent_session_ids::AgentSessionId;
use shoal_protocol::config::modes::ModeShapes;
use shoal_protocol::hub::Member;
use shoal_protocol::launches::{Carried, CarriedCopy, Launch, StartTarget};
use shoal_protocol::protocol::{LaunchMethod, Launched, Operation, WorkspaceMethod};
use shoal_protocol::space_names::SpaceSelector;
use shoal_refs::CommitId;

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
        let Some(machine) = self.machine_elsewhere(launch).await? else {
            launch.machine = None;
            return Ok(None);
        };
        ensure_carried(launch, &machine.name)?;
        let mut launch = std::mem::take(launch);
        self.forwarded_spaces(&mut launch, caller).await?;
        let copied = self.copy_as_carried(&mut launch, caller).await?;
        let carried = self.carried(&mut launch, &machine, caller).await?;
        let forked = (launch.fork.as_ref()).map(|session| (copied.as_ref(), session));
        if let Some(forked) = forked {
            let origin = carried.origin.as_deref();
            (self.carry_forked(&launch, forked, &machine, origin, caller)).await?;
        }
        launch.forwarded = Some(carried);
        // That machine starts it as its own.
        launch.machine = None;
        let mut launched = self.forward_launch(&machine, launch, caller).await?;
        launched.machine = Some(machine.name);
        Ok(Some(launched))
    }

    /// Turn the copy `launch` asks for `caller` into what another machine
    /// starts it from: a new branch, the one it names or else one named
    /// after the copied workspace's, at that workspace's HEAD as its base,
    /// with a commit that keeps the files the copy starts with, which go
    /// there with the base's history; the copied workspace, if any, whose
    /// agent's session a fork of the copy starts from. A scoped caller
    /// copies only what it copies here.
    async fn copy_as_carried(
        &self,
        launch: &mut Launch,
        caller: Option<&Caller>,
    ) -> Result<Option<SpaceSelector>> {
        let Some(mut copied) = self.launch_copy(launch, caller).await? else {
            return Ok(None);
        };
        if let Some(caller) = caller {
            self.copy_within(caller, &mut copied.creation.source)
                .await?;
        }
        let (head, files) = self.copy_snapshot(&copied.creation.source).await?;
        (launch.copy, launch.checkpoint) = (None, None);
        launch.repository = Some(copied.repository.to_string());
        launch.base = Some(head.to_string());
        let name = copied.creation.name;
        launch.carried_copy = Some(CarriedCopy { files, name });
        Ok(Some(copied.creation.source.workspace))
    }

    /// Carry `session`, which `launch` forks, to `machine` for `caller`'s
    /// start there in the repository whose origin's URL is `origin`, or in
    /// none: the session of the agent of the `copied` workspace, else of
    /// the space it goes below, as a fork here takes it.
    async fn carry_forked(
        &self,
        launch: &Launch,
        (copied, session): (Option<&SpaceSelector>, &AgentSessionId),
        machine: &Member,
        origin: Option<&str>,
        caller: Option<&Caller>,
    ) -> Result<()> {
        let forked = copied.or(launch.parent.as_ref()).context(
            "a start that forks a session goes below the space whose agent it forks, or copies its workspace",
        )?;
        (self.carry_session(machine, (forked, session), origin, caller)).await
    }

    /// The modes `launch` may start in for `caller` as `machine`, which it
    /// starts on, defines them where it would start, through the hub, with
    /// the repository it starts in carried by its origin's URL and the
    /// config it tracks, which that machine reads it by where it has none.
    pub(super) async fn modes_on_machine(
        &self,
        machine: &Member,
        launch: &Launch,
        caller: Option<&Caller>,
    ) -> Result<ModeShapes> {
        let mut launch = launch.clone();
        self.forwarded_spaces(&mut launch, caller).await?;
        let carried = match self.found_repository(&launch, caller).await? {
            Some(repository) => Carried {
                origin: crate::repository::remote_url_from_source(&repository.source).await?,
                tracked_config: Some(self.tracked_config(&repository).await?),
                ..Carried::default()
            },
            None => (self.inherited_carried(&launch, caller).await?).unwrap_or_default(),
        };
        unname_repository(&mut launch);
        launch.forwarded = Some(carried);
        launch.machine = None;
        let launch = Box::new(launch);
        let operation = Operation::Launch(LaunchMethod::LaunchModes { launch });
        let body = (self.hub.ask_machine(self, machine, operation, caller)).await?;
        ModeShapes::try_from(body)
    }

    /// The spaces `launch`, which goes to another machine, names by ID, and
    /// for `caller` the space it goes below, its own when it names none.
    async fn forwarded_spaces(&self, launch: &mut Launch, caller: Option<&Caller>) -> Result<()> {
        self.select_launch_spaces(launch).await?;
        if let Some(caller) = caller {
            self.parent_below(caller, &mut launch.parent).await?;
        }
        Ok(())
    }

    /// The other joined machine `launch` names to start on; none when it
    /// names none, or this one.
    pub(super) async fn machine_elsewhere(&self, launch: &Launch) -> Result<Option<Member>> {
        let Some(named) = &launch.machine else {
            return Ok(None);
        };
        let Named { machine, own } = self.hub.named_machine(self, named).await?;
        Ok((!own).then_some(machine))
    }

    /// Whether `launch` names another joined machine to start on.
    pub(super) async fn launches_elsewhere(&self, launch: &Launch) -> Result<bool> {
        Ok(self.machine_elsewhere(launch).await?.is_some())
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

    /// Name in `launch`, which another joined machine forwarded here to
    /// read its modes, the repository its origin names where this machine
    /// has registered it, which it reads without cloning; else the config
    /// that repository tracks, which the modes are read under instead.
    pub(super) async fn read_carried(&self, launch: &mut Launch) -> Result<Option<String>> {
        let Some(carried) = launch.forwarded.take() else {
            return Ok(None);
        };
        let Some(origin) = carried.origin else {
            return Ok(None);
        };
        let repositories = self.repositories().await?;
        let key = shoal_protocol::repository::url_key(&origin);
        let Some(repository) = crate::repository::with_origin(&repositories, &key).await else {
            return Ok(carried.tracked_config);
        };
        launch.repository = Some(repository.id.to_string());
        Ok(None)
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

    /// Register the repository of the space `launch` goes below, which
    /// another machine runs in a repository this one has not registered,
    /// as a clone of the origin that machine reports, where the launch
    /// names no repository and starts in one; it then starts there. A
    /// parent outside `caller`'s scope is refused before anything is
    /// cloned, as is what the launch would refuse there.
    pub(super) async fn take_parent_repository(
        &self,
        launch: &mut Launch,
        caller: Option<&Caller>,
    ) -> Result<()> {
        let Some(SpaceSelector::Id(parent)) = launch.parent.clone() else {
            return Ok(());
        };
        if !self.inherits_unregistered(launch).await? {
            return Ok(());
        }
        if let Some(caller) = caller {
            let mut named = launch.parent.clone();
            self.parent_below(caller, &mut named).await?;
        }
        let shape = self.launch_shape(launch, caller).await?;
        if !shape.repository {
            return Ok(());
        }
        // What the launch would refuse in that repository fails before
        // anything is cloned.
        launches::ensure_fits(launch, &[], shape.kind, Some(&shape.mode))?;
        self.ensure_inherited_agent(launch, caller).await?;
        let url = self.hub.origin_url_elsewhere(self, &parent).await?;
        let url = url.with_context(|| {
            format!("the machine of space {parent} reports no URL to clone its repository from")
        })?;
        let repository = (self.register_repository(url.into(), None, None)).await?;
        launch.repository = Some(repository.id.to_string());
        Ok(())
    }

    /// The config that the repository of the space `launch` goes below
    /// tracks, as that space's machine reads it for `caller`, where the
    /// launch names no repository and that space runs on another machine
    /// in a repository this one has not registered.
    pub(super) async fn inherited_tracked_config(
        &self,
        launch: &Launch,
        caller: Option<&Caller>,
    ) -> Result<Option<String>> {
        let Some(parent) = launch.parent.as_ref() else {
            return Ok(None);
        };
        if !self.inherits_unregistered(launch).await? {
            return Ok(None);
        }
        let Some((machine, listed)) = self.hub.machine_of(self, parent).await? else {
            return Ok(None);
        };
        let space = SpaceSelector::Id(listed.space.id);
        let operation = Operation::Workspace(WorkspaceMethod::TrackedConfig { space });
        Ok(Some(String::try_from(
            self.hub
                .ask_machine(self, &machine, operation, caller)
                .await?,
        )?))
    }

    /// Refuse the agent `launch` starts, the one it names or else
    /// `agent.default`, and the model it asks of it, where the settings of
    /// the repository it takes from the space it goes below, read from the
    /// config that repository tracks, cannot start them, before this
    /// machine clones it.
    async fn ensure_inherited_agent(&self, launch: &Launch, caller: Option<&Caller>) -> Result<()> {
        let tracked = self.inherited_tracked_config(launch, caller).await?;
        let settings = self.tracked_settings(&tracked.unwrap_or_default())?;
        let Some(agent) = (launch.agent.clone()).or_else(|| settings.agent.default.clone()) else {
            return Ok(());
        };
        crate::agents::ensure_available(&agent, &settings)?;
        let model = crate::agents::resolve_model(&settings, &agent, launch.model.clone());
        crate::agents::check_model(&settings, &agent, &model)
    }

    /// What `launch` carries to another machine of the repository it takes
    /// from the space it goes below, which a third machine runs in a
    /// repository this one has not registered: the URL of its origin and
    /// the config it tracks, which this machine reads for `caller`, since
    /// that machine never asks a third one for a caller it only vouches for.
    async fn inherited_carried(
        &self,
        launch: &Launch,
        caller: Option<&Caller>,
    ) -> Result<Option<Carried>> {
        let Some(tracked) = self.inherited_tracked_config(launch, caller).await? else {
            return Ok(None);
        };
        let parent = (launch.parent.as_ref()).and_then(SpaceSelector::given_id);
        let parent = parent.context("a launch names its parent by ID")?;
        let origin = self.hub.origin_url_elsewhere(self, parent).await?;
        let origin = origin.with_context(|| {
            format!("the machine of space {parent} reports no URL to clone its repository from")
        })?;
        Ok(Some(Carried {
            origin: Some(origin),
            tracked_config: Some(tracked),
            ..Carried::default()
        }))
    }

    /// Whether `launch`, which names no repository, takes that of the space
    /// it goes below, which another machine runs in a repository this one
    /// has not registered.
    async fn inherits_unregistered(&self, launch: &Launch) -> Result<bool> {
        Ok(!launches::names_repository(launch)? && self.parent_unregistered(launch).await?)
    }

    /// What `launch` carries to `machine` for `caller`: the URL of the
    /// origin of the repository it starts in, which then names that
    /// repository in place of how this machine names it, and its base as
    /// that machine fetches it, or with the commits it carries there; a
    /// parent that takes no space of that repository refuses it first.
    /// Nothing for a launch whose mode, as that machine defines it, starts
    /// it without a repository, and for one that takes a repository this
    /// machine has not registered, what [`Self::inherited_carried`] says.
    async fn carried(
        &self,
        launch: &mut Launch,
        machine: &Member,
        caller: Option<&Caller>,
    ) -> Result<Carried> {
        if !self.launch_shape(launch, caller).await?.repository {
            return Ok(Carried::default());
        }
        let Some(repository) = self.found_repository(launch, caller).await? else {
            return Ok((self.inherited_carried(launch, caller).await?).unwrap_or_default());
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
        let carrying = (machine, &repository, origin.as_str());
        // A copy without unsaved files keeps them in its HEAD, its base.
        let unsaved = (launch.carried_copy.as_ref())
            .filter(|copy| launch.base.as_deref() != Some(copy.files.as_str()));
        if let Some(copy) = unsaved {
            self.carry_files(carrying, &copy.files, caller).await?;
        }
        if let Some(base) = &launch.base {
            let (found, carried) = self.clone_base(carrying, base, caller).await?;
            (launch.base, base_branch) = (Some(found.base), Some(found.branch));
            commit = carried;
        }
        unname_repository(launch);
        Ok(Carried {
            origin: Some(origin),
            base_branch,
            commit,
            ..Carried::default()
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

    /// Carry the commit `files` of `repository`, whose origin's URL is
    /// `origin`, to `machine` for `caller`, with the history the origin's
    /// default branch lacks, as a copy's files go there.
    async fn carry_files(
        &self,
        (machine, repository, origin): (&Member, &Repository, &str),
        files: &CommitId,
        caller: Option<&Caller>,
    ) -> Result<()> {
        let lookup = crate::git::default_branch::DefaultBranchLookup::Cached;
        let branch = crate::git::default_branch::resolve(&repository.path, lookup).await?;
        let carried = (files, branch.as_str());
        (self.carry_commit(machine, (repository, origin), carried, caller)).await
    }

    /// The registered repository `launch` would start in for `caller`: the
    /// one it names, refused when this machine has not registered it, else
    /// the one an item it names, its parent or its directory belongs to;
    /// none that this machine finds none for, as for a parent elsewhere in
    /// a repository it has not registered, which the other machine finds on
    /// its own.
    async fn found_repository(
        &self,
        launch: &Launch,
        caller: Option<&Caller>,
    ) -> Result<Option<Repository>> {
        if let Some(named) = named_repository(launch) {
            let repositories = self.repositories().await?;
            let directory = launch.directory.as_deref();
            let selector = crate::repository::named_in(named.to_owned(), directory)?;
            let repository = crate::repository::select(&repositories, selector).await?;
            return Ok(Some(repository.clone()));
        }
        let state = self.launch_state(launch, caller).await?;
        if state.parent == launches::SpaceRepository::Unregistered {
            return Ok(None);
        }
        launches::shape::launch_repository(&state, launch).await
    }

    /// Whether the space `launch` goes below runs on another machine in a
    /// repository this machine has not registered.
    async fn parent_unregistered(&self, launch: &Launch) -> Result<bool> {
        let Some(parent) = &launch.parent else {
            return Ok(false);
        };
        let repository = self.parent_repository(parent).await?;
        Ok(repository == launches::SpaceRepository::Unregistered)
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

/// `launch` naming no repository, which a URL then names for another
/// machine.
fn unname_repository(launch: &mut Launch) {
    if matches!(launch.target, Some(StartTarget::Repository(_))) {
        launch.target = None;
    }
    launch.repository = None;
}

/// Refuse what `launch` asks of this machine that the machine `machine`
/// starts it without: this terminal, or the existing workspace `--in`
/// names, whose own machine starts its agents.
fn ensure_carried(launch: &Launch, machine: &str) -> Result<()> {
    let local = [
        ("--here", launch.here, "it runs the agent in this terminal"),
        (
            "--in",
            launch.in_workspace.is_some(),
            "a workspace's own machine starts its agents",
        ),
    ];
    match local.iter().find(|(_, set, _)| *set) {
        Some((flag, _, why)) => bail!("a start on machine {machine} takes no {flag}: {why}"),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests;
