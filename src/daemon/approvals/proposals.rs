//! Issues and PRs a scoped caller proposed instead of opening them. The user
//! approves a proposal, with any edits, and the daemon opens it; a decided
//! proposal is no longer stored.
use anyhow::{Result, ensure};
use rusqlite::{Connection, Transaction, params};
use serde::{Deserialize, Serialize};

use super::{
    ApprovalRequest, DecisionStatus, Specification, Target, by_key, current, record_row, retire,
};
use crate::{
    daemon::store,
    forge::{
        items::attach::Attachment,
        items::create::{NewIssue, NewPull},
    },
};

/// The name of a workspace's single PR proposal.
pub const PR: &str = "branch";
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
    /// Findings of proposals the user marked as duplicates of this one,
    /// posted as comments once it is opened.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub duplicates: Vec<Finding>,
}

/// A duplicate proposal's text and files, which become a comment on what it
/// duplicates.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Finding {
    pub body: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<Attachment>,
}

/// The PR `shoal pr open` would open, bound to the branch commit the user
/// approves; a newer commit outdates it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PullSpecification {
    pub pull: NewPull,
    pub commit: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
}

/// What the user decides about a proposal besides yes or no.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProposalDecision {
    /// Replaces the proposed title on approval.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Replaces the proposed description on approval.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    /// Tells the agent why its proposal was denied.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Denies an issue proposal as a duplicate and adds its finding there.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duplicate_of: Option<Duplicate>,
}

/// What a denied issue proposal duplicates.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Duplicate {
    /// An issue number or URL in the same repository.
    Issue(String),
    /// Another pending issue proposal, by its number.
    Approval(i64),
}

impl ProposalDecision {
    /// Refuse details that do not apply to this decision of `target`.
    pub fn validate(&self, target: &Target, approve: bool) -> Result<()> {
        if approve {
            ensure!(
                self.reason.is_none() && self.duplicate_of.is_none(),
                "a reason or duplicate applies only when denying"
            );
            ensure!(
                self.title
                    .as_ref()
                    .is_none_or(|title| !title.trim().is_empty()),
                "the title cannot be empty"
            );
        } else {
            ensure!(
                self.title.is_none() && self.body.is_none(),
                "a title or body applies only when approving"
            );
        }
        ensure!(
            self.duplicate_of.is_none() || *target == Target::IssueOpen,
            "only an issue proposal can be a duplicate"
        );
        crate::validate::reason("denial", self.reason.as_deref())
    }
}

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

/// Record the workspace's PR proposal. A pending one for the same branch and
/// commit is returned with the user's edits; any other is outdated.
pub fn propose_pull(tx: &Transaction<'_>, request: ApprovalRequest) -> Result<Recorded> {
    store::require_ready(tx, &request.workspace_id)?;
    if let Some(existing) = current(tx, &request.workspace_id, &Target::PrOpen, PR)? {
        let source = |r: &ApprovalRequest| {
            r.proposed_pull()
                .map(|p| (p.pull.head.clone(), p.commit.clone()))
        };
        if existing.status == DecisionStatus::Pending && source(&existing) == source(&request) {
            return Ok((existing, false));
        }
        retire(tx, &existing)?;
    }
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

/// Store the pending proposal's changed record, such as the user's edits or
/// a duplicate's finding.
pub fn save(tx: &Transaction<'_>, request: &ApprovalRequest) -> Result<()> {
    let stored = by_key(tx, &request.key)?;
    ensure!(
        stored.is_some_and(|stored| stored.status == DecisionStatus::Pending),
        "approval request {} is no longer pending",
        request.id
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

/// Whether the workspace's PR proposal waits for the user, which keeps the
/// workspace from unattended removal.
pub fn pull_pending(db: &Connection, workspace: &str) -> Result<bool> {
    Ok(current(db, workspace, &Target::PrOpen, PR)?
        .is_some_and(|request| request.status == DecisionStatus::Pending))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decisions_take_only_the_details_that_apply() {
        let edit = ProposalDecision {
            title: Some("Fix".into()),
            body: Some(String::new()),
            ..ProposalDecision::default()
        };
        let duplicate = ProposalDecision {
            reason: Some("Known".into()),
            duplicate_of: Some(Duplicate::Issue("12".into())),
            ..ProposalDecision::default()
        };
        assert!(edit.validate(&Target::PrOpen, true).is_ok());
        assert!(edit.validate(&Target::IssueOpen, false).is_err());
        assert!(duplicate.validate(&Target::IssueOpen, false).is_ok());
        assert!(duplicate.validate(&Target::IssueOpen, true).is_err());
        assert!(duplicate.validate(&Target::PrOpen, false).is_err());
        let blank = ProposalDecision {
            title: Some(" ".into()),
            ..ProposalDecision::default()
        };
        assert!(blank.validate(&Target::IssueOpen, true).is_err());
    }
}
