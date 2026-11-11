//! Removing a workspace in the background: the request returns once the
//! workspace is reserved and passes removal's first checks, and whoever
//! asked learns of a failure later.
use std::sync::Arc;

use anyhow::Result;

use super::Removal;
use crate::client::space_ids::SpaceId;
use crate::client::space_names::SpaceSelector;
use crate::{
    daemon::{
        log,
        notifications::{About, NotificationKind},
        workspace::Manager,
    },
    model::Workspace,
    removal::{InspectionPolicy, UnsavedWork},
};

impl Manager {
    /// Start removing the workspace `selector` names at a request, as
    /// [`Self::remove_workspace`] removes it, or as
    /// [`Self::remove_unheld_workspace`] does without `release_holds`.
    /// Returns the workspace once it is reserved and passes the checks
    /// removal makes before changing anything; the rest goes on in the
    /// background, which a daemon update waits for. A failure from then on
    /// retains the workspace and reaches the agent of `requester`, or else
    /// the user.
    pub(crate) async fn start_workspace_removal(
        self: &Arc<Self>,
        selector: &SpaceSelector,
        unsaved: UnsavedWork,
        release_holds: bool,
        requester: Option<SpaceId>,
    ) -> Result<Workspace> {
        let operation = self.gates.owned_operation().await;
        let removal = manual(&unsaved, release_holds);
        let reserved = self.reserve_removal(selector, removal).await?;
        self.check_reserved_removal(&reserved, removal).await?;
        let workspace = reserved.workspace.clone();
        let manager = self.clone();
        tokio::spawn(async move {
            let _operation = operation;
            let workspace = reserved.workspace.clone();
            let removal = manual(&unsaved, release_holds);
            if let Err(error) = manager.remove_reserved(reserved, removal).await {
                manager
                    .report_failed_removal(&workspace, requester.as_ref(), &error)
                    .await;
            }
        });
        Ok(workspace)
    }

    /// Tell the agent of `requester`, or else the user, that removing
    /// `workspace` failed with `error` and the workspace stays.
    async fn report_failed_removal(
        &self,
        workspace: &Workspace,
        requester: Option<&SpaceId>,
        error: &anyhow::Error,
    ) {
        let message = format!(
            "Could not remove {}: {error:#}; it stays, and shoal rm {} tries again",
            workspace.name, workspace.name
        );
        if let Some(requester) = requester {
            match (self.queue_space_message(requester, None, message.clone(), Vec::new())).await {
                Ok(()) => return,
                Err(error) => log!("removal failure not sent to {requester}: {error:#}"),
            }
        }
        self.notify_about(
            About::space(&workspace.name, &workspace.id),
            NotificationKind::RemovalFailed,
            message,
        )
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
