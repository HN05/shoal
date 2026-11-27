//! The requests the gate decides.
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, Transaction, params};
use shoal_protocol::approvals::GateState;

use crate::events::{self, EventKind};

use super::{
    ApprovalRequest, DecisionStatus, GateStep, proposals, row,
    store::{COLUMNS, by_key, close},
};

/// Hand the pending, unrouted request stored under `key` to the gate; false
/// when it is no longer one to hand over.
pub fn give_gate(tx: &Transaction<'_>, key: &str) -> Result<bool> {
    let Some(mut request) = by_key(tx, key)? else {
        return Ok(false);
    };
    if request.status != DecisionStatus::Pending || request.routed || request.gate.is_some() {
        return Ok(request.gate == Some(GateStep::Deciding));
    }
    request.gate = Some(GateStep::Deciding);
    row::write(tx, &request)?;
    Ok(true)
}

/// The pending requests the gate is deciding, in the order they were made.
pub fn deciding(db: &Connection) -> Result<Vec<ApprovalRequest>> {
    pending_at(db, GateState::Deciding, "")
}

/// The pending requests the gate allowed whose approval never started a
/// push, as when the daemon stopped between the two, in the order they
/// were made.
pub fn stranded(db: &Connection) -> Result<Vec<ApprovalRequest>> {
    pending_at(db, GateState::Allowed, "AND operation IS NULL")
}

/// The pending requests at the gate's step `gate` that `filter` keeps, in
/// the order they were made.
fn pending_at(db: &Connection, gate: GateState, filter: &str) -> Result<Vec<ApprovalRequest>> {
    db.prepare(&format!(
        "{COLUMNS} WHERE status=?1 AND gate=?2 {filter} ORDER BY rowid"
    ))?
    .query_map(params![DecisionStatus::Pending, gate], row::read)?
    .map(|row| row::decode(row?))
    .collect()
}

/// The request stored under `key` while the gate still decides it.
fn with_gate(tx: &Transaction<'_>, key: &str) -> Result<ApprovalRequest> {
    let request = by_key(tx, key)?.context("the approval request no longer exists")?;
    ensure!(
        request.status == DecisionStatus::Pending && request.gate == Some(GateStep::Deciding),
        "approval request {} is no longer the gate's to decide",
        request.number
    );
    Ok(request)
}

/// Record that the gate allowed the request stored under `key`, which its
/// approval then carries out.
pub fn allow(tx: &Transaction<'_>, key: &str) -> Result<ApprovalRequest> {
    let mut request = with_gate(tx, key)?;
    request.gate = Some(GateStep::Allowed);
    row::write(tx, &request)?;
    Ok(request)
}

/// Record that the gate could not judge the request stored under `key`, for
/// `reason`, so it takes the normal route.
pub fn unjudged(tx: &Transaction<'_>, key: &str, reason: &str) -> Result<ApprovalRequest> {
    let mut request = with_gate(tx, key)?;
    request.gate = Some(GateStep::Unjudged {
        reason: reason.to_owned(),
    });
    row::write(tx, &request)?;
    Ok(request)
}

/// Record that the allow of the pending request stored under `key` could not
/// be carried out, for `error`, so it takes the normal route and a restart
/// never carries it out again; `None` once it is no longer pending.
pub fn unapplied(tx: &Transaction<'_>, key: &str, error: &str) -> Result<Option<ApprovalRequest>> {
    let Some(mut request) = by_key(tx, key)? else {
        return Ok(None);
    };
    if request.status != DecisionStatus::Pending {
        return Ok(None);
    }
    request.gate = Some(GateStep::Unapplied {
        error: error.to_owned(),
    });
    row::write(tx, &request)?;
    Ok(Some(request))
}

/// Record the gate's denial of the request stored under `key`: a proposal
/// goes as a decided one does, and any other request stays as denied but
/// no longer current, so asking again makes a new request.
pub fn deny(tx: &Transaction<'_>, key: &str) -> Result<ApprovalRequest> {
    let request = with_gate(tx, key)?;
    let denied = ApprovalRequest {
        status: DecisionStatus::Denied,
        decided_at: Some(crate::time::UnixTime::now()),
        active: false,
        ..request
    };
    match denied.target.is_proposal() {
        true => proposals::remove(tx, &denied.key)?,
        false => {
            close(tx, denied.clone(), DecisionStatus::Denied)?;
        }
    }
    events::record_approval(tx, EventKind::ApprovalDecided, &denied)?;
    Ok(denied)
}

/// Record that the gate passed the request stored under `key` on, with its
/// reason and the case it quoted, for the normal route to take.
pub fn escalate(
    tx: &Transaction<'_>,
    key: &str,
    reason: &str,
    rule: &str,
) -> Result<ApprovalRequest> {
    let mut request = with_gate(tx, key)?;
    request.gate = Some(GateStep::Escalated {
        reason: reason.to_owned(),
        rule: rule.to_owned(),
    });
    row::write(tx, &request)?;
    Ok(request)
}
