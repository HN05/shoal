//! The named checks and views that confine a scoped caller to what its
//! scope allows.
use anyhow::{Context, Result, ensure};

use super::Caller;
use crate::{
    daemon::{approvals::ApprovalRequest, questions::Question, workspace::Manager},
    execution::recovery,
    model::{Chat, QueueEntry, QueueRequest, Repository, Workspace, WorkspaceTarget},
    repository::Selector,
};

impl Manager {
    /// The repositories `caller` may see: its own workspace's, or all of them
    /// for an unscoped caller.
    pub(in crate::daemon) async fn visible_repositories(
        &self,
        caller: Option<&Caller>,
    ) -> Result<Vec<Repository>> {
        let mut repositories = self.repositories().await?;
        if let Some(caller) = caller {
            let own = self.workspace(&caller.workspace_id).await?.repository_id;
            repositories.retain(|repository| repository.id == own);
        }
        Ok(repositories)
    }

    /// The workspaces `target` finds among those `caller` may see: the ones
    /// in its workspace tree, or all of them for an unscoped caller.
    pub(in crate::daemon) async fn find_visible_workspaces(
        &self,
        target: &WorkspaceTarget,
        caller: Option<&Caller>,
    ) -> Result<Vec<Workspace>> {
        let within = match caller {
            Some(caller) => Some(self.subtree(&caller.workspace_id).await?),
            None => None,
        };
        self.find_workspaces(target, within).await
    }

