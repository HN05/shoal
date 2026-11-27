//! Placing a launch that names no machine on a machine of the hub. A launch
//! stays here when it needs this machine (an agent that runs its own way in
//! the caller, or a space's process that runs it, an existing workspace,
//! one it would reopen, a copy, a fork, a worktree path, a repository
//! setup, blockers, or a repository this machine has not registered or that
//! has no origin, since placement never clones one), and when the hub
//! cannot be asked. See `docs/design/hub.md`.
use crate::Spaces;
use crate::daemon::Daemon;
use anyhow::Result;
use shoal_ids::member_ids::MemberId;
use shoal_protocol::hub::placement::Decision;
use shoal_protocol::launches::Launch;
use shoal_protocol::protocol::Launched;
use shoal_schedule::placement::snapshot::{Lifetime, Request};

use crate::{
    config::{Effective, modes::Weight},
    model::SpaceKind,
    {log, scope::Caller},
};

pub(super) enum Placed {
    Elsewhere(Launched),
    Here(PlacedHere),
}

/// The hub's decision, and why the launch starts here where the hub placed
/// it elsewhere.
#[derive(Default)]
pub(super) struct PlacedHere {
    decision: Option<Decision>,
    warning: Option<String>,
}

impl PlacedHere {
    /// The launch never reached the machine `decision` placed it on, as
    /// `why` says, which the daemon's log records.
    fn instead(decision: Option<Decision>, why: &str) -> Self {
        let name = (decision.as_ref()).map_or_else(String::new, |decision| decision.name.clone());
        let warning = format!(
            "the start never reached machine {name}, where the hub placed it, so it starts on this machine: {why}"
        );
        log!("{warning}");
        Self {
            decision,
            warning: Some(warning),
        }
    }

    pub(super) fn report(self, launched: &mut Launched) {
        launched.placement = self.decision;
        launched.warnings.extend(self.warning);
    }
}

impl Spaces {
    /// Otherwise reports its placement as it starts here. A launch placed on
    /// a machine that is no longer connected, or whose call this machine
    /// never sent, never reached that machine, so it starts here as before
    /// its placement, saying why. Any other failure there fails it, since
    /// that machine may have started it.
    pub(super) async fn launch_where_placed(
        &self,
        daemon: &dyn Daemon,
        launch: &mut Launch,
        caller: Option<&Caller>,
    ) -> Result<Placed> {
        let unplaced = launch.clone();
        let decision = self.place_launch(daemon, launch, caller).await?;
        let placed_elsewhere = unplaced.machine.is_none() && launch.machine.is_some();
        if placed_elsewhere && !self.reaches_named_machine(daemon, launch).await? {
            *launch = unplaced;
            let why = "it is no longer connected";
            return Ok(Placed::Here(PlacedHere::instead(decision, why)));
        }
        match self.launch_on_machine(daemon, launch, caller).await {
            Ok(Some(mut launched)) => {
                launched.placement = decision;
                Ok(Placed::Elsewhere(launched))
            }
            Ok(None) => Ok(Placed::Here(PlacedHere {
                decision,
                warning: None,
            })),
            Err(error) if placed_elsewhere && daemon.never_sent(&error) => {
                *launch = unplaced;
                let why = format!("{error:#}");
                Ok(Placed::Here(PlacedHere::instead(decision, &why)))
            }
            Err(error) => Err(error),
        }
    }

    async fn reaches_named_machine(&self, daemon: &dyn Daemon, launch: &Launch) -> Result<bool> {
        let machine = self.machine_elsewhere(daemon, launch).await?;
        Ok(machine.is_some_and(|machine| machine.connected))
    }

    /// Names the machine in `launch` and returns the hub's decision. None for
    /// a launch that is not placed.
    async fn place_launch(
        &self,
        daemon: &dyn Daemon,
        launch: &mut Launch,
        caller: Option<&Caller>,
    ) -> Result<Option<Decision>> {
        if !placeable(launch, caller) {
            return Ok(None);
        }
        let Some(this) = daemon.this_machine().await? else {
            return Ok(None);
        };
        let placed = async {
            let Some(request) = self
                .placement_request(daemon, launch, caller, &this.id)
                .await?
            else {
                return Ok(None);
            };
            daemon.place_start(request).await
        };
        let decision = match placed.await {
            Ok(decision) => decision,
            Err(error) => {
                log!(
                    "could not place a start among the hub's machines, so it starts here: {error:#}"
                );
                return Ok(None);
            }
        };
        let elsewhere = decision
            .as_ref()
            .filter(|decision| decision.machine != this.id);
        if let Some(decision) = elsewhere {
            launch.machine = Some(decision.machine.to_string());
            // That machine starts the agent detached, and a caller that
            // would have run it attaches to it there.
            launch.here = false;
        }
        Ok(decision)
    }

