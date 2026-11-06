//! Issues and PRs a scoped caller proposed instead of opening them. The user
//! approves a proposal, with any edits, and the daemon opens it; a decided
//! proposal is no longer stored.
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, Transaction, params};

use super::{
    ApprovalRequest, DecisionStatus, Specification, Target, by_key, current, record_row, retire,
};
pub(crate) use crate::client::approvals::{
    Duplicate, Finding, ISSUE, IssueSpecification, PR, ProposalDecision, PullSpecification,
    PushSpecification,
};
use crate::{
    daemon::{item_numbers, links::space::require_ready_space, store},
    forge::{items::action::Edit, items::attach::Attachment},
};

impl ApprovalRequest {
    /// The proposed issue, for an issue proposal.
    pub fn proposed_issue(&self) -> Option<&IssueSpecification> {
        match &self.specification {
            Specification::Issue(issue) => Some(issue),
            _ => None,
        }
    }

    /// The proposed PR, for a PR proposal.
    pub fn proposed_pull(&self) -> Option<&PullSpecification> {
        match &self.specification {
            Specification::Pull(pull) => Some(pull),
            _ => None,
        }
    }

    /// The push approval runs: a push request's, or a PR proposal's that
    /// pushes the branch first.
    pub fn pending_push(&self) -> Option<&PushSpecification> {
        match &self.specification {
            Specification::Push(push) => Some(push),
            Specification::Pull(pull) => pull.push.as_ref(),
            _ => None,
        }
    }

    /// The files the proposal attaches, its duplicates' included.
    pub fn attachments(&self) -> Vec<&Attachment> {
        match &self.specification {
            Specification::Issue(issue) => issue
                .issue
                .attachments
                .iter()
                .chain(issue.duplicates.iter().flat_map(|d| &d.attachments))
                .collect(),
            Specification::Pull(pull) => pull.pull.attachments.iter().collect(),
            _ => Vec::new(),
        }
    }

    /// The findings of duplicates an issue proposal has yet to post.
    pub fn findings(&self) -> &[Finding] {
        self.proposed_issue()
            .map_or(&[], |issue| issue.duplicates.as_slice())
    }

    /// What is left to do to an item approval posted.
    pub fn follow_up(&self) -> Option<&Edit> {
        match &self.specification {
            Specification::Issue(issue) => issue.follow_up.as_ref(),
            Specification::Pull(pull) => pull.follow_up.as_ref(),
            _ => None,
        }
    }

    pub fn set_follow_up(&mut self, follow_up: Option<Edit>) {
        match &mut self.specification {
            Specification::Issue(issue) => issue.follow_up = follow_up,
            Specification::Pull(pull) => pull.follow_up = follow_up,
            _ => {}
        }
    }

    /// Forget the first finding once it is posted.
    pub fn finding_posted(&mut self) {
        if let Specification::Issue(issue) = &mut self.specification
            && !issue.duplicates.is_empty()
        {
            issue.duplicates.remove(0);
        }
    }

    /// Apply the user's edits to the proposed title and description.
    pub fn edit(&mut self, decision: &ProposalDecision) {
        let (title, body) = match &mut self.specification {
            Specification::Issue(issue) => (&mut issue.issue.title, &mut issue.issue.body),
            Specification::Pull(pull) => (&mut pull.pull.title, &mut pull.pull.body),
            _ => return,
        };
        if let Some(edited) = &decision.title {
            edited.clone_into(title);
        }
        if let Some(edited) = &decision.body {
            edited.clone_into(body);
        }
    }
}

/// A stored proposal, and whether this call recorded it.
pub type Recorded = (ApprovalRequest, bool);

/// Record a proposed issue for the repository it names, else the space's,
/// or return the identical one the space proposed before and the user has
/// not decided.
pub fn propose_issue(tx: &Transaction<'_>, mut request: ApprovalRequest) -> Result<Recorded> {
    require_ready_space(tx, &request.space_id)?;
    let repository = match request.repository_id.clone() {
        Some(repository) => repository,
        None => (space_record(tx, &request.space_id)?.1)
            .context("a space without a repository names the repository of its proposal")?,
    };
    let existing = repository_proposals(tx, &repository)?
        .into_iter()
        .find(|r| {
            r.space_id == request.space_id
                && r.status == DecisionStatus::Pending
                && r.proposed_issue()
                    .map(|i| (&i.issue, &i.repository, i.link))
                    == request
                        .proposed_issue()
                        .map(|i| (&i.issue, &i.repository, i.link))
        });
    if let Some(existing) = existing {
        return Ok((existing, false));
    }
    request.repository_id = Some(repository);
    Ok((insert(tx, request)?, true))
}

