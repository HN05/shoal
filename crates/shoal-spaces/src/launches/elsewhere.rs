//! Launches on another joined machine, which `shoal start --machine` names.
//! What this machine reads for the launch goes along in a form the other
//! machine reads alike: the parent by its ID, the repository by the URL of
//! its origin, and the base as a branch or commit of that origin, or as a
//! commit this machine carries there. See `docs/design/hub.md`.
use crate::Spaces;
use crate::daemon::Daemon;
use anyhow::{Context as _, Result, bail, ensure};
use shoal_ids::agent_session_ids::AgentSessionId;
use shoal_protocol::config::modes::ModeShapes;
use shoal_protocol::hub::Member;
use shoal_protocol::launches::{Carried, CarriedCopy, Launch, StartTarget};
use shoal_protocol::protocol::{ConfigMethod, LaunchMethod, Launched, Operation};
use shoal_protocol::space_names::SpaceSelector;
use shoal_refs::CommitId;

use crate::{git::clone_base::CloneBase, model::Repository, scope::Caller};
use shoal_core::launches;

impl Spaces {
    /// None when the launch names no machine, or this one, where it starts
    /// here.
    pub(super) async fn launch_on_machine(
        &self,
        daemon: &dyn Daemon,
        launch: &mut Launch,
        caller: Option<&Caller>,
    ) -> Result<Option<Launched>> {
        let Some(machine) = self.machine_elsewhere(daemon, launch).await? else {
            launch.machine = None;
            return Ok(None);
        };
        ensure_carried(launch, &machine.name)?;
        let mut launch = std::mem::take(launch);
        self.forwarded_spaces(daemon, &mut launch, caller).await?;
        let copied = self.copy_as_carried(daemon, &mut launch, caller).await?;
        let carried = self.carried(daemon, &mut launch, &machine, caller).await?;
        let forked = (launch.fork.as_ref()).map(|session| (copied.as_ref(), session));
        if let Some(forked) = forked {
            let origin = carried.origin.as_deref();
            (self.carry_forked(daemon, &launch, forked, &machine, origin, caller)).await?;
        }
        launch.forwarded = Some(carried);
        // That machine starts it as its own.
        launch.machine = None;
        let mut launched = self
            .forward_launch(daemon, &machine, launch, caller)
            .await?;
        launched.machine = Some(machine.name);
        Ok(Some(launched))
    }

    /// Another machine starts a copy from a new branch, the one it names or
    /// else one named after the copied workspace's, at that workspace's HEAD
    /// as its base, with a commit that keeps the files the copy starts with.
    /// Those go there with the base's history. Also returns the copied
    /// workspace, whose agent's session a fork of the copy starts from. A
    /// scoped caller copies only what it copies here.
    async fn copy_as_carried(
        &self,
        daemon: &dyn Daemon,
        launch: &mut Launch,
        caller: Option<&Caller>,
    ) -> Result<Option<SpaceSelector>> {
        let Some(mut copied) = self.launch_copy(daemon, launch, caller).await? else {
            return Ok(None);
        };
        if let Some(caller) = caller {
            daemon
                .coordination()
                .copy_within(daemon, caller, &mut copied.creation.source)
                .await?;
        }
        let (head, files) = self.copy_snapshot(daemon, &copied.creation.source).await?;
        (launch.copy, launch.checkpoint) = (None, None);
        launch.repository = Some(copied.repository.to_string());
        launch.base = Some(head.to_string());
        let name = copied.creation.name;
        launch.carried_copy = Some(CarriedCopy { files, name });
        Ok(Some(copied.creation.source.workspace))
    }

    /// `origin` is the URL of the repository the start goes in, if any. The
    /// session is that of the agent of the `copied` workspace, else of the
    /// space the launch goes below, as a fork here takes it.
    async fn carry_forked(
        &self,
        daemon: &dyn Daemon,
        launch: &Launch,
        (copied, session): (Option<&SpaceSelector>, &AgentSessionId),
        machine: &Member,
        origin: Option<&str>,
        caller: Option<&Caller>,
    ) -> Result<()> {
        let forked = copied.or(launch.parent.as_ref()).context(
            "a start that forks a session goes below the space whose agent it forks, or copies its workspace",
        )?;
        (self.carry_session(daemon, machine, (forked, session), origin, caller)).await
    }

