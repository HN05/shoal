//! Executions whose processes ran in an earlier boot have ended, since a
//! restart of the machine ends every process. The daemon forgets them,
//! keeping the recovery records they saved for `shoal resume`, and the session
//! of an agent that saved none as one of its workspace's last sessions.
use crate::process::identity::IdentityExt;
use anyhow::Result;

use crate::Agents;
use crate::daemon::Daemon;
use crate::execution::recovery::saved_path;
use crate::log;
use crate::model::Execution;
use crate::state::ExecutionState;

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
            self.keep_unsaved_session(daemon, &execution).await?;
            self.forget_execution(daemon, &execution.id).await?;
            log!("forgot execution {} of an earlier boot", execution.id);
        }
        Ok(())
    }

    /// Keep the session of an agent that ended without saving a recovery
    /// record as `shoal resume` continues one that exited on its own.
    async fn keep_unsaved_session(&self, daemon: &dyn Daemon, execution: &Execution) -> Result<()> {
        // Its record, or the last session an interrupted pass kept, which
        // keeping it again would replace once its reported session is gone.
        if saved_path(&self.paths, &execution.space_id, &execution.id).exists() {
            return Ok(());
        }
        let id = execution.id.clone();
        let agent = self
            .store
            .run(move |db| shoal_store::execution_agent(db, &id))
            .await?;
        let Some(name) = agent else {
            return Ok(());
        };
        let agent = shoal_protocol::executions::TrackedAgent {
            name,
            model: execution.model.clone(),
            mode: (execution.mode.clone()).unwrap_or(crate::config::modes::ModeName::DEFAULT),
            narrowing: execution.narrowing,
            new_session: false,
        };
        let turn = self.reported_turn(&execution.id).await;
        self.keep_agent_session(daemon, &execution.space_id, &agent, &execution.id, turn)
            .await
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
