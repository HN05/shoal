//! Persist verified execution completion and setup readiness in one transaction.
use anyhow::Result;
use rusqlite::{TransactionBehavior, params};

use super::{ExecutionKind, Manager};
use crate::{
    demos::Launch,
    state::{ExecutionState, WorkspaceState},
};

impl Manager {
    pub(super) async fn record_execution_completion(
        &self,
        id: &str,
        kind: ExecutionKind,
        exit_code: Option<i32>,
        issue: Option<String>,
    ) -> Result<Option<Launch>> {
        let complete = issue.is_none();
        let record_id = id.to_owned();
        self.store
            .run(move |db| {
                let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
                if kind == ExecutionKind::Setup {
                    let code = exit_code.unwrap_or(1);
                    let error = (!(complete && code == 0)).then(|| {
                        let reason = issue.as_ref().map_or(String::new(), |issue| format!("; {issue}"));
                        if code == 0 {
                            format!("setup command exited successfully (exit 0), but process verification is incomplete{reason}; stop its processes with shoal stop, then retry or skip setup")
                        } else {
                            format!("setup failed (exit {code}, processes stopped: {complete}){reason}; retry with shoal work setup")
                        }
                    });
                    let state = if error.is_none() {
                        WorkspaceState::Ready
                    } else {
                        WorkspaceState::Failed
                    };
                    tx.execute(
                        "UPDATE workspaces SET state=?2,error=?3,setup_finished=?4 WHERE id=(SELECT workspace_id FROM executions WHERE id=?1) AND state=?5",
                        params![
                            record_id,
                            state,
                            error,
                            error.is_none(),
                            WorkspaceState::Preparing
                        ],
                    )?;
                }
                let demo = if complete { super::super::demos::execution_launch(&tx, &record_id)? } else { None };
                if complete {
                    tx.execute("DELETE FROM executions WHERE id=?1", [record_id])?;
                } else {
                    tx.execute(
                        "UPDATE executions SET state=?2 WHERE id=?1",
                        params![record_id, ExecutionState::Unknown],
                    )?;
                }
                tx.commit()?;
                Ok(demo)
            })
            .await
    }

    pub(super) async fn delete_execution_record(&self, id: &str) -> Result<Option<Launch>> {
        let id = id.to_owned();
        self.store
            .run(move |db| {
                let tx = db.transaction()?;
                let demo = super::super::demos::execution_launch(&tx, &id)?;
                tx.execute("DELETE FROM executions WHERE id=?1", [id])?;
                tx.commit()?;
                Ok(demo)
            })
            .await
    }
}
