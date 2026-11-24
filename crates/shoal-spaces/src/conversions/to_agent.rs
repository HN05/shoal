//! Handing a space's work over to another agent tool: the refusals, then
//! the switch, at once for a stopped or asleep agent, else once the
//! running agent's turn ends.
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
    /// Hand the work of the space `selector` names over to the agent tool
    /// `change` names, in `mode`, else the space's own, for `caller`: once
    /// the turn of its running agent ends, or at once for a stopped or
    /// asleep agent, or a space without one. Its own tool is refused.
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

    /// Refuse `taking` in the space `id` to the scoped `caller` where its
    /// mode and the permissions given to it exceed what the caller's own
    /// space holds.
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

    /// Refuse the scoped `caller` how the agent of the space `id` carries
    /// on in a conversion where it gives more than the caller's own space
    /// holds: `taking`'s mode and permissions when another tool takes over,
    /// else the permissions `change` makes to those the agent was given.
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

    /// Hand the work of `space` over to `taking`: once the turn of its
    /// running agent ends, which the conversion records, else at once, in
    /// place of its stopped or asleep agent. Whether it went over at once.
    async fn switch_agent(
        &self,
        daemon: &dyn Daemon,
        space: &Space,
        taking: Taking,
    ) -> Result<bool> {
        let admissions = daemon.lock_agent_admissions().await;
        if let Some(execution) = self.running_agent(daemon, &space.id).await? {
            let id = space.id.clone();
            (self.store)
                .write(move |tx| super::record::record_agent_switch(tx, &id, execution, taking))
                .await?;
            drop(admissions);
            // The relocation waits for the agent's turn to end, which it may
            // have already.
            self.signals.agent_changed();
            return Ok(false);
        }
        drop(admissions);
        daemon.ensure_replaceable(space).await?;
        let leaving = crate::spaces::sessions::newest_saved(&self.paths, &space.id)?;
        let handoff = self.hand_off(daemon, &space.id, leaving, taking, false);
        self.conversion_handovers.hold(handoff).await?;
        Ok(true)
    }

    /// The execution that runs the agent of the space `id`, if one runs.
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

/// The agent `agent` that takes the work of a space over in `mode` as
/// `change` asks, refused as a start under `settings` in a space of the kind
/// `place` says refuses it, with the permissions `change` gives it over
/// `narrowing`, those of the space's agent.
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

/// The agent tool that takes the work of a space, whose agent is `current`
/// and was given `narrowing`, over in `mode` as `change` asks, in a
/// conversion to a space of the kind `place` says under its settings; none
/// when `change` keeps the space's tool, which continues from a fork of its
/// session and takes no note.
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

/// The mode a space of `kind` whose agent recorded none starts in.
fn default_mode(kind: SpaceKind) -> ModeName {
    match kind {
        SpaceKind::Workspace => ModeName::DEFAULT,
        SpaceKind::Chat => crate::model::chat_mode(),
    }
}
