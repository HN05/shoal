//! Persist issue associations.
use shoal_ids::space_ids::SpaceId;

use anyhow::Result;
use rusqlite::Connection;

pub(crate) mod agent_settles;
mod agents;
mod assignees;

use crate::forge::items::reference::ItemRef;
pub(crate) use crate::model::{Assigner, IssueAssignee};
pub(crate) use assignees::AssignerOf;
use shoal_links::issue::{
    agents::{self as agent_records},
    assignees::{self as assignee_records, Dropped},
};

/// The user's assignees the space `id` stands for on every issue it links
/// as assigned, which its removal drops.
pub(crate) fn removed_assignees(db: &Connection, id: &SpaceId) -> Result<Vec<Dropped>> {
    assignee_records::dropped(db, id, &shoal_links::linked::assigned_urls(db, id)?)
}

/// The issues the space `id` links as assigned whose agent account its
/// removal settles: all but those it handed to the machine it moved to.
pub(crate) fn removed_issues(db: &Connection, id: &SpaceId) -> Result<Vec<ItemRef>> {
    agent_records::settled_here(db, id)
}

/// Record that an agent of the space `id` ended, within the caller's
/// transaction, so the issues an agent assigned it no longer count as
/// worked without one.
pub(crate) fn agent_ended(db: &Connection, id: &SpaceId) -> Result<()> {
    agent_records::agent_ended(db, id)
}

#[cfg(test)]
pub(in crate::daemon::links) mod test_support;
#[cfg(test)]
mod tests;
