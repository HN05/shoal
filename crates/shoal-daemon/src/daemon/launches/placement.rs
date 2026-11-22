//! Placing a launch that names no machine: on a machine joined to a hub,
//! a launch whose agent starts detached goes where the hub's placement
//! policy puts it, which it then names as `--machine` would. One whose
//! agent runs in the caller's terminal stays here, where the user is, as
//! does one that needs this machine: an existing workspace, a copy, a
//! fork, a worktree path, a repository setup, or a repository this machine
//! has not registered or that has no origin, since placement never clones
//! one. A hub that cannot be asked leaves it here too.
use anyhow::Result;
use shoal_ids::member_ids::MemberId;
use shoal_protocol::hub::placement::Decision;
use shoal_protocol::launches::Launch;
use shoal_schedule::placement::snapshot::Request;

use crate::{
    config::{Effective, modes::Weight},
    daemon::{log, manager::Manager, scope::Caller},
};

impl Manager {
    /// Name in `launch` for `caller` the machine the hub places it on, and
    /// return the hub's decision; none for a launch that is not placed.
    pub(super) async fn place_launch(
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
/// none, runs its agent detached, and needs nothing of this machine's.
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
}

/// Whether `mode`'s agent holds a heavy slot under `settings`.
fn heavy(settings: &Effective, mode: &crate::config::modes::ModeName) -> bool {
    (settings.modes.get(mode.as_str())).is_none_or(|mode| mode.weight != Weight::Light)
}

#[cfg(test)]
mod tests;
