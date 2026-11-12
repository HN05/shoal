//! The one worker a workspace has: a launch in an existing workspace starts
//! its agent only while no other agent of its own runs, waits or is stopped
//! there. Chats in its worktree are not its agents and never count.
use anyhow::{Result, bail};

use crate::client::space_names::SpaceSelector;
use crate::{daemon::workspace::Manager, execution::recovery, model::Workspace};

impl Manager {
    /// Refuse a launch in `workspace` while it has an agent of its own: a
    /// running one, one in the agent queue, or a stopped one awaiting
    /// `shoal resume`. A last session, whose agent exited on its own, does
    /// not count, since the new agent replaces it, nor does a start whose
    /// setup failed, whose place the agent left waiting takes. Requires the
    /// queue gate, which keeps another launch out until this one is
    /// recorded.
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
            bail!(HasAgent(format!(
                "workspace {space} already has a running agent; attach to it with `shoal attach {space}`, restart it with `shoal restart {space}`, or start a chat beside it with `-m chat`"
            )));
        }
        let id = workspace.id.clone();
        if self
            .store
            .read(move |tx| super::store::queues_agent_in(tx, &id))
            .await?
        {
            bail!(HasAgent(format!(
                "workspace {space} already has an agent in the agent queue; `shoal queue` lists it"
            )));
        }
        if !recovery::agent_records(&self.paths, &workspace.id)?.is_empty() {
            bail!(HasAgent(format!(
                "workspace {space} has a stopped agent; resume it with `shoal resume {space}` or discard it with `shoal resume --discard {space}`"
            )));
        }
        Ok(())
    }

    /// Take the queue gate for an agent that registers in `space` without a
    /// queue entry, such as a restored one or a resumed last session,
    /// refusing it while a launch waits or starts there, which holds the
    /// workspace's one agent. Held until the agent is tracked, the gate
    /// keeps a launch from being admitted there meanwhile.
    pub(crate) async fn reserve_worker(
        &self,
        space: &SpaceSelector,
    ) -> Result<tokio::sync::MutexGuard<'_, ()>> {
        let gate = self.agent_queue.lock_admission().await;
        let workspace = self.workspace(space).await?;
        let id = workspace.id.clone();
        if self
            .store
            .read(move |tx| super::store::queues_launch_in(tx, &id))
            .await?
        {
            let space = workspace.number;
            bail!(HasAgent(format!(
                "workspace {space} has an agent waiting in the agent queue, which takes its place; `shoal queue` lists it"
            )));
        }
        Ok(gate)
    }
}

/// The refusal of a launch in a workspace that has an agent of its own,
/// which names the command that reaches that agent.
#[derive(Debug)]
pub struct HasAgent(String);

impl std::fmt::Display for HasAgent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for HasAgent {}

#[cfg(test)]
mod tests;
