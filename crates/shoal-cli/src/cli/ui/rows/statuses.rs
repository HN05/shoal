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

    /// Every status the row of `workspace` shows: its own, and its queued
    /// agent's when its own wins over it.
    pub fn shown(&self, workspace: &Workspace) -> Vec<ListStatus> {
        let status = self.of(workspace);
        let queued = self.entry(workspace).map(ListStatus::of_entry);
        std::iter::once(status)
            .chain(queued.filter(|queued| *queued != status))
            .collect()
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

/// The cell of `status`, naming the queued agent `entry` with its place in
/// the queue: `queued (2)`, or after a status that wins over it, such as
/// `process running, queued (2)`.
pub(super) fn place_cell(status: ListStatus, entry: Option<&QueueEntry>) -> Cell {
    let (label, style) = status_cell(status);
    match entry {
        Some(entry) if ListStatus::of_entry(entry) == status => (place_label(entry), style),
        Some(entry) => (format!("{label}, {}", place_label(entry)), style),
        None => (label, style),
    }
}

/// A queued agent's status with its place in the queue while it waits:
/// `queued (2)`, or `starting`.
fn place_label(entry: &QueueEntry) -> String {
    let label = ListStatus::of_entry(entry).label();
    match entry.position {
        Some(position) => format!("{label} ({position})"),
        None => label.to_owned(),
    }
}
