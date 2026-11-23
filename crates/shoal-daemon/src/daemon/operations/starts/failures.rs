//! Telling a start's caller of its failed preparation: the agent of the
//! nearest space above the new workspace with one, or else the user,
//! naming `shoal setup`, which retries the preparation, and what removes
//! the workspace.
use std::sync::Arc;

use shoal_ids::space_ids::SpaceId;

use crate::{
    daemon::{
        log,
        manager::Manager,
        notifications::{About, AlertKind},
        operations::Entry,
    },
    protocol::operations::{Ended, Failure},
};

impl Manager {
    /// Once the preparation of the workspace `id`, which `operation`
    /// prepares, fails, tell its coordinator, the agent of the nearest
    /// space above it with one, or else the user, naming what retries it
    /// and what removes it. One that waits is told of once it starts.
    pub(super) fn report_preparation_failure(self: &Arc<Self>, id: &SpaceId, operation: &str) {
        let Some(entry) = self.operations.get(operation) else {
            return;
        };
        let (manager, id) = (self.clone(), id.clone());
        tokio::spawn(async move {
            if let Some(Ended::Failed(failure)) = ended(&entry).await {
                manager.tell_preparation_failure(&id, &failure).await;
            }
        });
    }

    /// Tell the coordinator of the workspace `id`, or else the user, that its
    /// preparation failed with `failure`, unless it is gone.
    pub(super) async fn tell_preparation_failure(&self, id: &SpaceId, failure: &Failure) {
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
        if let Err(error) = self
            .coordination
            .record_notification(self, about, kind, message)
            .await
        {
            log!("preparation failure of {name} not reported: {error:#}");
        }
    }
}

/// How the operation `entry` ended, once it did; `None` when its record goes
/// first.
async fn ended(entry: &Entry) -> Option<Ended> {
    let mut changed = entry.watch();
    loop {
        if let (_, Some(ended)) = entry.events_from(u64::MAX) {
            return Some(ended);
        }
        changed.changed().await.ok()?;
    }
}
