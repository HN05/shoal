//! The requests the gate decides: handing one to it, those it is deciding,
//! and recording that it allowed one, denied one, which a retry asks again
//! after, or passed one on, which then takes the normal route.
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, Transaction};

use crate::daemon::events::{self, EventKind};

use super::{
    ApprovalRequest, DecisionStatus, GateStep, proposals,
    store::{COLUMNS, by_key, close, decode_record, record_row, write_record},
};

/// Hand the pending, unrouted request stored under `key` to the gate; false
/// when it is no longer one to hand over.
pub(crate) fn give_gate(tx: &Transaction<'_>, key: &str) -> Result<bool> {
    let Some(mut request) = by_key(tx, key)? else {
        return Ok(false);
    };
    if request.status != DecisionStatus::Pending || request.routed || request.gate.is_some() {
        return Ok(request.gate == Some(GateStep::Deciding));
    }
    request.gate = Some(GateStep::Deciding);
    write_record(tx, &request)?;
    Ok(true)
}

/// The pending requests the gate is deciding, in the order they were made;
/// a malformed record never keeps the gate from the others.
pub(crate) fn deciding(db: &Connection) -> Result<Vec<ApprovalRequest>> {
    selected(db, DECIDING)
}

/// The pending requests the gate allowed whose approval never started a
/// push, as when the daemon stopped between the two, in the order they
/// were made.
pub(crate) fn stranded(db: &Connection) -> Result<Vec<ApprovalRequest>> {
    selected(db, STRANDED)
}

/// The requests `filter` selects, in the order they were made; a record
/// that does not decode is logged and left out.
fn selected(db: &Connection, filter: &str) -> Result<Vec<ApprovalRequest>> {
    let rows = db
        .prepare(&format!("{COLUMNS} WHERE {filter} ORDER BY rowid"))?
        .query_map([], record_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows
        .into_iter()
        .filter_map(|row| match decode_record(row) {
            Ok(request) => Some(request),
            Err(error) => {
                crate::daemon::log!("a request for the gate: {error:#}");
                None
            }
        })
        .collect())
}

/// Whether a stored request is pending with the gate; a malformed record
/// stays out of the selection.
const DECIDING: &str = "CASE WHEN json_valid(record) THEN
    json_extract(record, '$.status')='pending'
    AND json_extract(record, '$.gate.state')='deciding' END";

/// Whether a stored request is pending after the gate allowed it, with no
/// approved push started; a malformed record stays out of the selection.
const STRANDED: &str = "CASE WHEN json_valid(record) THEN
    json_extract(record, '$.status')='pending'
    AND json_extract(record, '$.gate.state')='allowed'
    AND json_extract(record, '$.operation') IS NULL END";

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
pub(crate) fn allow(tx: &Transaction<'_>, key: &str) -> Result<ApprovalRequest> {
    let mut request = with_gate(tx, key)?;
    request.gate = Some(GateStep::Allowed);
    write_record(tx, &request)?;
    Ok(request)
}

/// Record that the gate could not judge the request stored under `key`, for
/// `reason`, so it takes the normal route.
pub(crate) fn unjudged(tx: &Transaction<'_>, key: &str, reason: &str) -> Result<ApprovalRequest> {
    let mut request = with_gate(tx, key)?;
    request.gate = Some(GateStep::Unjudged {
        reason: reason.to_owned(),
    });
    write_record(tx, &request)?;
    Ok(request)
}

/// Record that the allow of the pending request stored under `key` could not
/// be carried out, for `error`, so it takes the normal route and a restart
/// never carries it out again; `None` once it is no longer pending.
pub(crate) fn unapplied(
    tx: &Transaction<'_>,
    key: &str,
    error: &str,
) -> Result<Option<ApprovalRequest>> {
    let Some(mut request) = by_key(tx, key)? else {
        return Ok(None);
    };
    if request.status != DecisionStatus::Pending {
        return Ok(None);
    }
    request.gate = Some(GateStep::Unapplied {
        error: error.to_owned(),
    });
    write_record(tx, &request)?;
    Ok(Some(request))
}

/// Record the gate's denial of the request stored under `key`: a proposal
/// goes as a decided one does, and any other request stays as denied but
/// no longer current, so asking again makes a new request.
pub(crate) fn deny(tx: &Transaction<'_>, key: &str) -> Result<ApprovalRequest> {
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
pub(crate) fn escalate(
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
    write_record(tx, &request)?;
    Ok(request)
}
