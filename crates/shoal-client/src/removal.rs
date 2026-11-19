//! What removing a space would discard, whether the caller lets it, and
//! what the removal did.
use anyhow::{Context as _, Result};
use shoal_protocol::removal::{RemovalResult, SpaceRemovalCheck, UnsavedWork};

use crate::connection::{Daemon, pages, request};
use shoal_protocol::space_names::SpaceSelector;
use shoal_protocol::{
    absorb::{Absorb, Absorbed},
    cleanup::{ManualCleanup, Selection},
    model::{RemovedSpace, RestoredSpace},
    protocol::RemovalMethod,
};

/// What removing the workspace or chat `space` would do, with its
/// worktree's changes when `include_changes`.
pub async fn check(
    daemon: &impl Daemon,
    space: SpaceSelector,
    include_changes: bool,
) -> Result<SpaceRemovalCheck> {
    let method = RemovalMethod::CheckRemoval {
        space,
        include_changes,
    };
    let check: Box<SpaceRemovalCheck> = request(daemon, method).await?;
    Ok(*check)
}

/// Remove the workspace or chat `space`, stopping its agent, deleting only
/// the `unsaved` work the caller saw.
pub async fn remove(
    daemon: &impl Daemon,
    space: SpaceSelector,
    unsaved: UnsavedWork,
) -> Result<RemovalResult> {
    let method = RemovalMethod::RemoveSpace {
        space,
        unsaved,
        keep_user_holds: false,
    };
    request(daemon, method).await
}

/// Start removing `space`, returning once nothing but a change made
/// meanwhile or a step that fails can keep it; a later failure reaches
/// the caller's agent, or else the user's notifications.
pub async fn start(daemon: &impl Daemon, space: SpaceSelector, unsaved: UnsavedWork) -> Result<()> {
    request(daemon, RemovalMethod::StartRemoval { space, unsaved }).await
}

/// Absorb the space `absorb` names into the space that takes its work,
/// returning once its work and records moved; its removal goes on in the
/// background.
pub async fn absorb(daemon: &impl Daemon, absorb: Absorb) -> Result<Absorbed> {
    let absorbed: Box<Absorbed> =
        request(daemon, RemovalMethod::AbsorbSpace(Box::new(absorb))).await?;
    Ok(*absorbed)
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
