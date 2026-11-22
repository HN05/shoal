//! Placing a launch that names no machine: on a machine joined to a hub,
//! a launch whose agent starts detached goes where the hub's placement
//! policy puts it, which it then names as `--machine` would. One whose
//! agent runs in the caller's terminal stays here, where the user is, as
//! does one that needs this machine: an existing workspace, one it would
//! reopen on its branch or issue, a copy, a fork, a worktree path, a
//! repository setup, blockers, which are its spaces and queue entries, or
//! a repository this machine has not registered or that has no origin,
//! since placement never clones one. A hub that cannot be asked leaves it
//! here too.
use anyhow::Result;
use shoal_hub::session::Unsent;
use shoal_ids::member_ids::MemberId;
use shoal_protocol::hub::placement::Decision;
use shoal_protocol::launches::Launch;
use shoal_protocol::protocol::Launched;
use shoal_schedule::placement::snapshot::Request;

use crate::{
    config::{Effective, modes::Weight},
    daemon::{log, manager::Manager, scope::Caller},
    model::SpaceKind,
};

/// Where a launch went once placed.
pub(super) enum Placed {
    /// Another machine started it.
    Elsewhere(Launched),
    /// It starts here.
    Here(PlacedHere),
}

/// What a launch that starts here reports of its placement: the hub's
/// decision, and why it starts here where the hub placed it elsewhere.
#[derive(Default)]
pub(super) struct PlacedHere {
    decision: Option<Decision>,
    warning: Option<String>,
}

impl PlacedHere {
    /// What a launch reports that starts here instead of on the machine
    /// `decision` placed it on, which it never reached, as `why` says, and
    /// the daemon's log records.
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

    /// Give `launched`, which started here, what it reports of its
    /// placement.
    pub(super) fn report(self, launched: &mut Launched) {
        launched.placement = self.decision;
        launched.warnings.extend(self.warning);
    }
}

impl Manager {
    /// Start `launch` for `caller` on the other machine it names, or the
    /// hub places it on, with the hub's decision; else what it reports of
    /// its placement as it starts here. A launch placed on a machine that
    /// is no longer connected, or whose call this machine never sent,
    /// never reached that machine, so `launch` is as before its placement
    /// and starts here, saying why; any other failure there fails it,
    /// since that machine may have started it.
    pub(super) async fn launch_where_placed(
        &self,
        launch: &mut Launch,
        caller: Option<&Caller>,
    ) -> Result<Placed> {
        let unplaced = launch.clone();
        let decision = self.place_launch(launch, caller).await?;
        let placed_elsewhere = unplaced.machine.is_none() && launch.machine.is_some();
        if placed_elsewhere && !self.reaches_named_machine(launch).await? {
            *launch = unplaced;
            let why = "it is no longer connected";
            return Ok(Placed::Here(PlacedHere::instead(decision, why)));
        }
        match self.launch_on_machine(launch, caller).await {
            Ok(Some(mut launched)) => {
                launched.placement = decision;
                Ok(Placed::Elsewhere(launched))
            }
            Ok(None) => Ok(Placed::Here(PlacedHere {
                decision,
                warning: None,
            })),
            Err(error) if placed_elsewhere && error.is::<Unsent>() => {
                *launch = unplaced;
                let why = format!("{error:#}");
                Ok(Placed::Here(PlacedHere::instead(decision, &why)))
            }
            Err(error) => Err(error),
        }
    }

    /// Whether the other machine `launch` names is connected now.
    async fn reaches_named_machine(&self, launch: &Launch) -> Result<bool> {
        let machine = self.machine_elsewhere(launch).await?;
        Ok(machine.is_some_and(|machine| machine.connected))
    }

    /// Name in `launch` for `caller` the machine the hub places it on, and
    /// return the hub's decision; none for a launch that is not placed.
    async fn place_launch(
        &self,
        launch: &mut Launch,
        caller: Option<&Caller>,
    ) -> Result<Option<Decision>> {
        if !placeable(launch) {
            return Ok(None);
        }
        let Some(this) = self.hub.this_machine(self).await? else {
            return Ok(None);
        };
        let placed = async {
            let Some(request) = self.placement_request(launch, caller, &this.id).await? else {
                return Ok(None);
            };
            self.hub.place(self, request).await
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
        }
        Ok(decision)
    }

