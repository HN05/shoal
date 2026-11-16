//! What removing a workspace would discard, whether the caller lets it, and
//! what the removal did.
use anyhow::{Context as _, Result};
use shoal_protocol::removal::{RemovalCheck, RemovalResult, UnsavedWork};

use crate::connection::{Daemon, pages, request};
use shoal_protocol::space_names::SpaceSelector;
use shoal_protocol::{
    cleanup::{ManualCleanup, Selection},
    model::{RemovedSpace, RestoredSpace},
    protocol::RemovalMethod,
};

/// What removing `workspace` would discard, with its changes when
/// `include_changes`.
pub async fn check(
    daemon: &impl Daemon,
    workspace: SpaceSelector,
    include_changes: bool,
) -> Result<RemovalCheck> {
    let method = RemovalMethod::CheckRemoval {
        workspace,
        include_changes,
    };
    request(daemon, method).await
}

/// Remove `workspace`, deleting only the `unsaved` work the caller saw.
pub async fn remove(
    daemon: &impl Daemon,
    workspace: SpaceSelector,
    unsaved: UnsavedWork,
) -> Result<RemovalResult> {
    request(
        daemon,
        RemovalMethod::RemoveWorkspace { workspace, unsaved },
    )
    .await
}

/// Start removing `workspace`, returning once nothing but a change made
/// meanwhile or a step that fails can keep it; a later failure reaches
/// the caller's agent, or else the user's notifications.
pub async fn start(
    daemon: &impl Daemon,
    workspace: SpaceSelector,
    unsaved: UnsavedWork,
) -> Result<()> {
    request(daemon, RemovalMethod::StartRemoval { workspace, unsaved }).await
}

/// Remove what `selection` selects that nothing keeps, or with `dry_run`
/// only list it.
pub async fn clean_up(
    daemon: &impl Daemon,
    dry_run: bool,
    selection: Selection,
) -> Result<ManualCleanup> {
    request(daemon, RemovalMethod::Cleanup { dry_run, selection }).await
}

/// The removed spaces whose records allow restoring them.
pub async fn removed(daemon: &impl Daemon) -> Result<Vec<RemovedSpace>> {
    pages(daemon, RemovalMethod::RemovedSpaces).await
}

/// The removed `space`, refused with the reason when the caller may not
/// restore it.
pub async fn removed_space(daemon: &impl Daemon, space: SpaceSelector) -> Result<RemovedSpace> {
    let removed: Vec<RemovedSpace> = request(daemon, RemovalMethod::RemovedSpace { space }).await?;
    removed.into_iter().next().context("no such removed space")
}

/// Open the removed `space` again from its record.
pub async fn restore(daemon: &impl Daemon, space: SpaceSelector) -> Result<RestoredSpace> {
    request(daemon, RemovalMethod::RestoreSpace { space }).await
}

/// Delete the session files the restore that opened `space` returned,
/// which the caller has put in place.
pub async fn drop_restored_sessions(daemon: &impl Daemon, space: SpaceSelector) -> Result<()> {
    request(daemon, RemovalMethod::DropRestoredSessions { space }).await
}
