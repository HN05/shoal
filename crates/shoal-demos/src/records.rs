//! The store's record of each demo with the execution that runs it, which
//! the transactions that register an execution and its runs write.
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use shoal_ids::execution_ids::ExecutionId;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::demos::{Launch, Running, validate_name};
use shoal_protocol::space_names::SpaceName;

use crate::time::UnixTime;

/// Whether execution `execution` runs a demo.
pub fn runs_demo(db: &Connection, execution: &ExecutionId) -> Result<bool> {
    shoal_store::exists(db, "SELECT 1 FROM demos WHERE execution_id=?1", [execution])
}

/// Resolve a demo's place before deleting the execution cascades its record.
pub fn execution_launch(db: &Connection, execution: &ExecutionId) -> Result<Option<Launch>> {
    let record: Option<String> = db
        .query_row(
            "SELECT record FROM demos WHERE execution_id=?1",
            [execution],
            |row| row.get(0),
        )
        .optional()?;
    record
        .map(|record| serde_json::from_str(&record).context("invalid demo record"))
        .transpose()
}

/// The ID of the launch of the demo `execution` runs, while its command
/// has not started.
pub(crate) fn unrun_launch(db: &Connection, execution: &ExecutionId) -> Result<Option<String>> {
    let record: Option<String> = db
        .query_row(
            "SELECT record FROM demos WHERE execution_id=?1 AND has_run=0",
            [execution],
            |row| row.get(0),
        )
        .optional()?;
    record
        .map(|record| {
            serde_json::from_str::<Launch>(&record)
                .map(|launch| launch.id)
                .context("invalid demo record")
        })
        .transpose()
}

/// Record `launch` as the demo `execution` runs, in the transaction that
/// registers the execution. A record lasts as long as its execution, so a
/// name still recorded belongs to a demo that runs or is stopping.
pub fn record(
    tx: &Transaction<'_>,
    execution: &ExecutionId,
    workspace: &SpaceId,
    launch: &Launch,
) -> Result<()> {
    validate_name(&launch.name)?;
    let taken = shoal_store::exists(
        tx,
        "SELECT 1 FROM demos WHERE workspace_id=?1 AND name=?2",
        params![workspace, launch.name],
    )?;
    ensure!(
        !taken,
        "demo {} is already running; stop it before starting another",
        launch.name
    );
    tx.execute(
        "INSERT INTO demos(execution_id,workspace_id,name,started_at,record) VALUES (?1,?2,?3,?4,?5)",
        params![
            execution,
            workspace,
            launch.name,
            UnixTime::now(),
            serde_json::to_string(launch)?
        ],
    )?;
    Ok(())
}

/// The demos of `workspace`, or of every workspace, by workspace and name.
pub(crate) fn list(db: &Connection, workspace: Option<&SpaceId>) -> Result<Vec<Running>> {
    let mut statement = db.prepare(
        "SELECT d.workspace_id,w.name,d.execution_id,d.started_at,d.record,d.has_run
         FROM demos d JOIN spaces w ON w.id=d.workspace_id
         WHERE ?1 IS NULL OR d.workspace_id=?1 ORDER BY w.name,d.name",
    )?;
    let rows = statement.query_map([workspace], |row| {
        Ok((
            row.get::<_, SpaceId>(0)?,
            row.get::<_, SpaceName>(1)?,
            row.get::<_, ExecutionId>(2)?,
            row.get(3)?,
            row.get::<_, String>(4)?,
            row.get::<_, bool>(5)?,
        ))
    })?;
    rows.map(|row| {
        let (workspace_id, workspace, execution_id, started_at, record, has_run) = row?;
        let launch: Launch = serde_json::from_str(&record).context("invalid demo record")?;
        Ok(Running {
            workspace_id,
            workspace,
            execution_id,
            started_at,
            has_run,
            kind: launch.kind(),
            url: launch.url(),
            viewer: None,
            launch,
            machine: None,
        })
    })
    .collect()
}

/// Note in the transaction registering an execution's child that its demo,
/// if it runs one, has started its command.
pub fn mark_run(tx: &Transaction<'_>, execution: &ExecutionId) -> Result<()> {
    tx.execute(
        "UPDATE demos SET has_run=1 WHERE execution_id=?1",
        [execution],
    )?;
    Ok(())
}