/// Record the workspace's PR proposal. A pending one for the same branch,
/// commit and head repository is returned with the user's edits; any other
/// is outdated.
pub fn propose_pull(tx: &Transaction<'_>, request: ApprovalRequest) -> Result<Recorded> {
    store::require_ready(tx, &request.space_id)?;
    if let Some(existing) = current(tx, &request.space_id, &Target::PrOpen, PR)? {
        let source = |r: &ApprovalRequest| {
            r.proposed_pull().map(|p| {
                let pull = &p.pull;
                (
                    pull.head.clone(),
                    pull.head_repository.clone(),
                    p.commit.clone(),
                    p.push.is_some(),
                )
            })
        };
        if existing.status == DecisionStatus::Pending && source(&existing) == source(&request) {
            return Ok((existing, false));
        }
        retire(tx, &existing)?;
    }
    // Approving the PR pushes the branch, so a separate request to push it
    // is no longer needed.
    if let Some(push) = request.pending_push()
        && let Some(separate) = current(tx, &request.space_id, &Target::Push, &push.request_name())?
    {
        retire(tx, &separate)?;
    }
    Ok((insert(tx, request)?, true))
}

/// Record the workspace's push of one ref. A pending request for the same
/// push is returned, as is a pending PR proposal that pushes it; any other
/// request for the ref is outdated.
pub fn propose_push(tx: &Transaction<'_>, request: ApprovalRequest) -> Result<Recorded> {
    store::require_ready(tx, &request.space_id)?;
    let push = request
        .pending_push()
        .context("the push request has no push")?;
    let same = |r: &ApprovalRequest| {
        r.status == DecisionStatus::Pending
            && r.pending_push()
                .is_some_and(|other| other.is_same_push(push))
    };
    if let Some(pull) = current(tx, &request.space_id, &Target::PrOpen, PR)?.filter(same) {
        return Ok((pull, false));
    }
    if let Some(existing) = current(tx, &request.space_id, &Target::Push, &request.name)? {
        if same(&existing) {
            return Ok((existing, false));
        }
        retire(tx, &existing)?;
    }
    Ok((insert(tx, request)?, true))
}

fn insert(tx: &Transaction<'_>, mut request: ApprovalRequest) -> Result<ApprovalRequest> {
    (request.space, _) = space_record(tx, &request.space_id)?;
    request.key = uuid::Uuid::new_v4().to_string();
    request.number = item_numbers::next(tx, &request.space_id)?;
    // A repository-owned request names no workspace row, so it stays when
    // the space goes, and keeps its space's number from another space.
    let workspace = request.repository_id.is_none().then_some(&request.space_id);
    tx.execute(
        "INSERT INTO access_requests(id,workspace_id,repository_id,target_key,name,record,
                space_number,number)
            VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
        params![
            request.key,
            workspace,
            request.repository_id,
            request.target,
            request.name,
            serde_json::to_string(&request)?,
            request.number.space,
            request.number.item
        ],
    )?;
    Ok(request)
}

/// The name and repository of the space `id`, a workspace or a chat, which
/// may have none.
fn space_record(db: &Connection, id: &str) -> Result<(String, Option<String>)> {
    Ok(db.query_row(
        "SELECT name,repository_id FROM workspaces WHERE id=?1
            UNION ALL SELECT name,repository_id FROM chats WHERE id=?1",
        [id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?)
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

/// Store the pending proposal's changed record, such as the user's edits or
/// a duplicate's finding.
pub fn save(tx: &Transaction<'_>, request: &ApprovalRequest) -> Result<()> {
    let stored = by_key(tx, &request.key)?;
    ensure!(
        stored.is_some_and(|stored| stored.status == DecisionStatus::Pending),
        "approval request {} is no longer pending",
        request.number
    );
    tx.execute(
        "UPDATE access_requests SET record=?2 WHERE id=?1",
        params![request.key, serde_json::to_string(request)?],
    )?;
    Ok(())
}

/// Forget a decided proposal; the agent's message carries the outcome.
pub fn remove(tx: &Transaction<'_>, key: &str) -> Result<()> {
    tx.execute("DELETE FROM access_requests WHERE id=?1", [key])?;
    Ok(())
}

/// Whether a push request of the workspace waits for the user, which keeps
/// the workspace from unattended removal.
pub fn push_pending(db: &Connection, workspace: &str) -> Result<bool> {
    Ok(super::list(db, Some(workspace))?
        .iter()
        .any(|r| r.target == Target::Push && r.active && r.status == DecisionStatus::Pending))
}

/// Whether the workspace's PR proposal waits for the user, which keeps the
/// workspace from unattended removal.
pub fn pull_pending(db: &Connection, workspace: &str) -> Result<bool> {
    Ok(current(db, workspace, &Target::PrOpen, PR)?
        .is_some_and(|request| request.status == DecisionStatus::Pending))
}

#[cfg(test)]
mod tests;