    /// `workspace`, which must be the caller's own; `refusal` says why
    /// another is refused.
    pub(in crate::daemon) async fn own_workspace<'a>(
        &self,
        caller: &Caller,
        workspace: &'a mut String,
        refusal: &str,
    ) -> Result<&'a mut String> {
        ensure!(
            self.workspace(workspace).await?.id == caller.workspace_id,
            "{refusal}"
        );
        Ok(workspace)
    }

    /// `workspace`, which must not be the caller's own; [`super::authorize`] confines
    /// it to the workspaces below. `refusal` says why the caller's own is
    /// refused.
    pub(in crate::daemon) async fn workspace_below<'a>(
        &self,
        caller: &Caller,
        workspace: &'a mut String,
        refusal: &str,
    ) -> Result<&'a mut String> {
        ensure!(
            self.workspace(workspace).await?.id != caller.workspace_id,
            "{refusal}"
        );
        Ok(workspace)
    }

    /// `workspace`, which must be one below the caller's whose agent stopped
    /// as `execution` the caller's process or one below it stopped: an agent
    /// the user or Shoal stopped, or that exited on its own, is the user's
    /// to resume.
    pub(in crate::daemon) async fn stopped_below<'a>(
        &self,
        caller: &Caller,
        workspace: &'a mut String,
        execution: &str,
    ) -> Result<&'a mut String> {
        let workspace = self
            .workspace_below(
                caller,
                workspace,
                "workspace processes resume agents only in the workspaces below theirs",
            )
            .await?;
        let id = self.workspace(workspace).await?.id;
        let stopper = recovery::stopped_by(&self.paths, &id, execution)?;
        let ours = match stopper {
            Some(stopper) => self.within_subtree(&caller.workspace_id, &stopper).await?,
            None => false,
        };
        ensure!(
            ours,
            "workspace processes resume only the agents that they or the workspaces below them stopped with shoal stop; ask the user to resume this one"
        );
        Ok(workspace)
    }

    /// `workspace`, which must be one below the caller's that nothing
    /// holds, nor any workspace below it: removing it releases their holds,
    /// which are the user's and host apps'.
    pub(in crate::daemon) async fn unheld_workspace_below<'a>(
        &self,
        caller: &Caller,
        workspace: &'a mut String,
        refusal: &str,
    ) -> Result<&'a mut String> {
        let workspace = self.workspace_below(caller, workspace, refusal).await?;
        let id = self.workspace(workspace).await?.id;
        for id in self.subtree(&id).await? {
            let held = self.workspace(&id).await?;
            ensure!(
                held.holds.is_empty(),
                "{} has holds, which are the user's; ask the user to remove it",
                held.name
            );
        }
        Ok(workspace)
    }

    /// Approval request `id`, which must be routed to the caller to decide.
    /// Only the stored routing counts; the decision rechecks it with its grant.
    pub(in crate::daemon) async fn routed_approval(
        &self,
        caller: &Caller,
        id: i64,
    ) -> Result<ApprovalRequest> {
        let request = self
            .approval_request(id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("unknown approval request {id}"))?;
        ensure!(
            request.decider.as_deref() == Some(caller.workspace_id.as_str()),
            "workspace processes decide only the approval requests routed to them; ask the user to run this command"
        );
        Ok(request)
    }

    /// The chats `caller` may see: none for a workspace process.
    pub(in crate::daemon) async fn visible_chats(
        &self,
        caller: Option<&Caller>,
    ) -> Result<Vec<Chat>> {
        match caller {
            Some(_) => Ok(Vec::new()),
            None => self.chats().await,
        }
    }

    /// The agent queue `caller` may see: the entries for the workspaces
    /// below its own, where it may queue agents.
    pub(in crate::daemon) async fn visible_queue(
        &self,
        caller: Option<&Caller>,
    ) -> Result<Vec<QueueEntry>> {
        let mut entries = self.queue_entries().await?;
        if let Some(caller) = caller {
            let subtree = self.subtree(&caller.workspace_id).await?;
            entries.retain(|entry| {
                entry.workspace().is_some_and(|workspace| {
                    workspace != caller.workspace_id && subtree.iter().any(|id| id == workspace)
                })
            });
        }
        Ok(entries)
    }

    /// The workspace `request` starts an agent in, which must be one below
    /// the caller's.
    pub(in crate::daemon) async fn queue_target<'a>(
        &self,
        caller: &Caller,
        request: &'a mut QueueRequest,
    ) -> Result<&'a mut String> {
        const REFUSAL: &str =
            "workspace processes can start agents only in the workspaces split from theirs";
        let target = request.workspace.as_mut().context(REFUSAL)?;
        self.workspace_below(caller, target, REFUSAL).await
    }

    /// Refuse a `branch` other than `workspace`'s own.
    pub(in crate::daemon) async fn own_branch(
        &self,
        workspace: &str,
        branch: Option<&str>,
    ) -> Result<()> {
        let own = self.workspace(workspace).await?.branch;
        ensure!(
            branch.is_none_or(|branch| branch == own),
            "workspace processes start CI runs only on their workspace's branch"
        );
        Ok(())
    }

    /// Question `id` for the caller to answer, which must be one of a
    /// workspace below its own. The answer binds to the question returned,
    /// `shown` when given, and refuses it once a later question reuses its
    /// number.
    pub(in crate::daemon) async fn answerable_question(
        &self,
        caller: &Caller,
        id: i64,
        shown: Option<Question>,
    ) -> Result<Question> {
        let owner = &caller.workspace_id;
        let question = match shown {
            Some(shown) => shown,
            None => self.question(id).await?,
        };
        ensure!(
            question.id == id
                && question.workspace_id != *owner
                && self.within_subtree(owner, &question.workspace_id).await?,
            "answering questions is the user's, or a coordinating agent's for the workspaces below it; workspace processes ask with shoal questions ask"
        );
        Ok(question)
    }

    /// The caller's own repository, which `repository` must select.
    pub(in crate::daemon) async fn own_repository(
        &self,
        caller: &Caller,
        repository: &Selector,
    ) -> Result<Selector> {
        let own = self.workspace(&caller.workspace_id).await?.repository_id;
        ensure!(
            self.repository(repository).await?.id == own,
            "workspace processes can only sync their own repository"
        );
        Ok(own.into())
    }
}
