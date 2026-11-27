//! The rows of `simulator_clean_requests`: a clean request's ID, workspace
//! and status in columns, which queries read, and its other fields in its
//! record.
use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, Row, Transaction, params};
use serde::{Deserialize, Serialize};
use shoal_ids::execution_ids::ExecutionId;
use shoal_ids::repository_ids::RepositoryId;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::space_names::SpaceName;

use super::{AuditEntry, CleanAction, CleanRequest, CleanRequestStatus, EvictedDevice, SimRequest};
use crate::time::UnixTime;

const COLUMNS: &str = "id,request_id,workspace_id,status,record";

/// A clean request's fields that its columns do not hold.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    workspace_name: SpaceName,
    repository_id: RepositoryId,
    execution_id: Option<ExecutionId>,
    requested_at: UnixTime,
    updated_at: UnixTime,
    attempts: u64,
    request: Request,
    action: Option<CleanAction>,
    simulator_id: Option<String>,
    udid: Option<String>,
    apps_removed: Option<usize>,
    erase_completed: bool,
    evicted: Vec<EvictedDevice>,
    error: Option<String>,
}

/// What a clean request asked for, without the ID its column holds.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    clean: bool,
    name: String,
    profile: Option<String>,
    device: Option<String>,
    runtime: Option<String>,
    reason: Option<String>,
    #[serde(default)]
    queue: bool,
}

/// The clean request recorded under `request_id`, if any.
pub(super) fn find(db: &Connection, request_id: &str) -> Result<Option<CleanRequest>> {
    let found = db
        .query_row(
            &format!("SELECT {COLUMNS} FROM simulator_clean_requests WHERE request_id=?1"),
            [request_id],
            |row| Ok(entry(row)),
        )
        .optional()?;
    found
        .transpose()
        .map(|entry| entry.map(|entry| entry.request))
}

/// Up to `limit` clean requests before `before`, of `owner` or of every
/// workspace, newest first.
pub(super) fn history(
    db: &Connection,
    owner: Option<&SpaceId>,
    limit: u32,
    before: Option<i64>,
) -> Result<Vec<AuditEntry>> {
    let mut statement = db.prepare(&format!(
        "SELECT {COLUMNS} FROM simulator_clean_requests
            WHERE (?1 IS NULL OR workspace_id=?1) AND (?2 IS NULL OR id<?2)
            ORDER BY id DESC LIMIT ?3"
    ))?;
    let entries = statement.query_map(params![owner, before, limit], |row| Ok(entry(row)))?;
    entries.map(|entry| entry?).collect()
}

/// The history entry that `row`, of [`COLUMNS`], holds.
fn entry(row: &Row<'_>) -> Result<AuditEntry> {
    let id: i64 = row.get(0)?;
    decoded(id, row).with_context(|| format!("decode clean request {id}"))
}

fn decoded(id: i64, row: &Row<'_>) -> Result<AuditEntry> {
    let record: Record = serde_json::from_str(&row.get::<_, String>(4)?)?;
    let Request {
        clean,
        name,
        profile,
        device,
        runtime,
        reason,
        queue,
    } = record.request;
    let request = CleanRequest {
        workspace_id: row.get(2)?,
        workspace_name: record.workspace_name,
        repository_id: record.repository_id,
        execution_id: record.execution_id,
        requested_at: record.requested_at,
        updated_at: record.updated_at,
        attempts: record.attempts,
        request: SimRequest {
            request_id: row.get(1)?,
            clean,
            name,
            profile,
            device,
            runtime,
            reason,
            queue,
        },
        status: row.get(3)?,
        action: record.action,
        simulator_id: record.simulator_id,
        udid: record.udid,
        apps_removed: record.apps_removed,
        erase_completed: record.erase_completed,
        evicted: record.evicted,
        error: record.error,
    };
    Ok(AuditEntry { id, request })
}

/// Record `audit`, in place of the row of its request ID.
pub fn save(db: &Connection, audit: &CleanRequest) -> Result<()> {
    let CleanRequest {
        workspace_id,
        workspace_name,
        repository_id,
        execution_id,
        requested_at,
        updated_at,
        attempts,
        request,
        status,
        action,
        simulator_id,
        udid,
        apps_removed,
        erase_completed,
        evicted,
        error,
    } = audit.clone();
    let SimRequest {
        request_id,
        clean,
        name,
        profile,
        device,
        runtime,
        reason,
        queue,
    } = request;
    let record = Record {
        workspace_name,
        repository_id,
        execution_id,
        requested_at,
        updated_at,
        attempts,
        request: Request {
            clean,
            name,
            profile,
            device,
            runtime,
            reason,
            queue,
        },
        action,
        simulator_id,
        udid,
        apps_removed,
        erase_completed,
        evicted,
        error,
    };
    db.execute(
        "INSERT INTO simulator_clean_requests(request_id,workspace_id,status,record)
            VALUES (?1,?2,?3,?4)
            ON CONFLICT(request_id) DO UPDATE SET status=excluded.status, record=excluded.record",
        params![
            request_id,
            workspace_id,
            status,
            serde_json::to_string(&record)?
        ],
    )?;
    Ok(())
}

/// Mark the clean requests a stopped daemon left running as interrupted.
pub fn interrupt(tx: &Transaction<'_>) -> Result<()> {
    tx.execute(
        "UPDATE simulator_clean_requests SET status=?1 WHERE status=?2",
        [
            CleanRequestStatus::Interrupted,
            CleanRequestStatus::Requested,
        ],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests;
