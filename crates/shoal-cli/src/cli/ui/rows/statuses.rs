//! What lists read besides a workspace's record to tell its status: whether
//! its work is stopped, and the agents queued to start in it.
use std::collections::HashSet;

use super::{Cell, status_cell, waiting_reasons};
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
            self.entries(workspace).first().copied(),
        )
    }

    /// Every status the row of `workspace` shows: its own, and those of the
    /// agents queued in it.
    pub fn shown(&self, workspace: &Workspace) -> Vec<ListStatus> {
        let mut shown = vec![self.of(workspace)];
        for status in self
            .entries(workspace)
            .into_iter()
            .map(ListStatus::of_entry)
        {
            if !shown.contains(&status) {
                shown.push(status);
            }
        }
        shown
    }

    /// The agents queued to start in `workspace`, in queue order.
    pub(super) fn entries(&self, workspace: &Workspace) -> Vec<&QueueEntry> {
        (self.queued.iter())
            .filter(|entry| entry.workspace() == Some(&workspace.id))
            .collect()
    }

    /// Sort `workspaces` as lists show them: by status, then number.
    pub fn sort(&self, workspaces: &mut [Workspace]) {
        workspaces.sort_by_key(|workspace| (self.of(workspace), workspace.number));
    }
}

/// The cell of `status`, naming the queued agents `entries` with their
/// places in the queue: `queued (2)`, or after a status that wins over the
/// first, such as `process running, queued (2)`.
pub(super) fn place_cell(status: ListStatus, entries: &[&QueueEntry]) -> Cell {
    let (label, style) = status_cell(status);
    let places = (entries.iter().map(|entry| place_label(entry)))
        .collect::<Vec<_>>()
        .join("; ");
    match entries.first() {
        Some(first) if ListStatus::of_entry(first) == status => (places, style),
        Some(_) => (format!("{label}, {places}"), style),
        None => (label, style),
    }
}

/// A queued agent's status with its place in the queue while it waits, and
/// what else it waits for: `queued (2)`, `queued (2), after an update`, or
/// `starting`.
fn place_label(entry: &QueueEntry) -> String {
    let label = ListStatus::of_entry(entry).label();
    let place = match entry.position {
        Some(position) => format!("{label} ({position})"),
        None => label.to_owned(),
    };
    place + &waiting_reasons(entry)
}
