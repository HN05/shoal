//! Issues and PRs a scoped caller proposed instead of opening them. The user
//! approves a proposal, with any edits, and the daemon opens it; a decided
//! proposal is no longer stored.
use crate::client::repository_ids::RepositoryId;
use crate::client::space_ids::SpaceId;
use crate::client::space_names::SpaceName;
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, Transaction, params};
use std::path::Path;

use super::{
    ApprovalRequest, DecisionStatus, Specification, Target,
    store::{
        COLUMNS, OWNING_SPACE, by_key, current, decode_record, list, record_row, retire,
        write_record,
    },
};
pub(crate) use crate::client::approvals::{
    Duplicate, Finding, ISSUE, IssueSpecification, PR, ProposalDecision, PullSpecification,
    PushSpecification,
};
use crate::daemon::{item_numbers, links::space::require_ready_space, store};
use crate::forge::items::attach::{self, Attachment};

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
    request.space = Some(space_record(tx, &request.space_id)?.0);
    request.key = uuid::Uuid::new_v4().to_string();
    request.number = item_numbers::next(tx, &request.space_id)?;
    // A repository-owned request names no space's row, so it stays when the
    // space goes, and keeps its space's number from another space.
    let space = request.repository_id.is_none().then_some(&request.space_id);
    tx.execute(
        &format!(
            "INSERT INTO access_requests(id,workspace_id,chat_id,repository_id,target_key,name,
                    record,space_number,number)
                VALUES (?1,{OWNING_SPACE},?3,?4,?5,?6,?7,?8)"
        ),
        params![
            request.key,
            space,
            request.repository_id,
            request.target,
            request.name,
            serde_json::to_string(&request)?,
            request.number.space,
            request.number.item
        ],
    )?;
    crate::daemon::events::record_approval(
        tx,
        crate::daemon::events::EventKind::ApprovalRequested,
        &request,
    )?;
    Ok(request)
}

/// The name and repository of the space `id`, a workspace or a chat, which
/// may have none.
fn space_record(db: &Connection, id: &SpaceId) -> Result<(SpaceName, Option<RepositoryId>)> {
    Ok(db.query_row(
        "SELECT name,repository_id FROM spaces WHERE id=?1",
        [id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?)
}

/// The issue proposals of a repository, oldest first.
fn repository_proposals(
    db: &Connection,
    repository: &RepositoryId,
) -> Result<Vec<ApprovalRequest>> {
    db.prepare(&format!(
        "{} WHERE repository_id=?1 AND target_key=?2 ORDER BY rowid",
        COLUMNS
    ))?
    .query_map(params![repository, Target::IssueOpen], record_row)?
    .map(|row| decode_record(row?))
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
pub fn push_pending(db: &Connection, workspace: &SpaceId) -> Result<bool> {
    Ok(list(db, Some(workspace))?
        .iter()
        .any(|r| r.target == Target::Push && r.active && r.status == DecisionStatus::Pending))
}

/// Whether the workspace's PR proposal waits for the user, which keeps the
/// workspace from unattended removal.
pub fn pull_pending(db: &Connection, workspace: &SpaceId) -> Result<bool> {
    Ok(current(db, workspace, &Target::PrOpen, PR)?
        .is_some_and(|request| request.status == DecisionStatus::Pending))
}

/// Point the pending proposals' copies below `source`, a repository
/// directory moved to `destination`, at their new place, in their files and
/// the descriptions that show them, in the caller's transaction.
pub fn rebase_files(tx: &Transaction<'_>, source: &Path, destination: &Path) -> Result<()> {
    let pending = list(tx, None)?
        .into_iter()
        .filter(|request| request.status == DecisionStatus::Pending);
    for mut request in pending {
        let mut moved = false;
        for (body, attachments) in shown_files(&mut request.specification) {
            moved |= rebase(body, attachments, source, destination);
        }
        if moved {
            write_record(tx, &request)?;
        }
    }
    Ok(())
}

/// Each description a proposal posts with the files it shows: an issue's
/// and its duplicates' findings, or a PR's.
fn shown_files(specification: &mut Specification) -> Vec<(&mut String, &mut Vec<Attachment>)> {
    match specification {
        Specification::Issue(issue) => {
            let findings = (issue.duplicates.iter_mut())
                .map(|finding| (&mut finding.body, &mut finding.attachments));
            std::iter::once((&mut issue.issue.body, &mut issue.issue.attachments))
                .chain(findings)
                .collect()
        }
        Specification::Pull(pull) => vec![(&mut pull.pull.body, &mut pull.pull.attachments)],
        _ => Vec::new(),
    }
}

/// Point `attachments` below `source` and the references `body` makes to
/// them below `destination` instead; whether any was.
fn rebase(
    body: &mut String,
    attachments: &mut [Attachment],
    source: &Path,
    destination: &Path,
) -> bool {
    let moved: Vec<_> = attachments
        .iter()
        .map(|attachment| Attachment {
            path: match attachment.path.strip_prefix(source) {
                Ok(inside) => destination.join(inside),
                Err(_) => attachment.path.clone(),
            },
            alt: attachment.alt.clone(),
        })
        .collect();
    if moved == attachments {
        return false;
    }
    *body = attach::retarget(body, attachments, &moved);
    attachments.clone_from_slice(&moved);
    true
}

#[cfg(test)]
mod tests;
