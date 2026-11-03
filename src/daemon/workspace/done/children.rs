//! The workspaces below a completed space: an ancestor's completion with
//! cleanup takes them whatever their own work, while it stands.
use anyhow::Result;

use super::super::Manager;
use crate::{
    daemon::{events::EventCause, notifications::NotificationKind},
    model::{Chat, Workspace},
};

/// A space above a workspace whose completion with cleanup stands.
#[derive(Debug, Clone)]
#[allow(
    clippy::large_enum_variant,
    reason = "a lookup's result, read once by the sweep that made it"
)]
pub(crate) enum CompletedAncestor {
    Workspace(Workspace),
    Chat(Chat),
}

impl CompletedAncestor {
    pub(crate) fn id(&self) -> &str {
        match self {
            Self::Workspace(workspace) => &workspace.id,
            Self::Chat(chat) => &chat.id,
        }
    }

    pub(crate) fn name(&self) -> &str {
        match self {
            Self::Workspace(workspace) => &workspace.name,
            Self::Chat(chat) => &chat.name,
        }
    }
}

impl Manager {
    /// The nearest ancestor of `workspace` whose completion with cleanup
    /// stands, which takes the workspace: a workspace it was split from, or
    /// the chat that opened the topmost of them.
    pub(crate) async fn completed_ancestor(
        &self,
        workspace: &Workspace,
    ) -> Result<Option<CompletedAncestor>> {
        let mut current = workspace.clone();
        while let Some(reference) = current.parent_workspace.clone() {
            let Some(ancestor) = self.find_workspace(&reference.id).await? else {
                return Ok(None);
            };
            let completed = CompletedAncestor::Workspace(ancestor.clone());
            if self.completion_stands(&completed).await? {
                return Ok(Some(completed));
            }
            current = ancestor;
        }
        let Some(chat) = current.chat.as_deref() else {
            return Ok(None);
        };
        let Some(chat) = self.recorded_chat(chat).await? else {
            return Ok(None);
        };
        let completed = CompletedAncestor::Chat(chat);
        Ok(self
            .completion_stands(&completed)
            .await?
            .then_some(completed))
    }

    /// Whether `ancestor` is marked done with cleanup, a chat as recorded
    /// now, a workspace at its current HEAD. A HEAD that cannot be read, as
    /// off its branch or in a directory whose file access stalled, is not
    /// that HEAD. Only a recorded stall counts: the candidate checks of a
    /// workspace below run this under [`Manager::check_access`] already,
    /// which a second limit would cut short, and branch observation, earlier
    /// in each sweep, records a stall of `workspace`'s directory.
    pub(in crate::daemon::workspace) async fn completion_stands(
        &self,
        ancestor: &CompletedAncestor,
    ) -> Result<bool> {
        let workspace = match ancestor {
            CompletedAncestor::Workspace(workspace) => workspace,
            CompletedAncestor::Chat(chat) => {
                let recorded = self.recorded_chat(&chat.id).await?;
                return Ok(recorded
                    .and_then(|chat| chat.completion)
                    .is_some_and(|completion| completion.cleanup));
            }
        };
        let Some(completion) = self.completion(&workspace.id).await? else {
            return Ok(false);
        };
        if self.file_access.stalled(&workspace.path).is_some() {
            return Ok(false);
        }
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
        ancestor: &CompletedAncestor,
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
                    &format!("removed after {} was marked done", ancestor.name()),
                ),
            ),
            Err(error) => {
                self.record_retained(&workspace.id, EventCause::ParentCompletion, &error)
                    .await?;
                (
                    NotificationKind::CleanupFailed,
                    format!(
                        "cleanup after {} was marked done retained the workspace: {error:#}",
                        ancestor.name()
                    ),
                )
            }
        };
        self.notify(Some(&workspace.name), kind, message).await;
        Ok(())
    }
}
