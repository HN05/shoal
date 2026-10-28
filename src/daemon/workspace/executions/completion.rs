//! Persist verified execution completion and setup readiness in one transaction.
use anyhow::Result;
use rusqlite::{Transaction, TransactionBehavior, params};

use super::{ExecutionKind, Manager};
use crate::state::{ExecutionState, WorkspaceState};

impl Manager {
    pub(super) async fn record_execution_completion(
        &self,
        id: &str,
        kind: ExecutionKind,
        exit_code: Option<i32>,
        issue: Option<String>,
    ) -> Result<Option<String>> {
        let complete = issue.is_none();
        let record_id = id.to_owned();
        let hosts: Vec<_> = self.session_hosts.iter().map(|host| host.name()).collect();
        self.store
            .run(move |db| {
                let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
                if kind == ExecutionKind::Setup {
                    record_setup_result(&tx, &record_id, exit_code, issue.as_deref())?;
                }
                let demo = if complete {
                    super::super::sessions::pending::record_demo(&tx, &record_id, &hosts)?
                } else {
                    None
                };
                record_execution_end(&tx, &record_id, complete)?;
                tx.commit()?;
                Ok(demo)
            })
            .await
    }

    pub(super) async fn delete_execution_record(&self, id: &str) -> Result<Option<String>> {
        let id = id.to_owned();
        let hosts: Vec<_> = self.session_hosts.iter().map(|host| host.name()).collect();
        self.store
            .run(move |db| {
                let tx = db.transaction()?;
                let demo = super::super::sessions::pending::record_demo(&tx, &id, &hosts)?;
                record_execution_end(&tx, &id, true)?;
                tx.commit()?;
                Ok(demo)
            })
            .await
    }
}

/// Persist setup readiness and its diagnostic only for the preparing workspace
/// that owns this execution; verification failure never readies it.
fn record_setup_result(
    tx: &Transaction<'_>,
    id: &str,
    exit_code: Option<i32>,
    issue: Option<&str>,
) -> Result<()> {
    let complete = issue.is_none();
    let code = exit_code.unwrap_or(1);
    let error = (!(complete && code == 0)).then(|| {
        let reason = issue.map_or(String::new(), |issue| format!("; {issue}"));
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
        params![id, state, error, error.is_none(), WorkspaceState::Preparing],
    )?;
    Ok(())
}

/// Retain unverified executions for recovery; only verified completion deletes
/// the record, together with any demo it owns.
fn record_execution_end(tx: &Transaction<'_>, id: &str, complete: bool) -> Result<()> {
    if complete {
        tx.execute("DELETE FROM executions WHERE id=?1", [id])?;
    } else {
        tx.execute(
            "UPDATE executions SET state=?2 WHERE id=?1",
            params![id, ExecutionState::Unknown],
        )?;
    }
    Ok(())
}
