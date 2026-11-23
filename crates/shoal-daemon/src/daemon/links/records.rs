//! What links read of a space in their own transactions: whether it takes
//! link changes, its place in the space tree and its checked branch.
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, Transaction};
use shoal_ids::space_ids::SpaceId;
use shoal_links::daemon::{ProposalKind, Records};
use shoal_protocol::model::{ChangedOnBothSides, SpaceRef};

use crate::daemon::{
    agent_service::AgentRecords,
    alerts::AlertKind,
    approvals::{self, ApprovalRequest, proposals},
    events::{self, EventKind},
    item_numbers::ItemNumber,
    messages,
    tree::{self, TrackedAgents},
    workspace::record_base_changed,
};
use crate::model::EventCause;

/// The space records links read, which the daemon keeps.
pub(crate) struct LinkRecords;

impl Records for LinkRecords {
    fn ensure_accepts_links(&self, db: &Connection, id: &SpaceId) -> Result<()> {
        super::ensure_accepts_links(db, id)
    }

    fn accepts_links(&self, db: &Connection, id: &SpaceId) -> Result<bool> {
        super::accepts_links(db, id)
    }

    fn forget_ready_marks(&self, db: &Connection, id: &SpaceId, urls: &[String]) -> Result<()> {
        crate::daemon::ready_marks::forget(db, id, urls)
    }

    fn require_ready_space(&self, db: &Connection, id: &SpaceId) -> Result<()> {
        super::space::require_ready_space(db, id)
    }

    fn delegated(
        &self,
        db: &Connection,
        id: &SpaceId,
        agents: &[SpaceId],
    ) -> Result<Vec<SpaceRef>> {
        tree::delegated(db, id, &TrackedAgents::of(agents))
    }

    fn receiving_paths(
        &self,
        db: &Connection,
        id: &SpaceId,
        agents: &[SpaceId],
    ) -> Result<Vec<Vec<SpaceId>>> {
        tree::receiving_paths(db, id, &TrackedAgents::of(agents))
    }

    fn delegated_elsewhere(
        &self,
        db: &Connection,
        agents: &[SpaceId],
    ) -> Result<Vec<(SpaceId, Vec<SpaceRef>)>> {
        tree::delegated_elsewhere(db, &TrackedAgents::of(agents))
    }

    fn changed_on_both_sides(&self, db: &Connection) -> Result<Vec<(SpaceId, ChangedOnBothSides)>> {
        crate::daemon::workspace::conflicts::recorded::listing_files(db)
    }

    fn record_proposal(
        &self,
        tx: &Transaction<'_>,
        request: ApprovalRequest,
        kind: ProposalKind,
    ) -> Result<(ApprovalRequest, bool)> {
        match kind {
            ProposalKind::Issue => proposals::propose_issue(tx, request),
            ProposalKind::Pull => proposals::propose_pull(tx, request),
            ProposalKind::Push => proposals::propose_push(tx, request),
        }
    }

    fn save_proposal(&self, tx: &Transaction<'_>, request: &ApprovalRequest) -> Result<()> {
        proposals::save(tx, request)
    }

    fn forget_proposal(&self, tx: &Transaction<'_>, request: &ApprovalRequest) -> Result<()> {
        events::record_approval(tx, EventKind::ApprovalDecided, request)?;
        proposals::remove(tx, &request.key)
    }

    fn retire_approval(&self, tx: &Transaction<'_>, request: &ApprovalRequest) -> Result<()> {
        approvals::retire(tx, request)
    }

    fn approval_by_number(
        &self,
        db: &Connection,
        number: ItemNumber,
    ) -> Result<Option<ApprovalRequest>> {
        approvals::by_number(db, number)
    }

    fn approval_by_key(&self, db: &Connection, key: &str) -> Result<Option<ApprovalRequest>> {
        approvals::by_key(db, key)
    }

    fn approval_requests(
        &self,
        db: &Connection,
        space: Option<&SpaceId>,
    ) -> Result<Vec<ApprovalRequest>> {
        approvals::list_of_space(db, space)
    }

    fn builds_on(&self, db: &Connection, base: &SpaceId, id: &SpaceId) -> Result<bool> {
        Ok(db.query_row(
            "WITH RECURSIVE up(id) AS (SELECT ?1
                UNION SELECT workspaces.base_workspace_id FROM workspaces JOIN up USING(id)
                    WHERE workspaces.base_workspace_id IS NOT NULL)
             SELECT EXISTS(SELECT 1 FROM up WHERE id=?2)",
            rusqlite::params![base, id],
            |row| row.get(0),
        )?)
    }

    fn change_base(
        &self,
        tx: &Transaction<'_>,
        id: &SpaceId,
        base: &SpaceId,
        new_base: Option<&SpaceId>,
        cause: EventCause,
    ) -> Result<bool> {
        let changed = tx.execute(
            "UPDATE workspaces SET base_workspace_id=?3 WHERE id=?1 AND base_workspace_id=?2",
            rusqlite::params![id, base, new_base],
        )?;
        if changed > 0 {
            record_base_changed(tx, id, cause)?;
        }
        Ok(changed > 0)
    }

    fn queue_shoal_message(
        &self,
        tx: &Transaction<'_>,
        id: &SpaceId,
        kind: AlertKind,
        message: &str,
    ) -> Result<()> {
        messages::queue_shoal_message(tx, &AgentRecords, id, kind, message).map(drop)
    }

    fn checked_branch(&self, db: &Connection, id: &SpaceId) -> Result<Option<String>> {
        checked_branch(db, id)
    }
}

/// The branch of the workspace `id` while its conflicts are checked: a
/// chat, a workspace at a commit and one whose assignment is done have none.
fn checked_branch(db: &rusqlite::Connection, id: &SpaceId) -> Result<Option<String>> {
    Ok(db
        .query_row(
            "SELECT branch FROM workspaces WHERE id=?1
             AND NOT EXISTS (SELECT 1 FROM space_completion WHERE space_id=?1)",
            [id],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()?
        .flatten())
}
