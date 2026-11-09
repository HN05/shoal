//! What a workspace branch's local merge check reports to its watch, and
//! how it reads as an observation.
use anyhow::Result;

use super::items::linked_items;
use crate::{
    client::space_ids::SpaceId,
    forge::{
        items::link::{ItemKind, Selection},
        updates::{MergeCheck, Snapshot},
    },
    model::ConflictCheck,
};

/// Conflicted paths named in a branch conflict message; `shoal conflicts` lists all.
const MAX_LISTED_FILES: usize = 10;

/// What a workspace branch's local check reports: every conflict, or only
/// sensitive files changed on both sides while a linked PR's forge reports
/// its conflicts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum BranchReport {
    Conflicts,
    ChangedOnBothSides,
}

/// What the branch of the space `id` reports: the forge reports the
/// conflicts of an assigned PR, never of one another space owns.
pub(super) fn branch_report(db: &rusqlite::Connection, id: &SpaceId) -> Result<BranchReport> {
    Ok(if linked_items(db, id, Some(ItemKind::Pr))?.is_empty() {
        BranchReport::Conflicts
    } else {
        BranchReport::ChangedOnBothSides
    })
}

/// A workspace branch's local merge check as `report` keeps it, observed
/// like PR mergeability.
pub(super) fn branch_conflicts(check: Result<ConflictCheck>, report: BranchReport) -> Snapshot {
    Snapshot::merge_check(check.map(|check| MergeCheck {
        conflict: conflict_message(&check, report),
        head: check.head,
        base: check.target_commit,
    }))
}

/// The conflicts `report` keeps of the check, naming the first paths of
/// each kind; `None` when there are none.
fn conflict_message(check: &ConflictCheck, report: BranchReport) -> Option<String> {
    let conflicts = (report == BranchReport::Conflicts && check.conflicts)
        .then(|| format!("Conflicts with {}: {}", check.target, listed(&check.files)));
    let changed = &check.changed_on_both_sides;
    let both = (!changed.is_empty()).then(|| {
        let target = &check.target;
        format!("Changed on both sides with {target}: {}", listed(changed))
    });
    let parts: Vec<String> = conflicts.into_iter().chain(both).collect();
    (!parts.is_empty()).then(|| parts.join("; "))
}

/// The first of `files`, and how many more there are.
fn listed(files: &[String]) -> String {
    let mut listed = files[..files.len().min(MAX_LISTED_FILES)].join(", ");
    if files.len() > MAX_LISTED_FILES {
        listed.push_str(&format!(" and {} more", files.len() - MAX_LISTED_FILES));
    }
    listed
}

/// Whether a wait also reports the workspace branch's local conflicts with
/// its base, as [`branch_report`] decides: only an unfiltered wait.
pub(super) fn watches_branch(selection: &Selection) -> bool {
    selection.input.is_none() && selection.kind.is_none()
}
