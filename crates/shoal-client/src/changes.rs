//! Agents' changes: the hunks Shoal records as the agents it runs change
//! the files of their worktrees, who made each and the thread it is for.
use anyhow::Result;
use shoal_protocol::{
    changes::{Change, ChangeDiff, ChangeNumber, ToolChange},
    protocol::{ChangeFilter, ChangeMethod},
    space_names::SpaceSelector,
    threads::ThreadNumber,
};

use crate::connection::{Daemon, pages, request};

/// Record what a tool call of the caller's agent changed, as its hook
/// reports it; the changes Shoal could not tell were the agent's.
pub async fn record(daemon: &impl Daemon, change: ToolChange) -> Result<Vec<Change>> {
    request(daemon, ChangeMethod::RecordChange { change }).await
}

/// The changes `filter` keeps, in the order they were made.
pub async fn list(daemon: &impl Daemon, filter: ChangeFilter) -> Result<Vec<Change>> {
    request(daemon, ChangeMethod::ListChanges { filter }).await
}

/// The changes `filter` keeps, with the lines each removed and added.
pub async fn diffs(daemon: &impl Daemon, filter: ChangeFilter) -> Result<Vec<ChangeDiff>> {
    request(daemon, ChangeMethod::ChangeDiffs { filter }).await
}

/// Say the changes `changes`, bare numbers of `space` or else the caller's
/// own, are its agent's, and with `thread` for that thread.
pub async fn claim(
    daemon: &impl Daemon,
    space: Option<SpaceSelector>,
    changes: Vec<ChangeNumber>,
    thread: Option<ThreadNumber>,
) -> Result<Vec<Change>> {
    let method = ChangeMethod::ClaimChanges {
        space,
        changes,
        thread,
    };
    request(daemon, method).await
}

/// Say the changes `changes` are not its agent's.
pub async fn unclaim(
    daemon: &impl Daemon,
    space: Option<SpaceSelector>,
    changes: Vec<ChangeNumber>,
) -> Result<Vec<Change>> {
    request(daemon, ChangeMethod::UnclaimChanges { space, changes }).await
}

/// Tell Shoal the user edited or saved the files `paths`, by absolute
/// path, as an editor sees them, so a change an agent's command made to
/// one while it ran counts as mixed.
pub async fn report_edits(daemon: &impl Daemon, paths: Vec<std::path::PathBuf>) -> Result<()> {
    request(daemon, ChangeMethod::ReportEdits { paths }).await
}
