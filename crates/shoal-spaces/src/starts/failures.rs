//! Telling a start's caller of its failed preparation, naming `shoal setup`,
//! which retries it, and what removes the workspace.
use crate::Spaces;
use crate::daemon::Daemon;
use std::sync::Arc;

use shoal_ids::space_ids::SpaceId;

use crate::{
    protocol::operations::{Ended, Failure},
    {
        log,
        notifications::{About, AlertKind},
    },
};

impl Spaces {
    /// Tells the coordinator, the agent of the nearest space above with one,
    /// or else the user. A preparation that waits is told of once it starts.
    pub(super) fn report_preparation_failure(
        &self,
        daemon: &Arc<dyn Daemon>,
        id: &SpaceId,
        operation: &str,
    ) {
        let (daemon, id, operation) = (daemon.clone(), id.clone(), operation.to_owned());
        tokio::spawn(async move {
            if let Some(Ended::Failed(failure)) = daemon.spaces().operations.ended(&operation).await
            {
                (daemon.spaces())
                    .tell_preparation_failure(&*daemon, &id, &failure)
                    .await;
            }
        });
    }

    /// Does nothing once the workspace is gone.
    pub(super) async fn tell_preparation_failure(
        &self,
        daemon: &dyn Daemon,
        id: &SpaceId,
        failure: &Failure,
    ) {
        let Ok(workspace) = self.workspace_by_id(id).await else {
            return;
        };
        let (name, number) = (&workspace.name, workspace.number);
        let reason = (failure.message().map(str::to_owned))
            .or_else(|| workspace.error.clone())
            .unwrap_or_else(|| format!("{:?}", failure.code()));
        let message = format!(
            "The preparation of {name} failed: {reason}; `shoal setup {number}` retries it, and `shoal rm {number}` removes the workspace"
        );
        let about = About::space(name, id);
        let kind = AlertKind::PreparationFailed;
        if let Err(error) = daemon
            .coordination()
            .record_notification(daemon, about, kind, message)
            .await
        {
            log!("preparation failure of {name} not reported: {error:#}");
        }
    }
}
