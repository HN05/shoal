//! Executions whose processes ran in an earlier boot have ended, since a
//! restart of the machine ends every process. The daemon forgets them and
//! saves each agent among them as a shutdown's stop saves it, so its restore
//! waits in the agent queue: its machine went down without the user choosing
//! it.
use crate::process::identity::IdentityExt;
use anyhow::Result;

use crate::Agents;
use crate::daemon::Daemon;
use crate::execution::recovery::{
    self, LastSession, LastSessionExt, Record, RecordExt, Stage, record_path,
};
use crate::log;
use crate::model::Execution;
use crate::state::ExecutionState;
use crate::terminal::detached_start_environment;
use shoal_daemon_base::events::{AgentEvent, EventKind};
use shoal_protocol::agent_ends::StopCause;

impl Agents {
    /// Forget the unknown executions of an earlier boot.
    pub async fn forget_earlier_boot(&self, daemon: &dyn Daemon) -> Result<()> {
        let ended = self
            .store
            .run(|db| {
                let mut executions = shoal_store::all_executions(db)?;
                executions.retain(|execution| {
                    execution.state == ExecutionState::Unknown && ran_in_earlier_boot(execution)
                });
                Ok(executions)
            })
            .await?;
        for execution in ended {
            self.save_ended_agent(&execution).await?;
            self.forget_execution(daemon, &execution.id).await?;
            log!("forgot execution {} of an earlier boot", execution.id);
        }
        Ok(())
    }

    /// Save the agent of `execution`, which its machine going down ended, as
    /// a shutdown's stop saves it: with a request for the agent queue to
    /// restore it, unless no session to resume is known, which leaves it for
    /// `shoal resume`, in its terminal host's place. A record a stop, or an
    /// interrupted pass, left half done gets the request too, as does one a
    /// waiting wrapper was to restore; a finished stop's, and a last session
    /// an earlier release's pass kept, stay.
    async fn save_ended_agent(&self, execution: &Execution) -> Result<()> {
        let (paths, space) = (&self.paths, &execution.space_id);
        let saved = match Record::load(&record_path(paths, space, &execution.id)) {
            Ok(saved) => saved,
            // Saving over it would lose the session and handoff it may hold.
            Err(error) => {
                log!(
                    "kept the unreadable record of execution {}: {error:#}",
                    execution.id
                );
                return Ok(());
            }
        };
        let record = match saved {
            // A wrapper that waited to restore it ended with its boot.
            Some(record) if record.stage == Some(Stage::Stopping) || record.waiting.is_some() => {
                record
            }
            Some(_) => return Ok(()),
            None if LastSession::path(paths, space, &execution.id).exists() => return Ok(()),
            None => match self.ended_agent_record(execution).await? {
                Some(record) => record,
                None => return Ok(()),
            },
        };
        let record = Record {
            origin: record.origin.or_else(|| execution.origin.clone()),
            ..record
        };
        let resumable = record.session.is_some()
            || recovery::carried::continued(paths, space, &execution.id)
                .session
                .is_some();
        let env = resumable.then(|| detached_start_environment(record.origin.as_ref()));
        let (agent, mode) = (record.agent.clone(), record.mode.clone());
        recovery::save_stopped(paths, space, &execution.id, record, env)?;
        self.record_machine_down(execution, agent, mode).await;
        Ok(())
    }

    /// Record that its machine going down stopped `agent`, which `execution`
    /// ran in `mode`.
    async fn record_machine_down(
        &self,
        execution: &Execution,
        agent: crate::agent::Agent,
        mode: crate::config::modes::ModeName,
    ) {
        let stopped = AgentEvent {
            name: agent,
            execution_id: Some(execution.id.clone()),
            mode: Some(mode),
            exit_code: None,
            oom_kills: 0,
            source: None,
            cause: Some(StopCause::MachineDown),
        };
        let kind = EventKind::AgentStopped;
        let space = &execution.space_id;
        shoal_daemon_base::events::record_agent(&self.store, space, kind, stopped).await;
    }

    /// Save the record of the agent `execution` ran, which saved none, at the
    /// turn it last reported and in its terminal host's place, as a stop it
    /// has yet to finish; none for an execution that ran no agent.
    async fn ended_agent_record(&self, execution: &Execution) -> Result<Option<Record>> {
        let id = execution.id.clone();
        let agent = self
            .store
            .run(move |db| crate::executions::execution_agent(db, &id))
            .await?;
        let Some(name) = agent else {
            return Ok(None);
        };
        let agent = shoal_protocol::executions::TrackedAgent {
            name,
            model: execution.model.clone(),
            mode: (execution.mode.clone()).unwrap_or(crate::config::modes::ModeName::DEFAULT),
            narrowing: execution.narrowing,
            new_session: false,
        };
        let place = execution.origin.clone();
        let record = Record {
            turn: self.reported_turn(&execution.id).await,
            ..Record::new(&agent, Some(MACHINE_DOWN_REASON), Stage::Stopping, place)
        };
        let (paths, space) = (&self.paths, &execution.space_id);
        let path = recovery::save_stopping(paths, space, &execution.id, record)?;
        Record::load(&path)
    }
}

/// The stop reason of an agent its machine going down ended, which its
/// restore tells it.
const MACHINE_DOWN_REASON: &str = "its machine went down";

/// Whether the execution's wrapper, and its child when recorded, ran in an
/// earlier boot. One without a recorded wrapper may have run in this one.
fn ran_in_earlier_boot(execution: &Execution) -> bool {
    execution
        .wrapper
        .as_ref()
        .is_some_and(|wrapper| wrapper.of_earlier_boot())
        && execution
            .child
            .as_ref()
            .is_none_or(|child| child.of_earlier_boot())
}
