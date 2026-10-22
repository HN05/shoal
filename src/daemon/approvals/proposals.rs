//! Issues and PRs a scoped caller proposed instead of opening them. The user
//! approves a proposal, with any edits, and the daemon opens it; a decided
//! proposal is no longer stored.
use anyhow::Result;
use rusqlite::{Connection, Transaction, params};
use serde::{Deserialize, Serialize};

use super::{ApprovalRequest, DecisionStatus, Specification, Target, record_row};
use crate::{daemon::store, forge::items::create::NewIssue};

/// The name every issue proposal shares; the repository holds any number.
pub const ISSUE: &str = "issue";

/// What `shoal issue open` was told, and where it was proposed from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IssueSpecification {
    pub issue: NewIssue,
    /// Whether approval also assigns the issue to the workspace.
    pub link: bool,
    pub branch: String,
    pub commit: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
}

impl ApprovalRequest {
    /// The proposed issue, for an issue proposal.
    pub fn proposed_issue(&self) -> Option<&IssueSpecification> {
        match &self.specification {
            Specification::Issue(issue) => Some(issue),
            _ => None,
        }
    }
}

/// A stored proposal, and whether this call recorded it.
pub type Recorded = (ApprovalRequest, bool);

/// Record a proposed issue for the workspace's repository, or return the
/// identical one the workspace proposed before and the user has not decided.
pub fn propose_issue(tx: &Transaction<'_>, mut request: ApprovalRequest) -> Result<Recorded> {
    store::require_ready(tx, &request.workspace_id)?;
    let repository: String = tx.query_row(
        "SELECT repository_id FROM workspaces WHERE id=?1",
        [&request.workspace_id],
        |r| r.get(0),
    )?;
    let existing = repository_proposals(tx, &repository)?
        .into_iter()
        .find(|r| {
            r.workspace_id == request.workspace_id
                && r.status == DecisionStatus::Pending
                && r.proposed_issue().map(|i| &i.issue)
                    == request.proposed_issue().map(|i| &i.issue)
        });
    if let Some(existing) = existing {
        return Ok((existing, false));
    }
    request.repository_id = Some(repository);
    Ok((insert(tx, request)?, true))
}

fn insert(tx: &Transaction<'_>, mut request: ApprovalRequest) -> Result<ApprovalRequest> {
    request.workspace = tx.query_row(
        "SELECT name FROM workspaces WHERE id=?1",
        [&request.workspace_id],
        |r| r.get(0),
    )?;
    request.key = uuid::Uuid::new_v4().to_string();
    request.id = store::free_number(tx, "access_requests")?;
    // A repository-owned request names no workspace row, so it stays when
    // the workspace goes.
    let workspace = request
        .repository_id
        .is_none()
        .then_some(&request.workspace_id);
    tx.execute(
        "INSERT INTO access_requests(id,workspace_id,repository_id,target_key,name,record,number)
            VALUES (?1,?2,?3,?4,?5,?6,?7)",
        params![
            request.key,
            workspace,
            request.repository_id,
            request.target,
            request.name,
            serde_json::to_string(&request)?,
            request.id
        ],
    )?;
    Ok(request)
}

/// The issue proposals of a repository, oldest first.
fn repository_proposals(db: &Connection, repository: &str) -> Result<Vec<ApprovalRequest>> {
    db.prepare(&format!(
        "{} WHERE repository_id=?1 AND target_key=?2 ORDER BY rowid",
        super::COLUMNS
    ))?
    .query_map(params![repository, Target::IssueOpen], record_row)?
    .map(|row| super::decode_record(row?))
    .collect()
}

/// Forget a decided proposal; the agent's message carries the outcome.
pub fn remove(tx: &Transaction<'_>, key: &str) -> Result<()> {
    tx.execute("DELETE FROM access_requests WHERE id=?1", [key])?;
    Ok(())
}
