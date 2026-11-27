//! Opening the space of a starting launch for the agent its caller runs there
//! now, the last step of every queued start.
use anyhow::{Context as _, Result};

use crate::Agents;
use crate::daemon::Daemon;
use crate::forge::items::link::LinkKind;
use crate::forge::items::reference::ItemInput;
use crate::model::QueuedPlan;
use crate::model::Relation;
use crate::model::Workspace;
use shoal_daemon_base::callers::Caller;
use shoal_ids::queue_entry_ids::QueueEntryId;
use shoal_protocol::space_names::SpaceSelector;

impl Agents {
    /// Open the space of the starting launch `id` for the agent `caller`
    /// runs there now, and return the launch's plan.
    pub async fn open_queued_start(
        &self,
        daemon: &dyn Daemon,
        id: &QueueEntryId,
        caller: Option<&Caller>,
    ) -> Result<QueuedPlan> {
        let queued = self.queued_plan(id).await?;
        let space = (queued.entry.space()).context("the starting launch names no space")?;
        if let Ok(workspace) = daemon.space_by_id(space).await?.into_workspace() {
            let pr = queued.start.pr.clone();
            self.open_admitted(daemon, id, &workspace, pr, caller)
                .await?;
        }
        Ok(queued)
    }

    /// Open `workspace` for the agent of the starting launch `id`: refuse
    /// one that is no longer its ready, verified worktree, give it the
    /// launch's entry, which its agent takes over, bring its branch up to
    /// date, then link `pr`.
    async fn open_admitted(
        &self,
        daemon: &dyn Daemon,
        id: &QueueEntryId,
        workspace: &Workspace,
        pr: Option<ItemInput>,
        caller: Option<&Caller>,
    ) -> Result<()> {
        let selector = SpaceSelector::id(&workspace.id);
        let workspace = daemon.reopen_workspace(&selector).await?.workspace;
        self.queue_attach(daemon, id, &selector).await?;
        daemon.refresh_admitted(&workspace).await;
        if let Some(pr) = pr {
            self.link_start_pr(daemon, workspace, &pr, caller).await;
        }
        Ok(())
    }

    /// Link `pr` as the PR the agent starting in `workspace` works on,
    /// telling the workspace's starter what that warns of, or why it was
    /// refused, which leaves the start to go on without it.
    async fn link_start_pr(
        &self,
        daemon: &dyn Daemon,
        workspace: Workspace,
        pr: &ItemInput,
        caller: Option<&Caller>,
    ) {
        let selector = SpaceSelector::id(&workspace.id);
        let (kind, relation) = (LinkKind::Pr, Relation::Assigned);
        let linked = (daemon.link_item(&selector, kind, pr, relation, caller)).await;
        let warnings = match linked {
            Ok(warnings) => warnings,
            Err(error) => vec![format!("cannot link PR {pr}: {error:#}")],
        };
        daemon
            .tell_start_warnings(&workspace.into(), &warnings)
            .await;
    }
}
