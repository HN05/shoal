//! The work in progress that a daemon exit would cut off, named for the
//! client that asked for the exit.
use anyhow::Result;

use crate::client::space_ids::SpaceId;
use crate::daemon::workspace::Manager;
use crate::protocol::{Work, WorkSpace};
use crate::state::WorkspaceState;

impl Manager {
    /// Each workspace in a state an exit would interrupt, each operation
    /// and connected command that would stop in a workspace not already
    /// listed, or, when none, the requests and background passes that keep
    /// the daemon from exiting.
    pub(crate) async fn work_an_exit_cuts_off(&self) -> Result<Vec<Work>> {
        let workspaces = self.store.read(workspaces).await?;
        let mut work: Vec<_> = (workspaces.iter())
            .filter(|(_, state)| WorkspaceState::transient().any(|transient| transient == *state))
            .map(|(workspace, state)| Work::Workspace {
                workspace: workspace.clone(),
                state: *state,
            })
            .collect();
        let operations = (self.operations.running().into_iter())
            .map(|entry| (entry.workspace.clone(), Some(entry.kind)));
        let commands = (self.executions.unattachable_spaces().into_iter()).map(|id| (id, None));
        for (id, operation) in operations.chain(commands) {
            if work.iter().any(|listed| working_in(listed) == Some(&id)) {
                continue;
            }
            let Some((workspace, _)) = workspaces.iter().find(|(workspace, _)| workspace.id == id)
            else {
                continue;
            };
            let workspace = workspace.clone();
            work.push(match operation {
                Some(operation) => Work::Operation {
                    workspace,
                    operation,
                },
                None => Work::Command { workspace },
            });
        }
        if work.is_empty() {
            work.push(Work::Other);
        }
        Ok(work)
    }
}

/// Whether `work` holds only requests and background passes, which end
/// soon on their own.
pub(super) fn only_passes(work: &[Work]) -> bool {
    work.iter().all(|work| matches!(work, Work::Other))
}

/// The workspace `work` works in.
fn working_in(work: &Work) -> Option<&SpaceId> {
    match work {
        Work::Workspace { workspace, .. }
        | Work::Operation { workspace, .. }
        | Work::Command { workspace } => Some(&workspace.id),
        Work::Other => None,
    }
}

/// Every workspace with its state, by number.
fn workspaces(tx: &rusqlite::Transaction<'_>) -> Result<Vec<(WorkSpace, WorkspaceState)>> {
    let mut statement =
        tx.prepare("SELECT id, number, name, state FROM workspaces ORDER BY number")?;
    let rows = statement.query_map([], |row| {
        let workspace = WorkSpace {
            id: row.get("id")?,
            number: row.get("number")?,
            name: row.get("name")?,
        };
        Ok((workspace, row.get("state")?))
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}