    /// Asks `machine` through the hub. The repository the launch starts in
    /// goes along by its origin's URL and the config it tracks, which that
    /// machine reads where it has none.
    pub(super) async fn modes_on_machine(
        &self,
        daemon: &dyn Daemon,
        machine: &Member,
        launch: &Launch,
        caller: Option<&Caller>,
    ) -> Result<ModeShapes> {
        let mut launch = launch.clone();
        self.forwarded_spaces(daemon, &mut launch, caller).await?;
        let carried = match self.found_repository(daemon, &launch, caller).await? {
            Some(repository) => Carried {
                origin: crate::repository::remote_url_from_source(&repository.source).await?,
                tracked_config: Some(daemon.tracked_config(&repository).await?),
                ..Carried::default()
            },
            None => (self.inherited_carried(daemon, &launch, caller).await?).unwrap_or_default(),
        };
        unname_repository(&mut launch);
        launch.forwarded = Some(carried);
        launch.machine = None;
        let launch = Box::new(launch);
        let operation = Operation::Launch(LaunchMethod::LaunchModes { launch });
        let body = (daemon.ask_machine(machine, operation, caller)).await?;
        ModeShapes::try_from(body)
    }

    /// For `caller`, the space it goes below is its own when it names none.
    /// Waits until the hub lists that space for the other machine to find.
    async fn forwarded_spaces(
        &self,
        daemon: &dyn Daemon,
        launch: &mut Launch,
        caller: Option<&Caller>,
    ) -> Result<()> {
        self.select_launch_spaces(daemon, launch).await?;
        if let Some(caller) = caller {
            daemon
                .coordination()
                .parent_below(daemon, caller, &mut launch.parent)
                .await?;
        }
        self.parent_listed(daemon, launch.parent.as_ref()).await
    }

    /// A space created here just before is listed only once this machine's
    /// report of it settles.
    async fn parent_listed(
        &self,
        daemon: &dyn Daemon,
        parent: Option<&SpaceSelector>,
    ) -> Result<()> {
        let Some(id) = parent.and_then(SpaceSelector::given_id) else {
            return Ok(());
        };
        if self.holds_space(&SpaceSelector::id(id)).await? {
            daemon.listed(id).await;
        }
        Ok(())
    }

    /// None when the launch names none, or this one.
    pub(super) async fn machine_elsewhere(
        &self,
        daemon: &dyn Daemon,
        launch: &Launch,
    ) -> Result<Option<Member>> {
        let Some(named) = &launch.machine else {
            return Ok(None);
        };
        let (machine, own) = daemon.named_machine(named).await?;
        Ok((!own).then_some(machine))
    }

    pub(super) async fn launches_elsewhere(
        &self,
        daemon: &dyn Daemon,
        launch: &Launch,
    ) -> Result<bool> {
        Ok(self.machine_elsewhere(daemon, launch).await?.is_some())
    }

    async fn forward_launch(
        &self,
        daemon: &dyn Daemon,
        machine: &Member,
        launch: Launch,
        caller: Option<&Caller>,
    ) -> Result<Launched> {
        let launch = LaunchMethod::Launch {
            launch: Box::new(launch),
        };
        let body = (daemon.ask_machine(machine, Operation::Launch(launch), caller)).await?;
        Launched::try_from(body)
    }

    /// Names the repository its origin names where this machine registered
    /// it, which it reads without cloning; else the config that repository
    /// tracks, which the modes are read under instead.
    pub(super) async fn read_carried(
        &self,
        daemon: &dyn Daemon,
        launch: &mut Launch,
    ) -> Result<Option<String>> {
        let Some(carried) = launch.forwarded.take() else {
            return Ok(None);
        };
        let Some(origin) = carried.origin else {
            return Ok(None);
        };
        let repositories = daemon.repositories().await?;
        let key = shoal_protocol::repository::url_key(&origin);
        let Some(repository) = crate::repository::with_origin(&repositories, &key).await else {
            return Ok(carried.tracked_config);
        };
        launch.repository = Some(repository.id.to_string());
        Ok(None)
    }

    /// Registers the repository its origin names as a URL clone where this
    /// machine has none, and fetches the origin's branch that holds the
    /// base. The launch then starts there.
    pub(super) async fn take_carried(
        &self,
        daemon: &dyn Daemon,
        launch: &mut Launch,
    ) -> Result<()> {
        let Some(carried) = launch.forwarded.take() else {
            return Ok(());
        };
        ensure!(
            !launch.here,
            "--here runs an agent in the caller's terminal, which another machine has none of"
        );
        if let Some(origin) = carried.origin {
            let repository = (daemon.register_repository(origin.into(), None, None)).await?;
            if let Some(branch) = &carried.base_branch {
                crate::git::clone_base::fetch_branch(&repository.path, branch).await?;
            }
            launch.repository = Some(repository.id.to_string());
        }
        Ok(())
    }

