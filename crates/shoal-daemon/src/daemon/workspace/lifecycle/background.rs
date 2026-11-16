//! Removing a space in the background: the request returns once the space
//! is reserved and passes removal's first checks, and whoever asked learns
//! of a failure later.
use std::sync::Arc;

use anyhow::{Context as _, Result};
use tokio::sync::oneshot;

use super::Removal;
use crate::{
    daemon::{
        agent_messages, log,
        notifications::{About, NotificationKind},
        scope::Caller,
        workspace::Manager,
    },
    model::Space,
    removal::{InspectionPolicy, UnsavedWork},
};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::space_names::SpaceSelector;

impl Manager {
    /// Start removing the space `selector` names at `caller`'s request, as
    /// [`Self::remove_selected`] removes it for the user, or as
    /// [`Self::remove_unheld`] does for a scoped caller. Returns the space
    /// once it is reserved and passes the checks removal makes before
    /// changing anything; the rest goes on in the background, which a daemon
    /// update waits for. A failure from then on retains the space and
    /// reaches the agent of the caller's space, or else the user.
    pub(crate) async fn start_removal(
        self: &Arc<Self>,
        selector: &SpaceSelector,
        unsaved: UnsavedWork,
        caller: Option<&Caller>,
    ) -> Result<Space> {
        let operation = self.gates.owned_operation().await;
        let id = self.space(selector).await?.id;
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
            let mut reserved = None;
            let removed = manager.remove_space_checked(&id, removal, |space| {
                if let Some(checked) = checked.take() {
                    let _ = checked.send(Ok(space.clone()));
                }
                reserved = Some(space.clone());
                Ok(())
            });
            let Err(error) = removed.await else {
                return;
            };
            match (checked, reserved) {
                (Some(checked), _) => {
                    let _ = checked.send(Err(error));
                }
                (None, Some(space)) => {
                    manager
                        .report_failed_removal(&space, requester.as_ref(), &error)
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
    /// `space` failed with `error` and the space stays. A requesting chat
    /// removed before it reads the message hands it to the user.
    async fn report_failed_removal(
        &self,
        space: &Space,
        requester: Option<&SpaceId>,
        error: &anyhow::Error,
    ) {
        let kind = NotificationKind::RemovalFailed;
        let message = format!(
            "removal failed: {error:#}; the {} stays, and shoal rm {} tries again",
            space.kind(),
            space.number
        );
        if let Some(requester) = requester {
            let about = agent_messages::About {
                name: space.name.clone(),
                id: Some(space.id.clone()),
                kind: Some(kind),
            };
            let queued =
                self.queue_space_message(requester, Some(about), message.clone(), Vec::new());
            match queued.await {
                Ok(()) => return,
                Err(error) => log!("removal failure not sent to {requester}: {error:#}"),
            }
        }
        self.notify_about(About::space(&space.name, &space.id), kind, message)
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
