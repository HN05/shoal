//! Handing a space's work over to another agent tool: at once for a stopped
//! or asleep agent, else once the running agent's turn ends.
use crate::Spaces;
use crate::daemon::Daemon;
use anyhow::{Context as _, Result, ensure};
use shoal_ids::{execution_ids::ExecutionId, space_ids::SpaceId};

use super::handoff::Taking;
use crate::{
    agent::Agent,
    config::{Effective, modes::ModeName, permissions::PermissionsConfig},
    model::{Space, SpaceKind},
    protocol::{AgentChange, AgentSwitch, operations::QueuedLaunch},
    scope::Caller,
};
use shoal_protocol::space_names::SpaceSelector;

impl Spaces {
    /// In `mode`, else the space's own. Refuses the space's own tool.
    pub async fn convert_agent(
        &self,
        daemon: &dyn Daemon,
        selector: &SpaceSelector,
        mode: Option<ModeName>,
        change: AgentChange,
        caller: Option<&Caller>,
    ) -> Result<AgentSwitch> {
        let space = self.space(selector).await?;
        self.ensure_not_converting(&space.id, &space.name).await?;
        ensure!(
            !self.space_removals.reserved(&space.id),
            "{} is being removed",
            space.name
        );
        let to = (change.agent.clone())
            .context("name the agent tool that takes the work over")?
            .normalized();
        let from = self.agent_of(&space.id).await?;
        ensure!(
            from.as_ref() != Some(&to),
            "the agent of {} is {to} already",
            space.name
        );
        let mode = mode.or(space.mode.clone());
        let mode = mode.unwrap_or_else(|| default_mode(space.kind()));
        let settings = (daemon.space_settings_with_templates(&space, None)).await?;
        let place = (space.kind(), space.repository_id.is_some());
        let taking = taking(
            space.narrowing,
            (&settings, place),
            to.clone(),
            mode,
            change,
        )?;
        self.ensure_taking_held(daemon, &space.id, &taking, caller)
            .await?;
        let at_once = self.switch_agent(daemon, &space, taking).await?;
        Ok(AgentSwitch {
            space,
            from,
            to,
            at_once,
        })
    }

    /// Refuses where its mode and the permissions given to it exceed what
    /// the caller's own space holds.
    pub(super) async fn ensure_taking_held(
        &self,
        daemon: &dyn Daemon,
        id: &SpaceId,
        taking: &Taking,
        caller: Option<&Caller>,
    ) -> Result<()> {
        let selector = SpaceSelector::id(id);
        (daemon.coordination().ensure_agent_held(
            daemon,
            caller,
            &selector,
            &taking.mode,
            &taking.narrowing,
        ))
        .await
    }

    /// Refuses where it gives more than the caller's own space holds:
    /// `taking`'s mode and permissions when another tool takes over, else
    /// the permissions `change` makes to those the agent was given.
    pub(super) async fn ensure_carrying_held(
        &self,
        daemon: &dyn Daemon,
        id: &SpaceId,
        (taking, change): (Option<&Taking>, &AgentChange),
        caller: Option<&Caller>,
    ) -> Result<()> {
        if let Some(taking) = taking {
            return self.ensure_taking_held(daemon, id, taking, caller).await;
        }
        if change.permissions.is_empty() {
            return Ok(());
        }
        let space = self.space(&SpaceSelector::id(id)).await?;
        let repository = space.repository_id.clone();
        (daemon.coordination().ensure_change_held(
            daemon,
            caller,
            &space,
            &change.permissions,
            repository.as_ref(),
        ))
        .await
    }

    /// Returns whether it went over at once. The hand-overs are held
    /// throughout, so the relocation, which takes a recorded switch over
    /// from the end of a turn, never runs it too.
    async fn switch_agent(
        &self,
        daemon: &dyn Daemon,
        space: &Space,
        taking: Taking,
    ) -> Result<bool> {
        let handovers = &self.conversion_handovers;
        handovers
            .hold(self.switch_held(daemon, space, taking))
            .await
    }

