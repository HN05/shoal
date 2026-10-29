//! The workspaces below a completed one: an ancestor's completion with
//! cleanup takes them whatever their own work, while it stands.
use anyhow::Result;

use super::super::Manager;
use crate::{
    daemon::{events::EventCause, notifications::NotificationKind},
    model::Workspace,
};

impl Manager {
    /// The nearest ancestor of `workspace` whose completion with cleanup
    /// stands, which takes the workspace.
    pub(crate) async fn completed_ancestor(
        &self,
        workspace: &Workspace,
    ) -> Result<Option<Workspace>> {
        let mut parent = workspace.parent_workspace.clone();
        while let Some(reference) = parent {
            let Some(ancestor) = self.find_workspace(&reference.id).await? else {
                return Ok(None);
            };
            if self.completion_stands(&ancestor).await? {
                return Ok(Some(ancestor));
            }
            parent = ancestor.parent_workspace;
        }
        Ok(None)
    }

    /// Whether `workspace` is marked done with cleanup at its current HEAD.
    /// A HEAD that cannot be read, as off its branch, is not that HEAD.
    pub(in crate::daemon::workspace) async fn completion_stands(
        &self,
        workspace: &Workspace,
    ) -> Result<bool> {
        let Some(completion) = self.completion(&workspace.id).await? else {
            return Ok(false);
        };
        Ok(completion.cleanup
            && crate::daemon::links::pr::current_head(workspace)
                .await
                .is_ok_and(|head| head == completion.head))
    }

    /// Remove `workspace` for its completed `ancestor` once its grace period
    /// ends, unless something keeps it or workspaces below it remain, which
    /// go first under the same completion.
    pub(super) async fn sweep_under_completed(
        &self,
        workspace: &Workspace,
        ancestor: &Workspace,
    ) -> Result<()> {
        if !workspace.child_workspaces.is_empty()
            || self.keep_reason(&workspace.id).await?.is_some()
        {
            return Ok(());
        }
        let (kind, message) = match self.remove_under_completed(&workspace.id, ancestor).await {
            Ok(false) => return Ok(()),
            Ok(true) => (
                NotificationKind::WorkspaceRemoved,
                super::super::removal_notice(
                    &workspace.name,
                    &format!("removed after {} was marked done", ancestor.name),
                ),
            ),
            Err(error) => {
                self.record_retained(&workspace.id, EventCause::ParentCompletion, &error)
                    .await?;
                (
                    NotificationKind::CleanupFailed,
                    format!(
                        "cleanup after {} was marked done retained the workspace: {error:#}",
                        ancestor.name
                    ),
                )
            }
        };
        self.notify(Some(&workspace.name), kind, message).await;
        Ok(())
    }
}