    /// Only where the launch names no repository and starts in one: the
    /// parent runs on another machine in a repository this one has not
    /// registered, so this clones the origin that machine reports. A parent
    /// outside `caller`'s scope is refused before anything is cloned, as is
    /// what the launch would refuse there.
    pub(super) async fn take_parent_repository(
        &self,
        daemon: &dyn Daemon,
        launch: &mut Launch,
        caller: Option<&Caller>,
    ) -> Result<()> {
        let Some(SpaceSelector::Id(parent)) = launch.parent.clone() else {
            return Ok(());
        };
        if !self.inherits_unregistered(daemon, launch).await? {
            return Ok(());
        }
        if let Some(caller) = caller {
            let mut named = launch.parent.clone();
            daemon
                .coordination()
                .parent_below(daemon, caller, &mut named)
                .await?;
        }
        let shape = self.launch_shape(daemon, launch, caller).await?;
        if !shape.repository {
            return Ok(());
        }
        // What the launch would refuse in that repository fails before
        // anything is cloned.
        launches::ensure_fits(launch, &[], shape.kind, Some(&shape.mode))?;
        self.ensure_inherited_agent(daemon, launch, caller).await?;
        let url = daemon.origin_url_elsewhere(&parent).await?;
        let url = url.with_context(|| {
            format!("the machine of space {parent} reports no URL to clone its repository from")
        })?;
        let repository = (daemon.register_repository(url.into(), None, None)).await?;
        launch.repository = Some(repository.id.to_string());
        Ok(())
    }

    /// As the parent's machine reads it for `caller`, where the launch names
    /// no repository and the parent runs on another machine in a repository
    /// this one has not registered.
    pub(super) async fn inherited_tracked_config(
        &self,
        daemon: &dyn Daemon,
        launch: &Launch,
        caller: Option<&Caller>,
    ) -> Result<Option<String>> {
        let Some(parent) = launch.parent.as_ref() else {
            return Ok(None);
        };
        if !self.inherits_unregistered(daemon, launch).await? {
            return Ok(None);
        }
        let Some((machine, listed)) = daemon.machine_of(parent).await? else {
            return Ok(None);
        };
        let space = SpaceSelector::Id(listed.space.id);
        let operation = Operation::Config(ConfigMethod::TrackedConfig { space });
        Ok(Some(String::try_from(
            daemon.ask_machine(&machine, operation, caller).await?,
        )?))
    }

    /// Refuses the agent the launch starts, the one it names or else
    /// `agent.default`, and its model, where the settings read from the
    /// config the parent's repository tracks cannot start them. Runs before
    /// this machine clones the repository.
    async fn ensure_inherited_agent(
        &self,
        daemon: &dyn Daemon,
        launch: &Launch,
        caller: Option<&Caller>,
    ) -> Result<()> {
        let tracked = self
            .inherited_tracked_config(daemon, launch, caller)
            .await?;
        let settings = daemon.tracked_settings(&tracked.unwrap_or_default())?;
        let Some(agent) = (launch.agent.clone()).or_else(|| settings.agent.default.clone()) else {
            return Ok(());
        };
        crate::agents::ensure_available(&agent, &settings)?;
        let model = crate::agents::resolve_model(&settings, &agent, launch.model.clone());
        crate::agents::check_model(&settings, &agent, &model)
    }

    /// The URL of its origin and the config it tracks, which this machine
    /// reads for `caller`: that machine never asks a third one for a caller
    /// it only vouches for.
    async fn inherited_carried(
        &self,
        daemon: &dyn Daemon,
        launch: &Launch,
        caller: Option<&Caller>,
    ) -> Result<Option<Carried>> {
        let Some(tracked) = self
            .inherited_tracked_config(daemon, launch, caller)
            .await?
        else {
            return Ok(None);
        };
        let parent = (launch.parent.as_ref()).and_then(SpaceSelector::given_id);
        let parent = parent.context("a launch names its parent by ID")?;
        let origin = daemon.origin_url_elsewhere(parent).await?;
        let origin = origin.with_context(|| {
            format!("the machine of space {parent} reports no URL to clone its repository from")
        })?;
        Ok(Some(Carried {
            origin: Some(origin),
            tracked_config: Some(tracked),
            ..Carried::default()
        }))
    }

    async fn inherits_unregistered(&self, daemon: &dyn Daemon, launch: &Launch) -> Result<bool> {
        Ok(
            !launches::names_repository(launch)?
                && self.parent_unregistered(daemon, launch).await?,
        )
    }

