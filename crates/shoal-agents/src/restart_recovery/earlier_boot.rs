//! Executions whose processes ran in an earlier boot have ended, since a
//! restart of the machine ends every process. The daemon saves each one's
//! agent as a system shutdown's stop saves it, then forgets the execution.
use crate::process::identity::IdentityExt;
use anyhow::Result;
use shoal_protocol::executions::TrackedAgent;
use shoal_protocol::stop_causes::StopCause;

use crate::Agents;
use crate::daemon::Daemon;
use crate::execution::recovery::{self, Record, RecordExt as _, Stage, record_path};
use crate::log;
use crate::model::Execution;
use crate::state::ExecutionState;

/// The stop reason of an agent whose machine went down while it ran.
const MACHINE_DOWN_REASON: &str = "its machine went down";

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
            self.save_ended_agent(daemon, &execution).await?;
            self.forget_execution(daemon, &execution.id).await?;
            log!("forgot execution {} of an earlier boot", execution.id);
        }
        Ok(())
    }

    /// Save the agent `execution` ran, which its machine going down ended,
    /// for the agent queue to restore, as a system shutdown's stop saves it.
    /// A record its wrapper finished saving chose its restore already. One a
    /// stop left `stopping`, whose wrapper never finished, gets the request
    /// a new record does. The record names the cause, which forgetting the
    /// execution logs, so a pass cut off after the save still logs it.
    async fn save_ended_agent(&self, daemon: &dyn Daemon, execution: &Execution) -> Result<()> {
        let (space_id, id) = (&execution.space_id, &execution.id);
        let path = record_path(&self.paths, space_id, id);
        let record = match Record::load(&path)? {
            Some(record) if record.stage == Some(Stage::Stopping) => record,
            Some(_) => return Ok(()),
            None => match self.ended_agent(execution).await? {
                Some(record) => record,
                None => return Ok(()),
            },
        };
        let record = Record {
            cause: record.cause.or(Some(StopCause::MachineDown)),
            ..record
        };
        let restore = self.unattended_restore(daemon, execution, &record).await;
        recovery::save_stopped(&self.paths, space_id, id, record, restore)?;
        Ok(())
    }

    /// The record of the agent `execution` ran, which saved none, in the
    /// turn it last reported; none for an execution that ran no agent.
    async fn ended_agent(&self, execution: &Execution) -> Result<Option<Record>> {
        let id = execution.id.clone();
        let agent = self
            .store
            .run(move |db| crate::executions::execution_agent(db, &id))
            .await?;
        let Some(name) = agent else {
            return Ok(None);
        };
        let agent = TrackedAgent {
            name,
            model: execution.model.clone(),
            mode: (execution.mode.clone()).unwrap_or(crate::config::modes::ModeName::DEFAULT),
            narrowing: execution.narrowing,
            new_session: false,
        };
        let stopped = Record::new(&agent, Some(MACHINE_DOWN_REASON), Stage::Stopped, None);
        Ok(Some(Record {
            origin: execution.origin.clone(),
            turn: self.reported_turn(&execution.id).await,
            ..stopped
        }))
    }

    /// The variables of the terminal host the agent queue restores the
    /// agent of `record` with, when its restore needs no session picker;
    /// otherwise none, so that it waits for `shoal resume`, as it does when
    /// its space's settings cannot be read.
    async fn unattended_restore(
        &self,
        daemon: &dyn Daemon,
        execution: &Execution,
        record: &Record,
    ) -> Option<std::collections::BTreeMap<String, String>> {
        let settings = match self.settings_of(daemon, &execution.space_id).await {
            Ok(settings) => settings,
            Err(error) => {
                log!(
                    "agent of execution {} waits for shoal resume: {error:#}",
                    execution.id
                );
                return None;
            }
        };
        let session = (record.session.clone()).or_else(|| {
            recovery::carried::continued(&self.paths, &execution.space_id, &execution.id).session
        });
        settings
            .restores_unattended(&record.agent, session.as_ref())
            .then(|| crate::terminal::detached_start_environment(record.origin.as_ref()))
    }

    /// The settings of the space `id`.
    async fn settings_of(
        &self,
        daemon: &dyn Daemon,
        id: &shoal_ids::space_ids::SpaceId,
    ) -> Result<crate::config::Effective> {
        let space = daemon.space_by_id(id).await?;
        daemon.space_settings(&space).await
    }
}

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