    /// What the hub places `launch` for `caller`, made on the machine
    /// `this`, by; none where it starts here whatever the policy.
    async fn placement_request(
        &self,
        launch: &mut Launch,
        caller: Option<&Caller>,
        this: &MemberId,
    ) -> Result<Option<Request>> {
        self.select_launch_spaces(launch).await?;
        let shape = self.launch_shape(launch, caller).await?;
        if shape.kind == SpaceKind::Workspace && self.opens_workspace_here(launch, caller).await {
            return Ok(None);
        }
        let placed = self.placed_repository(launch, caller, shape.repository);
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
            mode: shape.mode.to_string(),
            caller: this.clone(),
            parent: self.parent_machine(launch, caller, this).await?,
        }))
    }

    /// Whether `launch` goes into a workspace this machine has for
    /// `caller`: the one on the branch it names, or the one its issue's
    /// earlier work left, which it reopens. One that check refuses stays
    /// here too, where its start says why.
    async fn opens_workspace_here(&self, launch: &Launch, caller: Option<&Caller>) -> bool {
        let opened = self.check_workspace_creation(launch, caller).await;
        !matches!(opened, Ok(None))
    }

    /// The origin of the repository `launch` starts in for `caller`, by the
    /// key machines match repositories by, with the settings it starts
    /// under: none for a start without a `repository`, under the global
    /// config. Nothing for a repository this machine has not registered or
    /// that has no origin, which keeps the start here.
    async fn placed_repository(
        &self,
        launch: &Launch,
        caller: Option<&Caller>,
        repository: bool,
    ) -> Result<Option<(Option<String>, Effective)>> {
        if !repository {
            return Ok(Some((None, self.config().resolve(&Default::default())?)));
        }
        let Some(repository) = self.found_repository(launch, caller).await? else {
            return Ok(None);
        };
        let url = crate::repository::remote_url_from_source(&repository.source).await?;
        let Some(url) = url else {
            return Ok(None);
        };
        let settings = self
            .repositories
            .repository_settings(&repository.id)
            .await?;
        Ok(Some((
            Some(shoal_protocol::repository::url_key(&url)),
            settings,
        )))
    }

    /// The machine of the space `launch` goes below for `caller`, its own
    /// when it names none: `this` machine where it holds that space.
    async fn parent_machine(
        &self,
        launch: &Launch,
        caller: Option<&Caller>,
        this: &MemberId,
    ) -> Result<Option<MemberId>> {
        let mut parent = launch.parent.clone();
        if let Some(caller) = caller {
            self.parent_below(caller, &mut parent).await?;
        }
        let Some(parent) = parent else {
            return Ok(None);
        };
        if self.holds_space(&parent).await? {
            return Ok(Some(this.clone()));
        }
        let elsewhere = self.hub.machine_of(self, &parent).await?;
        Ok(elsewhere.map(|(machine, _)| machine.id))
    }
}

/// Whether `launch` may go to another machine than its caller's: it names
/// none, runs its agent detached, and needs nothing of this machine's, as
/// blockers, which wait on its spaces and queue entries, do.
fn placeable(launch: &Launch) -> bool {
    launch.machine.is_none()
        && launch.forwarded.is_none()
        && !launch.here
        && launch.in_workspace.is_none()
        && launch.copy.is_none()
        && launch.carried_copy.is_none()
        && launch.fork.is_none()
        && launch.path.is_none()
        && launch.setup.is_none()
        && launch.blockers.is_empty()
}

/// Whether `mode`'s agent holds a heavy slot under `settings`.
pub(crate) fn heavy(settings: &Effective, mode: &crate::config::modes::ModeName) -> bool {
    (settings.modes.get(mode.as_str())).is_none_or(|mode| mode.weight != Weight::Light)
}

#[cfg(test)]
mod tests;
