//! Executions whose processes ran in an earlier boot have ended, since a
//! restart of the machine ends every process. The daemon forgets them,
//! keeping the recovery records they saved for `shoal resume`, and the session
//! of an agent that saved none as one of its workspace's last sessions.
use anyhow::Result;

use super::{Manager, store};
use crate::{
    daemon::log, execution::recovery::record_path, model::Execution, state::ExecutionState,
};

impl Manager {
    /// Forget the unknown executions of an earlier boot.
    pub(super) async fn forget_earlier_boot(&self) -> Result<()> {
        let ended = self
            .store
            .run(|db| {
                let mut executions = store::all_executions(db)?;
                executions.retain(|execution| {
                    execution.state == ExecutionState::Unknown && ran_in_earlier_boot(execution)
                });
                Ok(executions)
            })
            .await?;
        for execution in ended {
            self.keep_unsaved_session(&execution).await?;
            self.forget_execution(&execution.id).await?;
            log!("forgot execution {} of an earlier boot", execution.id);
        }
        Ok(())
    }

    /// Keep the session of an agent that ended without saving a recovery
    /// record as `shoal resume` continues one that exited on its own.
    async fn keep_unsaved_session(&self, execution: &Execution) -> Result<()> {
        if record_path(&self.paths, &execution.workspace_id, &execution.id).exists() {
            return Ok(());
        }
        let id = execution.id.clone();
        let agent = self
            .store
            .run(move |db| store::execution_agent(db, &id))
            .await?;
        let Some(agent) = agent else {
            return Ok(());
        };
        let workspace = self.workspace(&execution.workspace_id).await?;
        self.keep_agent_session(&workspace, &agent, &execution.id)
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
