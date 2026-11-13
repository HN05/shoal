//! Removing a workspace in the background: the request returns once the
//! workspace is reserved and passes removal's first checks, and whoever
//! asked learns of a failure later.
use std::sync::Arc;

use anyhow::{Context as _, Result};
use tokio::sync::oneshot;

use super::Removal;
use crate::client::space_ids::SpaceId;
use crate::client::space_names::SpaceSelector;
use crate::{
    daemon::{
        agent_messages, log,
        notifications::{About, NotificationKind},
        scope::Caller,
        workspace::Manager,
    },
    model::Workspace,
    removal::{InspectionPolicy, UnsavedWork},
};

impl Manager {
    /// Start removing the workspace `selector` names at `caller`'s request,
    /// as [`Self::remove_workspace`] removes it for the user, or as
    /// [`Self::remove_unheld_workspace`] does for a scoped caller.
    /// Returns the workspace once it is reserved and passes the checks
    /// removal makes before changing anything; the rest goes on in the
    /// background, which a daemon update waits for. A failure from then on
    /// retains the workspace and reaches the agent of the caller's space,
    /// or else the user.
    pub(crate) async fn start_workspace_removal(
        self: &Arc<Self>,
        selector: &SpaceSelector,
        unsaved: UnsavedWork,
        caller: Option<&Caller>,
    ) -> Result<Workspace> {
        let operation = self.gates.owned_operation().await;
        let id = self.workspace(selector).await?.id;
        let (checked, passed) = oneshot::channel();
        let manager = self.clone();
        let release_holds = caller.is_none();
        let requester = caller.map(|caller| caller.space_id.clone());
        // The reservation lives in the task, which reports the first checks'
        // outcome and goes on after them.
        tokio::spawn(async move {
            let _operation = operation;
            let removal = manual(&unsaved, release_holds);
            let mut checked = Some(checked);
            let mut workspace = None;
            let removed = manager.remove_space_checked(&id, removal, |space| {
                workspace = Some(space.worktree().context("no longer a workspace")?.clone());
                if let (Some(checked), Some(workspace)) = (checked.take(), &workspace) {
                    let _ = checked.send(Ok(workspace.clone()));
                }
                Ok(())
            });
            let Err(error) = removed.await else {
                return;
            };
            match (checked, workspace) {
                (Some(checked), _) => {
                    let _ = checked.send(Err(error));
                }
                (None, Some(workspace)) => {
                    manager
                        .report_failed_removal(&workspace, requester.as_ref(), &error)
                        .await;
                }
                (None, None) => log!("removal of {id} failed: {error:#}"),
            }
        });
        passed
            .await
            .context("the removal ended before its checks")?
    }

    /// Tell the agent of `requester`, or else the user, that removing
    /// `workspace` failed with `error` and the workspace stays. A requesting
    /// chat removed before it reads the message hands it to the user.
    async fn report_failed_removal(
        &self,
        workspace: &Workspace,
        requester: Option<&SpaceId>,
        error: &anyhow::Error,
    ) {
        let kind = NotificationKind::RemovalFailed;
        let message = format!(
            "removal failed: {error:#}; the workspace stays, and shoal rm {} tries again",
            workspace.number
        );
        if let Some(requester) = requester {
            let about = agent_messages::About {
                name: workspace.name.clone(),
                id: Some(workspace.id.clone()),
                kind: Some(kind),
            };
            let queued =
                self.queue_space_message(requester, Some(about), message.clone(), Vec::new());
            match queued.await {
                Ok(()) => return,
                Err(error) => log!("removal failure not sent to {requester}: {error:#}"),
            }
        }
        self.notify_about(About::space(&workspace.name, &workspace.id), kind, message)
            .await;
    }
}

/// A removal at a request that deletes only the `unsaved` work the caller
/// confirmed.
fn manual(unsaved: &UnsavedWork, release_holds: bool) -> Removal<'_> {
    Removal::Manual {
        unsaved,
        inspection: InspectionPolicy::GitOnly,
        release_holds,
        taking: None,
    }
}

#[cfg(test)]
mod tests;
