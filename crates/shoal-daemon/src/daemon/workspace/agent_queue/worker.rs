//! The one worker a workspace has: a launch in an existing workspace starts
//! its agent only while no other agent of its own runs, waits or is stopped
//! there. Chats in its worktree are not its agents and never count.
use anyhow::{Result, bail};

use crate::{daemon::workspace::Manager, execution::recovery, model::Workspace};

impl Manager {
    /// Refuse a launch in `workspace` while it has an agent of its own: a
    /// running one, one in the agent queue, or a stopped one awaiting
    /// `shoal resume`. A last session, whose agent exited on its own, does
    /// not count, since the new agent replaces it, nor does a start whose
    /// setup failed, whose place the agent left waiting takes. Requires the queue gate,
    /// which keeps another launch out until this one is recorded.
    pub(super) async fn ensure_takes_worker(&self, workspace: &Workspace) -> Result<()> {
        let space = workspace.number;
        if self
            .running_agent_workspaces()
            .await
            .contains(&workspace.id)
            || self
                .recorded_agent_workspaces()
                .await?
                .contains(&workspace.id)
        {
            bail!(
                "workspace {space} already has a running agent; attach to it with `shoal attach {space}`, restart it with `shoal restart {space}`, or start a chat beside it with `-m chat`"
            );
        }
        let id = workspace.id.clone();
        if self
            .store
            .read(move |tx| super::store::queues_agent_in(tx, &id))
            .await?
        {
            bail!(
                "workspace {space} already has an agent in the agent queue; `shoal queue` lists it"
            );
        }
        if !recovery::agent_records(&self.paths, &workspace.id)?.is_empty() {
            bail!(
                "workspace {space} has a stopped agent; resume it with `shoal resume {space}` or discard it with `shoal resume --discard {space}`"
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
