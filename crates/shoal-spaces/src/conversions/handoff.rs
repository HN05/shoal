//! Handing a space's work from one agent tool to another through copies of
//! its sessions in its attached files. See `docs/design/spaces.md`.
use crate::Spaces;
use crate::daemon::Daemon;
use shoal_ids::{
    agent_session_ids::AgentSessionId, execution_ids::ExecutionId, space_ids::SpaceId,
};

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::{
    agent::{Agent, ModelChoice},
    config::{modes::ModeName, permissions::PermissionsConfig},
    execution::recovery::{HandoffBrief, LastSession, LastSessionExt as _, StagedLastSession},
    sessions::Origin,
    stops::StopRequest,
};
use shoal_protocol::space_names::SpaceSelector;
use shoal_protocol::transcripts::HandedOverSession;

mod brief;
mod copies;

pub use copies::Listed;

/// Its tool, the session it reported last, the execution it ran as and
/// where it ran.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Leaving {
    pub agent: Agent,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<AgentSessionId>,
    pub execution: ExecutionId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<Origin>,
}

/// Its tool and mode, the model asked of it, the permissions given to it and
/// the note it is told after its mode's instructions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Taking {
    pub agent: Agent,
    pub mode: ModeName,
    #[serde(default)]
    pub model: ModelChoice,
    #[serde(default)]
    pub narrowing: PermissionsConfig,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl Spaces {
    /// The recorded conversion keeps any agent from starting there
    /// meanwhile. Copies the agents' sessions into the attached files, saves
    /// `taking` in place of `leaving`, if any, and forgets the conversion,
    /// then starts `taking` unless `held` keeps it for `shoal resume`.
    pub async fn hand_off(
        &self,
        daemon: &dyn Daemon,
        id: &SpaceId,
        leaving: Option<Leaving>,
        taking: Taking,
        held: bool,
    ) -> Result<()> {
        let selector = SpaceSelector::id(id);
        let listed = self
            .copy_sessions(daemon, &self.space(&selector).await?)
            .await?;
        let origin = leaving.as_ref().and_then(|leaving| leaving.origin.clone());
        let execution = (self.save_taking(daemon, id, leaving, taking, &listed)).await?;
        let space = id.to_owned();
        (self.store)
            .write(move |tx| super::record::forget(tx, &space))
            .await?;
        if held {
            return Ok(());
        }
        let env = crate::terminal::detached_start_environment(origin.as_ref());
        (daemon
            .start_stopped_restore(&selector, &execution, env)
            .await)
            .map(drop)
    }

    /// `taking` is told what the space is and where the sessions `listed`
    /// are. Returns the execution `taking` is saved as, `leaving`'s when
    /// there was one.
    pub(crate) async fn save_taking(
        &self,
        daemon: &dyn Daemon,
        id: &SpaceId,
        leaving: Option<Leaving>,
        taking: Taking,
        listed: &[Listed],
    ) -> Result<ExecutionId> {
        let handoff = (self.plan_handoff(daemon, id, leaving, taking, listed)).await?;
        self.carry_out_handoff(daemon, handoff).await
    }

    /// Settles what can fail before `leaving` is stopped, staging `taking`
    /// beside the space's last sessions.
    async fn plan_handoff(
        &self,
        daemon: &dyn Daemon,
        id: &SpaceId,
        leaving: Option<Leaving>,
        taking: Taking,
        listed: &[Listed],
    ) -> Result<Handoff> {
        let space = self.space(&SpaceSelector::id(id)).await?;
        let told = brief::told(&space, leaving.as_ref(), &taking.agent, listed);
        let sides = match &leaving {
            Some(leaving) => Some(
                self.handoff_sides(daemon, &space, leaving, &taking.agent)
                    .await?,
            ),
            None => None,
        };
        let origin = leaving.as_ref().and_then(|leaving| leaving.origin.clone());
        let execution =
            (leaving.map(|leaving| leaving.execution)).unwrap_or_else(ExecutionId::generate);
        let taking = taking
            .saved(execution.clone(), origin, told)
            .stage(&self.paths, id)?;
        Ok(Handoff {
            space,
            execution,
            taking,
            sides,
        })
    }

    /// Stops and discards the leaving agent, then puts `taking` in its place.
    /// Only renaming the staged session and recording the handoff follow
    /// the discard, so the space is left without an agent only when the file
    /// system or the store fails.
    async fn carry_out_handoff(
        &self,
        daemon: &dyn Daemon,
        handoff: Handoff,
    ) -> Result<ExecutionId> {
        let Handoff {
            space,
            execution,
            taking,
            sides,
        } = handoff;
        self.retire_leaving(daemon, &space).await?;
        taking.commit()?;
        self.signals.agent_changed();
        if let Some((from, to)) = sides {
            let space = space.id;
            (self.store)
                .write(move |tx| crate::store::agent_handoffs::record(tx, &space, &from, &to))
                .await?;
        }
        Ok(execution)
    }

    /// Keeps nothing of it, and discards what a stop saved of it, so the
    /// agent taking over is the space's only one.
    async fn retire_leaving(&self, daemon: &dyn Daemon, space: &crate::model::Space) -> Result<()> {
        let request = StopRequest::handoff();
        (daemon.stop_space_agent_executions(&space.id, &request)).await?;
        daemon.discard_replaced_agent(space).await
    }

    /// Each with the tool the space's settings give it.
    async fn handoff_sides(
        &self,
        daemon: &dyn Daemon,
        space: &crate::model::Space,
        leaving: &Leaving,
        taking: &Agent,
    ) -> Result<(HandedOverSession, HandedOverSession)> {
        let settings = daemon.space_settings(space).await?;
        let from = HandedOverSession {
            agent: leaving.agent.clone(),
            session: leaving.session.clone(),
            integration: settings.integration(&leaving.agent),
        };
        let to = HandedOverSession {
            agent: taking.clone(),
            session: None,
            integration: settings.integration(taking),
        };
        Ok((from, to))
    }
}

/// A handoff whose refusals are settled: the agent taking over, staged as
/// the space's last session under the execution it is saved as, and the
/// sides it records. Only [`Spaces::plan_handoff`] builds one, and only
/// [`Spaces::carry_out_handoff`] stops and discards the leaving agent.
struct Handoff {
    space: crate::model::Space,
    execution: ExecutionId,
    taking: StagedLastSession,
    sides: Option<(HandedOverSession, HandedOverSession)>,
}

impl Taking {
    /// Saved as `execution`, which ran at `origin`. It starts a new session
    /// told `told`.
    fn saved(self, execution: ExecutionId, origin: Option<Origin>, told: String) -> LastSession {
        LastSession {
            execution,
            agent: self.agent,
            session: None,
            moved: false,
            copied: None,
            converted: false,
            handoff: Some(HandoffBrief {
                told,
                note: self.note,
            }),
            origin,
            stop: None,
            turn: None,
            model: self.model,
            mode: self.mode,
            narrowing: self.narrowing,
        }
    }
}
