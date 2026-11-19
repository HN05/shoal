//! Opening the space of a starting launch for the agent its caller runs
//! there now, the last step of every queued start, whoever runs it: a
//! workspace must still be its ready, verified worktree, its launch's entry
//! takes it, its branch is brought up to date as an admitted agent's is,
//! and the PR its start names is linked. A chat, which its entry took when
//! it was recorded, needs none of it.
use anyhow::{Context as _, Result};

use crate::{
    daemon::{manager::Manager, scope::Caller},
    forge::items::{link::LinkKind, reference::ItemInput},
    model::{QueuedPlan, Relation, Workspace},
    start_plan::QueuedStart,
};
use shoal_ids::queue_entry_ids::QueueEntryId;
use shoal_protocol::space_names::SpaceSelector;

impl Manager {
    /// Open the space of the starting launch `id` for the agent `caller`
    /// runs there now, and return the launch's plan.
    pub(crate) async fn open_queued_start(
        &self,
        id: &QueueEntryId,
        caller: Option<&Caller>,
    ) -> Result<QueuedPlan> {
        let queued = self.queued_plan(id).await?;
        let space = (queued.entry.space()).context("the starting launch names no space")?;
        if let Ok(workspace) = self.space_by_id(space).await?.into_workspace() {
            let start = QueuedStart::of(&queued.plan)?;
            self.open_admitted(id, &workspace, start.pr, caller).await?;
        }
        Ok(queued)
    }

    /// Open `workspace` for the agent of the starting launch `id`: refuse
    /// one that is no longer its ready, verified worktree, give it the
    /// launch's entry, which its agent takes over, bring its branch up to
    /// date, then link `pr`.
    async fn open_admitted(
        &self,
        id: &QueueEntryId,
        workspace: &Workspace,
        pr: Option<ItemInput>,
        caller: Option<&Caller>,
    ) -> Result<()> {
        let selector = SpaceSelector::id(&workspace.id);
        let workspace = self.reopen_workspace(&selector).await?.workspace;
        self.queue_attach(id, &selector).await?;
        self.refresh_admitted(&workspace).await;
        if let Some(pr) = pr {
            self.link_start_pr(workspace, &pr, caller).await;
        }
        Ok(())
    }

    /// Link `pr` as the PR the agent starting in `workspace` works on,
    /// telling the workspace's starter what that warns of, or why it was
    /// refused, which leaves the start to go on without it.
    async fn link_start_pr(&self, workspace: Workspace, pr: &ItemInput, caller: Option<&Caller>) {
        let selector = SpaceSelector::id(&workspace.id);
        let (kind, relation) = (LinkKind::Pr, Relation::Assigned);
        let linked = (self.link_item(&selector, kind, pr, relation, None, caller)).await;
        let warnings = match linked {
            Ok(linked) => linked.warnings,
            Err(error) => vec![format!("cannot link PR {pr}: {error:#}")],
        };
        self.tell_start_warnings(&workspace.into(), &warnings).await;
    }
}

#[cfg(test)]
mod tests;
