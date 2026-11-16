//! Restoring a removed chat: recording it again under its ID, stopped, in
//! the place it ran, with what its removal record keeps of its agent.
use anyhow::{Context, Result, bail, ensure};

use crate::daemon::scope::Caller;
use crate::daemon::workspace::Manager;
use crate::model::{Chat, KeptChat, RemovedSpace, RestoredChat};
use shoal_protocol::space_names::{SpaceName, SpaceSelector};

impl Manager {
    /// Record the removed chat `removed` again, stopped, in the place it
    /// ran, below its parent while that takes it, with its links, mode,
    /// permissions and the stopped agents and last sessions it saved, which
    /// a restore of its agent continues, and the session files an import
    /// kept for it; `caller` relinks only what it may link. The caller links
    /// its PRs and puts the session files in place.
    pub(super) async fn reopen_removed_chat(
        &self,
        removed: RemovedSpace,
        caller: Option<&Caller>,
    ) -> Result<RestoredChat> {
        let kept = (removed.chat()).context("the removal record is not a chat's")?;
        self.ensure_chat_place(&removed, kept).await?;
        let mut warnings = Vec::new();
        let parent = (self.restored_parent(&removed, caller, &mut warnings)).await?;
        let chat = self
            .record_restored_chat(restored_record(&removed, kept), parent)
            .await?;
        let id = &chat.id;
        warnings.extend(self.relink_space(id, &removed.links, caller).await);
        let session_files = self.restore_agents(&removed, id, &mut warnings).await;
        if removed.agents.is_empty() && removed.last_sessions.is_empty() {
            warnings.push(format!(
                "{} saved no session of its agent, whose restore starts a new one",
                removed.name
            ));
        }
        self.adopt_removal(&removed).await?;
        let chat = (self.chat_by_id(id).await?).context("the chat was removed meanwhile")?;
        Ok(RestoredChat {
            chat,
            removed,
            session_files,
            warnings,
        })
    }

    /// Refuse to restore the chat `removed` where it can no longer run: in
    /// the worktree of a workspace that is gone, or still removed, which its
    /// own restore opens first, or not ready, or in a `root_dir` that no
    /// longer exists.
    async fn ensure_chat_place(&self, removed: &RemovedSpace, kept: &KeptChat) -> Result<()> {
        if let Some(workspace) = &kept.workspace {
            if self.find_space(&workspace.id).await?.is_none() {
                let restorable = (self.removed_workspaces().await?.iter())
                    .any(|record| record.space_id == workspace.id);
                ensure!(
                    !restorable,
                    "{} ran in the worktree of {}, which is removed; shoal restore {} {} opens it first",
                    removed.name,
                    workspace.name,
                    workspace.id,
                    removed.space_id
                );
                bail!(
                    "{} ran in the worktree of {}, which no longer exists",
                    removed.name,
                    workspace.name
                );
            }
            self.chat_workspace(&SpaceSelector::id(&workspace.id))
                .await?;
        }
        if let Some(root_dir) = &kept.root_dir {
            ensure!(
                root_dir.is_dir(),
                "{} ran in {}, which no longer exists",
                removed.name,
                root_dir.display()
            );
        }
        Ok(())
    }
}

/// The chat record that restores `removed`, which `kept` describes: under
/// its ID, stopped, with what it was given and where it ran.
fn restored_record(removed: &RemovedSpace, kept: &KeptChat) -> Chat {
    Chat {
        id: removed.space_id.clone(),
        number: Default::default(),
        repository_id: removed.repository_id.clone(),
        // A name an older Shoal recorded may be longer than one set now
        // allows.
        name: SpaceName::derive(removed.name.as_str(), "chat"),
        agent: kept.agent.clone(),
        mode: kept.mode.clone(),
        narrowing: kept.narrowing,
        workspace: kept.workspace.clone(),
        started_at: crate::time::UnixTime::now(),
        origin: kept.origin.clone(),
        tmp_dir: None,
        root_dir: kept.root_dir.clone(),
        running: false,
        links: Default::default(),
        review: Vec::new(),
        holds: Vec::new(),
        completion: None,
        agent_state: None,
        progress: None,
        sleep: None,
        asleep: None,
        parent: None,
        children: Vec::new(),
        converted: false,
    }
}

#[cfg(test)]
mod tests;
