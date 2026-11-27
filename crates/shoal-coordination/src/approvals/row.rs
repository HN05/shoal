//! A request's row of `access_requests`: the columns that queries read,
//! and the record that holds the rest of the request.
use anyhow::{Context, Result, bail};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::{approvals::GateState, config::repo::Lifetime, space_names::SpaceName};

use super::{ApprovalRequest, DecisionStatus, GateStep, Specification, Target};
use crate::{forge::items::reference::ItemRef, item_numbers::ItemNumber, time::UnixTime};

/// The query of the stored requests that `$filter` keeps, which [`read`]
/// reads: each request's columns, and whether the space of a request its
/// repository owns is gone, which only a proposed issue outlives.
macro_rules! select {
    ($($filter:literal)?) => {
        concat!(
            "SELECT id,space_id,proposer_id,repository_id,target_key,name,status,decider,gate,
                operation,record,active,space_number,number,
                repository_id IS NOT NULL AND NOT EXISTS (SELECT 1 FROM spaces
                    WHERE spaces.number=access_requests.space_number) AS space_removed
            FROM access_requests "$(, $filter)?
        )
    };
}
pub(super) use select;

/// A stored request's row, before its record is decoded.
pub struct Row {
    key: String,
    space_id: Option<SpaceId>,
    proposer_id: Option<SpaceId>,
    repository_id: Option<shoal_ids::repository_ids::RepositoryId>,
    target: Target,
    name: String,
    status: DecisionStatus,
    decider: Option<SpaceId>,
    gate: Option<GateState>,
    operation: Option<String>,
    record: String,
    active: bool,
    number: ItemNumber,
    space_removed: bool,
}

pub fn read(row: &rusqlite::Row<'_>) -> rusqlite::Result<Row> {
    Ok(Row {
        key: row.get("id")?,
        space_id: row.get("space_id")?,
        proposer_id: row.get("proposer_id")?,
        repository_id: row.get("repository_id")?,
        target: row.get("target_key")?,
        name: row.get("name")?,
        status: row.get("status")?,
        decider: row.get("decider")?,
        gate: row.get("gate")?,
        operation: row.get("operation")?,
        record: row.get("record")?,
        active: row.get("active")?,
        number: ItemNumber {
            space: row.get("space_number")?,
            item: row.get("number")?,
        },
        space_removed: row.get("space_removed")?,
    })
}

/// What a request's record holds: each field no column of its row holds.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    space: Option<SpaceName>,
    specification: Specification,
    lifetime: Lifetime,
    reason: String,
    created_at: UnixTime,
    decided_at: Option<UnixTime>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    routed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    url: Option<ItemRef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    warnings: Vec<String>,
    #[serde(default, skip_serializing_if = "GateDetails::is_empty")]
    gate: GateDetails,
}

/// What the gate's step says beside its state, which the `gate` column
/// holds.
#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct GateDetails {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    rule: Option<String>,
}

impl GateDetails {
    fn is_empty(&self) -> bool {
        matches!(
            self,
            Self {
                error: None,
                reason: None,
                rule: None
            }
        )
    }

    fn of(step: &GateStep) -> Self {
        match step.clone() {
            GateStep::Deciding | GateStep::Allowed => Self::default(),
            GateStep::Unapplied { error } => Self {
                error: Some(error),
                ..Self::default()
            },
            GateStep::Unjudged { reason } => Self {
                reason: Some(reason),
                ..Self::default()
            },
            GateStep::Escalated { reason, rule } => Self {
                reason: Some(reason),
                rule: Some(rule),
                ..Self::default()
            },
        }
    }