    /// The URL of the origin of the repository it starts in, which then names
    /// that repository, and its base as that machine fetches it, or with the
    /// commits it carries there. A parent that takes no space of that
    /// repository refuses it first. Nothing for a launch whose mode, as that
    /// machine defines it, starts it without a repository. For one that takes
    /// a repository this machine has not registered, what
    /// [`Self::inherited_carried`] says.
    async fn carried(
        &self,
        daemon: &dyn Daemon,
        launch: &mut Launch,
        machine: &Member,
        caller: Option<&Caller>,
    ) -> Result<Carried> {
        if !self.launch_shape(daemon, launch, caller).await?.repository {
            return Ok(Carried::default());
        }
        let Some(repository) = self.found_repository(daemon, launch, caller).await? else {
            return Ok((self.inherited_carried(daemon, launch, caller).await?).unwrap_or_default());
        };
        // Refuse before anything goes there, as that machine would refuse
        // it.
        let parent = (launch.parent.as_ref()).and_then(SpaceSelector::given_id);
        let selector = (&repository.id).into();
        self.ensure_parent_takes(daemon, Some(&selector), parent)
            .await?;
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
            self.carry_files(daemon, carrying, &copy.files, caller)
                .await?;
        }
        if let Some(base) = &launch.base {
            let (found, carried) = self.clone_base(daemon, carrying, base, caller).await?;
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

    /// `base` as a clone of the origin on `machine` fetches it, or else,
    /// where only this machine has its commits, its commit, which this
    /// machine carries there for `caller` with the history the origin's
    /// default branch lacks. Returns it as carried.
    async fn clone_base(
        &self,
        daemon: &dyn Daemon,
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
        (self.carry_commit(daemon, machine, carrying, carried, caller)).await?;
        let base = CloneBase {
            base: commit.to_string(),
            branch: branch.to_string(),
        };
        Ok((base, Some(commit)))
    }

    /// Carries the history the origin's default branch lacks, as a copy's
    /// files go there.
    async fn carry_files(
        &self,
        daemon: &dyn Daemon,
        (machine, repository, origin): (&Member, &Repository, &str),
        files: &CommitId,
        caller: Option<&Caller>,
    ) -> Result<()> {
        let lookup = crate::git::default_branch::DefaultBranchLookup::Cached;
        let branch = crate::git::default_branch::resolve(&repository.path, lookup).await?;
        let carried = (files, branch.as_str());
        (self.carry_commit(daemon, machine, (repository, origin), carried, caller)).await
    }

    /// The one it names, refused when this machine has not registered it,
    /// else the one an item it names, its parent or its directory belongs
    /// to. None where this machine finds none, as for a parent elsewhere in
    /// a repository it has not registered, which the other machine finds on
    /// its own.
    pub(super) async fn found_repository(
        &self,
        daemon: &dyn Daemon,
        launch: &Launch,
        caller: Option<&Caller>,
    ) -> Result<Option<Repository>> {
        if let Some(named) = named_repository(launch) {
            let repositories = daemon.repositories().await?;
            let directory = launch.directory.as_deref();
            let selector = crate::repository::named_in(named.to_owned(), directory)?;
            let repository = crate::repository::select(&repositories, selector).await?;
            return Ok(Some(repository.clone()));
        }
        let state = self.launch_state(daemon, launch, caller).await?;
        if state.parent == launches::SpaceRepository::Unregistered {
            return Ok(None);
        }
        launches::shape::launch_repository(&state, launch).await
    }

    async fn parent_unregistered(&self, daemon: &dyn Daemon, launch: &Launch) -> Result<bool> {
        let Some(parent) = &launch.parent else {
            return Ok(false);
        };
        let repository = self.parent_repository(daemon, parent).await?;
        Ok(repository == launches::SpaceRepository::Unregistered)
    }
}

/// With `--repo` or as its positional argument.
fn named_repository(launch: &Launch) -> Option<&str> {
    let positional = match &launch.target {
        Some(StartTarget::Repository(named)) => Some(named.as_str()),
        _ => None,
    };
    launch.repository.as_deref().or(positional)
}

/// A URL then names the repository for another machine.
fn unname_repository(launch: &mut Launch) {
    if matches!(launch.target, Some(StartTarget::Repository(_))) {
        launch.target = None;
    }
    launch.repository = None;
}

/// Refuses what `machine` starts the launch without: this terminal, or the
/// existing workspace `--in` names, whose own machine starts its agents.
pub fn ensure_carried(launch: &Launch, machine: &str) -> Result<()> {
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