    /// Call once the hand-overs are held. The switch of a stopped or asleep
    /// agent is recorded first, so no agent starts there while its sessions
    /// are copied. A switch that fails forgets it.
    async fn switch_held(
        &self,
        daemon: &dyn Daemon,
        space: &Space,
        taking: Taking,
    ) -> Result<bool> {
        let id = space.id.clone();
        let admissions = daemon.lock_agent_admissions().await;
        if let Some(execution) = self.running_agent(daemon, &id).await? {
            (self.store)
                .write(move |tx| super::pending::record_agent_switch(tx, &id, execution, taking))
                .await?;
            drop(admissions);
            // The relocation waits for the agent's turn to end, which it may
            // have already.
            self.signals.agent_changed();
            return Ok(false);
        }
        // Not one that waits in the agent queue.
        daemon.ensure_replaceable(space).await?;
        let saved = crate::spaces::sessions::newest_saved(&self.paths, &id)?;
        let execution = (saved.map(|saved| saved.execution)).unwrap_or_else(ExecutionId::generate);
        let (space, recorded, switching) = (id.clone(), execution.clone(), taking.clone());
        (self.store)
            .write(move |tx| super::pending::record_stopped_switch(tx, &space, recorded, switching))
            .await?;
        drop(admissions);
        // An attach follows the agent across the switch until its restore
        // runs.
        let _moving = self.relocating.mark(&id);
        let switched = (self.switch_after_turn(daemon, &id, &execution, taking, false)).await;
        if switched.is_err() {
            let space = id.clone();
            (self.store)
                .write(move |tx| super::pending::forget(tx, &space))
                .await?;
        }
        switched.map(|()| true)
    }

    pub(super) async fn running_agent(
        &self,
        daemon: &dyn Daemon,
        id: &SpaceId,
    ) -> Result<Option<ExecutionId>> {
        Ok((daemon.running_agents().await?.into_iter())
            .find(|agent| agent.space_id == *id && agent.agent.is_some())
            .map(|agent| agent.execution))
    }
}

/// Refused as a start under `settings` in a space of the kind `place` says
/// refuses it. Takes the permissions `change` gives over `narrowing`, those
/// of the space's agent.
pub(super) fn taking(
    narrowing: PermissionsConfig,
    (settings, place): (&Effective, (SpaceKind, bool)),
    agent: Agent,
    mode: ModeName,
    change: AgentChange,
) -> Result<Taking> {
    super::ensure_mode(settings, &mode)?;
    let mut launch = QueuedLaunch::new(agent.clone());
    launch.mode = Some(mode.clone());
    launch.model = change.model.clone();
    crate::agents::check_queued_launch(&launch, settings, place)?;
    Ok(Taking {
        agent,
        mode,
        model: change.model,
        narrowing: change.permissions.applied_to(narrowing),
        note: change.note,
    })
}

/// None when `change` keeps the space's tool, which continues from a fork of
/// its session and takes no note.
pub(super) fn switching(
    narrowing: PermissionsConfig,
    place: (&Effective, (SpaceKind, bool)),
    mode: &ModeName,
    change: &AgentChange,
    current: Option<&Agent>,
) -> Result<Option<Taking>> {
    let agent =
        (change.agent.as_ref().map(Agent::normalized)).filter(|agent| Some(agent) != current);
    let Some(agent) = agent else {
        ensure!(
            change.note.is_none(),
            "a note goes to another agent tool taking the work over; the agent continuing its own session takes none"
        );
        return Ok(None);
    };
    taking(narrowing, place, agent, mode.clone(), change.clone()).map(Some)
}

fn default_mode(kind: SpaceKind) -> ModeName {
    match kind {
        SpaceKind::Workspace => ModeName::DEFAULT,
        SpaceKind::Chat => crate::model::chat_mode(),
    }
}