    /// The step at `state` that says this, refused when the state takes
    /// other details.
    fn step(self, state: GateState) -> Result<GateStep> {
        Ok(match (state, self) {
            (GateState::Deciding, details) if details.is_empty() => GateStep::Deciding,
            (GateState::Allowed, details) if details.is_empty() => GateStep::Allowed,
            (
                GateState::Unapplied,
                Self {
                    error: Some(error),
                    reason: None,
                    rule: None,
                },
            ) => GateStep::Unapplied { error },
            (
                GateState::Unjudged,
                Self {
                    error: None,
                    reason: Some(reason),
                    rule: None,
                },
            ) => GateStep::Unjudged { reason },
            (
                GateState::Escalated,
                Self {
                    error: None,
                    reason: Some(reason),
                    rule: Some(rule),
                },
            ) => GateStep::Escalated { reason, rule },
            (state, _) => bail!("the gate's step {state} does not take the details recorded"),
        })
    }
}

/// The request `row` holds.
pub fn decode(row: Row) -> Result<ApprovalRequest> {
    let number = row.number;
    let invalid = || format!("approval request {number} has an invalid record");
    let Record {
        space,
        specification,
        lifetime,
        reason,
        created_at,
        decided_at,
        routed,
        url,
        warnings,
        gate,
    } = serde_json::from_str(&row.record).with_context(invalid)?;
    let gate = match row.gate {
        Some(state) => Some(gate.step(state).with_context(invalid)?),
        None if gate.is_empty() => None,
        None => bail!("approval request {number} records the details of no gate's step"),
    };
    Ok(ApprovalRequest {
        number,
        key: row.key,
        space_id: (row.space_id.or(row.proposer_id))
            .with_context(|| format!("approval request {number} names no space"))?,
        space,
        space_removed: row.space_removed,
        repository_id: row.repository_id,
        target: row.target,
        name: row.name,
        specification,
        lifetime,
        reason,
        status: row.status,
        created_at,
        decided_at,
        active: row.active,
        routed,
        decider: row.decider,
        url,
        warnings,
        operation: row.operation,
        gate,
    })
}

/// The record of `request`: each field no column of its row holds.
fn record(request: &ApprovalRequest) -> Result<String> {
    // Destructured whole, so that a new field is placed in a column or the
    // record.
    let ApprovalRequest {
        number: _,
        key: _,
        space_id: _,
        space,
        space_removed: _,
        repository_id: _,
        target: _,
        name: _,
        specification,
        lifetime,
        reason,
        status: _,
        created_at,
        decided_at,
        active: _,
        routed,
        decider: _,
        url,
        warnings,
        operation: _,
        gate,
    } = request;
    Ok(serde_json::to_string(&Record {
        space: space.clone(),
        specification: specification.clone(),
        lifetime: *lifetime,
        reason: reason.clone(),
        created_at: *created_at,
        decided_at: *decided_at,
        routed: *routed,
        url: url.clone(),
        warnings: warnings.clone(),
        gate: gate.as_ref().map(GateDetails::of).unwrap_or_default(),
    })?)
}

/// Record the new `request`. One its repository owns names its space as
/// the proposer instead of the owner, so it stays when the space goes, and
/// keeps its space's number from another space.
pub fn insert(db: &Connection, request: &ApprovalRequest) -> Result<()> {
    let (owner, proposer) = match request.repository_id {
        Some(_) => (None, Some(&request.space_id)),
        None => (Some(&request.space_id), None),
    };
    db.execute(
        "INSERT INTO access_requests(id,space_id,proposer_id,repository_id,target_key,name,
                status,decider,gate,operation,record,active,space_number,number)
            VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)",
        params![
            request.key,
            owner,
            proposer,
            request.repository_id,
            request.target,
            request.name,
            request.status,
            request.decider,
            request.gate.as_ref().map(GateStep::state),
            request.operation,
            record(request)?,
            request.active,
            request.number.space,
            request.number.item
        ],
    )?;
    Ok(())
}

/// Store what `request` says now in place of the row under its key; its
/// owner, target, name and number stay.
pub fn write(db: &Connection, request: &ApprovalRequest) -> Result<()> {
    db.execute(
        "UPDATE access_requests SET status=?2,decider=?3,gate=?4,operation=?5,record=?6,active=?7
            WHERE id=?1",
        params![
            request.key,
            request.status,
            request.decider,
            request.gate.as_ref().map(GateStep::state),
            request.operation,
            record(request)?,
            request.active
        ],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests;