    /// Made on the machine `this`. None where it starts here whatever the
    /// policy.
    async fn placement_request(
        &self,
        daemon: &dyn Daemon,
        launch: &mut Launch,
        caller: Option<&Caller>,
        this: &MemberId,
    ) -> Result<Option<Request>> {
        self.select_launch_spaces(daemon, launch).await?;
        let shape = self.launch_shape(daemon, launch, caller).await?;
        if shape.kind == SpaceKind::Workspace
            && self.opens_workspace_here(daemon, launch, caller).await
        {
            return Ok(None);
        }
        let placed = self.placed_repository(daemon, launch, caller, shape.repository);
        let Some((origin, settings)) = placed.await? else {
            return Ok(None);
        };
        let Some(agent) = (launch.agent.clone()).or_else(|| settings.agent.default.clone()) else {
            return Ok(None);
        };
        Ok(Some(Request {
            origin,
            base: launch.base.clone(),
            agent: agent.name().to_owned(),
            heavy: heavy(&settings, &shape.mode),
            lifetime: lifetime(launch.disposable),
            mode: shape.mode.to_string(),
            caller: this.clone(),
            parent: self.parent_machine(daemon, launch, caller, this).await?,
        }))
    }

    /// The one on the branch it names, or the one its issue's earlier work
    /// left, which it reopens. One that check refuses stays here too, where
    /// its start says why.
    async fn opens_workspace_here(
        &self,
        daemon: &dyn Daemon,
        launch: &Launch,
        caller: Option<&Caller>,
    ) -> bool {
        let opened = self.check_workspace_creation(daemon, launch, caller).await;
        !matches!(opened, Ok(None))
    }

    /// By the key machines match repositories by, with the settings it starts
    /// under: none for a start without a repository, under the global
    /// config. Nothing for a repository this machine has not registered or
    /// that has no origin, which keeps the start here.
    async fn placed_repository(
        &self,
        daemon: &dyn Daemon,
        launch: &Launch,
        caller: Option<&Caller>,
        repository: bool,
    ) -> Result<Option<(Option<String>, Effective)>> {
        if !repository {
            return Ok(Some((
                None,
                self.config.current().resolve(&Default::default())?,
            )));
        }
        let Some(repository) = self.found_repository(daemon, launch, caller).await? else {
            return Ok(None);
        };
        let url = crate::repository::remote_url_from_source(&repository.source).await?;
        let Some(url) = url else {
            return Ok(None);
        };
        let settings = daemon.repository_settings(&repository.id).await?;
        Ok(Some((
            Some(shoal_protocol::repository::url_key(&url)),
            settings,
        )))
    }

    /// Its own when it names none. `this` machine where it holds that space.
    async fn parent_machine(
        &self,
        daemon: &dyn Daemon,
        launch: &Launch,
        caller: Option<&Caller>,
        this: &MemberId,
    ) -> Result<Option<MemberId>> {
        let mut parent = launch.parent.clone();
        if let Some(caller) = caller {
            daemon
                .coordination()
                .parent_below(daemon, caller, &mut parent)
                .await?;
        }
        let Some(parent) = parent else {
            return Ok(None);
        };
        if self.holds_space(&parent).await? {
            return Ok(Some(this.clone()));
        }
        let elsewhere = daemon.machine_of(&parent).await?;
        Ok(elsewhere.map(|(machine, _)| machine.id))
    }
}

/// It names no machine and needs nothing of this machine's, as blockers,
/// which wait on its spaces and queue entries, do. Nor does a caller that
/// runs its agent here: the user attaches to one on another machine, which
/// a space's process cannot, nor anyone to one that runs its own way.
pub fn placeable(launch: &Launch, caller: Option<&Caller>) -> bool {
    launch.machine.is_none()
        && launch.forwarded.is_none()
        && !(launch.here && (launch.own_way || caller.is_some()))
        && launch.in_workspace.is_none()
        && launch.copy.is_none()
        && launch.carried_copy.is_none()
        && launch.fork.is_none()
        && launch.path.is_none()
        && launch.setup.is_none()
        && launch.blockers.is_empty()
}

/// Disposable for a workspace that `disposable` marks, whose every removal
/// deletes it.
pub fn lifetime(disposable: bool) -> Lifetime {
    match disposable {
        true => Lifetime::Disposable,
        false => Lifetime::Lasting,
    }
}

pub fn heavy(settings: &Effective, mode: &crate::config::modes::ModeName) -> bool {
    (settings.modes.get(mode.as_str())).is_none_or(|mode| mode.weight != Weight::Light)
}

#[cfg(test)]
mod tests;
