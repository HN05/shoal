//! What lists read besides a workspace's record to tell its status: whether
//! its work is stopped, and the agent queued to start in it.
use std::collections::HashSet;

use super::{Cell, status_cell};
use crate::cli::list_status::ListStatus;
use crate::client::space_ids::SpaceId;
use crate::model::{QueueEntry, Workspace};

#[derive(Default)]
pub struct Statuses {
    /// The workspaces whose work `shoal stop` saved.
    pub stopped: HashSet<SpaceId>,
    /// The agents queued to start, in a workspace or in a new one.
    pub queued: Vec<QueueEntry>,
}

impl Statuses {
    /// The status lists show for `workspace`.
    pub fn of(&self, workspace: &Workspace) -> ListStatus {
        ListStatus::of_listed_workspace(
            workspace,
            self.stopped.contains(&workspace.id),
            self.entry(workspace),
        )
    }

    /// The agent queued to start in `workspace`.
    pub(super) fn entry(&self, workspace: &Workspace) -> Option<&QueueEntry> {
        (self.queued.iter()).find(|entry| entry.workspace() == Some(&workspace.id))
    }

    /// Sort `workspaces` as lists show them: by status, then number.
    pub fn sort(&self, workspaces: &mut [Workspace]) {
        workspaces.sort_by_key(|workspace| (self.of(workspace), workspace.number));
    }
}

/// The cell of `status`, naming the place of the queued agent `entry` in
/// the queue: `queued (2)`.
pub(super) fn place_cell(status: ListStatus, entry: Option<&QueueEntry>) -> Cell {
    let (label, style) = status_cell(status);
    let place = entry.and_then(|entry| entry.position);
    match place.filter(|_| status == ListStatus::Queued) {
        Some(position) => (format!("{label} ({position})"), style),
        None => (label, style),
    }
}
