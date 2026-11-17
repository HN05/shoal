//! Restoring a removed chat: recording it again under its ID, stopped, in
//! the place it ran, with what its removal record keeps of its agent.
use anyhow::{Context, Result, bail, ensure};

use crate::daemon::scope::Caller;
use crate::daemon::workspace::Manager;
use crate::model::{Chat, ChatPart, KeptChat, RemovedSpace, RestoredChat, Workdir};
use shoal_protocol::space_names::SpaceSelector;

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
        let workdir = self.restored_workdir(&removed, kept).await?;
        let mut warnings = Vec::new();
        let parent = (self.restored_parent(&removed, caller, &mut warnings)).await?;
        let chat = self
            .record_restored_chat(restored_record(&removed, kept, workdir), parent)
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

    /// Where the chat `removed` works once restored: where it ran. Refuses
    /// it where it can no longer run: in the worktree of a workspace that is
    /// gone, or still removed, which its own restore opens first, or not
    /// ready, or in a `root_dir` that no longer exists.
    async fn restored_workdir(&self, removed: &RemovedSpace, kept: &KeptChat) -> Result<Workdir> {
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
            let workspace = (self.chat_workspace(&SpaceSelector::id(&workspace.id))).await?;
            return Ok(Workdir::Worktree {
                workspace: workspace.reference(),
                path: workspace.part.path,
            });
        }
        if let Some(repository) = &removed.repository_id {
            let path = self.repository(repository).await?.path;
            return Ok(Workdir::Checkout { path });
        }
        let root_dir = (kept.root_dir.clone())
            .with_context(|| format!("{} recorded no root_dir it ran in", removed.name))?;
        ensure!(
            root_dir.is_dir(),
            "{} ran in {}, which no longer exists",
            removed.name,
            root_dir.display()
        );
        Ok(Workdir::RootDir { path: root_dir })
    }
}

/// The chat record that restores `removed`, which `kept` describes: under
/// its ID, stopped, with what it was given, working in `workdir`.
fn restored_record(removed: &RemovedSpace, kept: &KeptChat, workdir: Workdir) -> Chat {
    Chat {
        id: removed.space_id.clone(),
        number: Default::default(),
        repository_id: removed.repository_id.clone(),
        name: removed.name.clone(),
        mode: Some(kept.mode.clone()),
        narrowing: kept.narrowing,
        running: false,
        links: Default::default(),
        review: Vec::new(),
        holds: Vec::new(),
        agent_state: None,
        progress: None,
        sleep: None,
        wake_at: None,
        asleep: None,
        parent: None,
        children: Vec::new(),
        workdir,
        part: ChatPart {
            // Its agent is the one the saved agents its restore puts back
            // name.
            agent: None,
            started_at: crate::time::UnixTime::now(),
            tmp_dir: None,
            completion: None,
        },
    }
}

#[cfg(test)]
mod tests;
