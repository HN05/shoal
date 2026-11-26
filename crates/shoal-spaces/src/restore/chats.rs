//! A restored chat's place: where it ran, which it does not own, and where
//! its restore records it again under its ID, stopped.
use crate::Spaces;
use crate::daemon::Daemon;
use anyhow::{Context, Result, bail, ensure};

use crate::model::{Chat, ChatPart, KeptChat, RemovedSpace, Workdir};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::space_names::SpaceSelector;

impl Spaces {
    /// Stopped, in the place it ran, with its mode and permissions. Its
    /// restore puts back its links and what it saved of its agent.
    pub(super) async fn reopen_chat_place(
        &self,
        daemon: &dyn Daemon,
        removed: &RemovedSpace,
        parent: Option<SpaceId>,
    ) -> Result<()> {
        let kept = (removed.chat()).context("the removal record is not a chat's")?;
        let workdir = self.restored_workdir(daemon, removed, kept).await?;
        let chat = restored_record(removed, kept, workdir);
        self.record_restored_chat(daemon, chat, parent).await?;
        Ok(())
    }

    /// Where it ran. Refuses a place where it can no longer run: the worktree
    /// of a workspace that is gone, still removed, which its own restore
    /// opens first, or not ready, or a `root_dir` that no longer exists.
    async fn restored_workdir(
        &self,
        daemon: &dyn Daemon,
        removed: &RemovedSpace,
        kept: &KeptChat,
    ) -> Result<Workdir> {
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
            let workspace =
                (self.chat_workspace(daemon, &SpaceSelector::id(&workspace.id))).await?;
            return Ok(Workdir::Worktree {
                workspace: workspace.reference(),
                path: workspace.part.path,
            });
        }
        if let Some(repository) = &removed.repository_id {
            let path = daemon
                .repository(crate::repository::Selector::from(repository))
                .await?
                .path;
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

/// Under its ID, stopped, with what it was given, working in `workdir`.
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
        restart: None,
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
