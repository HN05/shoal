use crate::daemon::quarantine::*;
use crate::daemon::store::Store;
use crate::state::WorkspaceState;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::sim::{CleanRequest, CleanRequestStatus, SimRequest};
use shoal_protocol::test_support;
use shoal_protocol::time::UnixTime;
use shoal_store::test_support::seeds;
use std::path::Path;

use crate::state::ExecutionState;
use anyhow::Result;
use rusqlite::Connection;
use std::sync::Arc;

/// The records of the daemon's other subsystems, which quarantine also
/// marks.
fn records() -> Arc<dyn shoal_spaces::daemon::Records> {
    Arc::new(crate::daemon::space_service::SpaceRecords {
        coordination: crate::daemon::coordination_service::CoordinationRecords(
            crate::daemon::spaces::Removing::default(),
        ),
        config: Arc::new(crate::daemon::config_state::ConfigState::new(
            Default::default(),
        )),
    })
}

fn seed_interrupted_operations(db: &mut Connection) -> Result<()> {
    seeds::repository(db, &test_support::repository("repo", Path::new("/repo")))?;
    for state in WorkspaceState::ALL {
        let id = state.to_string();
        let path = Path::new("/work").join(&id);
        let mut workspace = test_support::workspace(&id, "repo", &id, &path);
        (workspace.state, workspace.error) = (*state, Some("original error".into()));
        seeds::workspace(db, &workspace)?;
    }
    let preparing = SpaceId::from("preparing");
    seeds::bare_execution(db, "execution", &preparing, ExecutionState::Running)?;
    shoal_leases::sim::record_clean_request(db, &requested_clean(preparing))?;
    Ok(())
}

/// A clean request of `workspace` that a stopped daemon left in progress.
fn requested_clean(workspace: SpaceId) -> CleanRequest {
    CleanRequest {
        workspace_id: workspace,
        workspace_name: "preparing".into(),
        repository_id: "repo".into(),
        execution_id: None,
        requested_at: UnixTime::from_seconds(1),
        updated_at: UnixTime::from_seconds(1),
        attempts: 1,
        request: SimRequest::new("default".into()),
        status: CleanRequestStatus::Requested,
        action: None,
        simulator_id: None,
        udid: None,
        apps_removed: None,
        erase_completed: false,
        evicted: Vec::new(),
        error: None,
    }
}

fn operation_snapshot(db: &mut Connection) -> Result<Vec<String>> {
    shoal_store::test_support::text_rows(
        db,
        &[
            "SELECT id,state,error FROM workspaces",
            "SELECT id,state FROM executions",
            "SELECT status FROM simulator_clean_requests",
        ],
    )
}

/// The operation snapshot of `seed_interrupted_operations` after quarantine.
fn quarantined_snapshot() -> Vec<String> {
    let mut rows: Vec<String> = (WorkspaceState::ALL.iter().zip(1..))
        .map(|(&original, number)| {
            let (state, error) = match original {
                // Numbered in the order the seed inserts them.
                WorkspaceState::Removing => (
                    WorkspaceState::Failed,
                    format!(
                        "removal stopped when the daemon stopped; shoal rm {number} finishes it"
                    ),
                ),
                WorkspaceState::Preparing
                | WorkspaceState::Stopping
                | WorkspaceState::Reconciling
                | WorkspaceState::Restoring
                | WorkspaceState::Updating => (
                    WorkspaceState::Failed,
                    "daemon stopped during workspace operation; inspect before cleanup".into(),
                ),
                WorkspaceState::Ready | WorkspaceState::Failed => {
                    (original, "original error".into())
                }
            };
            serde_json::json!([original, state, error]).to_string()
        })
        .collect();
    rows.push(serde_json::json!(["execution", ExecutionState::Unknown]).to_string());
    rows.push(serde_json::json!([CleanRequestStatus::Interrupted]).to_string());
    rows.sort();
    rows
}

#[tokio::test]
async fn startup_quarantines_transient_workspaces_and_preserves_settled_states() -> Result<()> {
    let root = tempfile::tempdir()?;
    let store = Store::open(root.path().join("state.db")).await?;
    store.run(seed_interrupted_operations).await?;
    // A second quarantine finds nothing left to change.
    for _ in 0..2 {
        interrupted_operations(&store, records()).await?;
        assert_eq!(store.run(operation_snapshot).await?, quarantined_snapshot());
    }
    Ok(())
}

#[tokio::test]
async fn failed_quarantine_rolls_back_until_a_retry_succeeds() -> Result<()> {
    let root = tempfile::tempdir()?;
    let store = Store::open(root.path().join("state.db")).await?;
    store.run(seed_interrupted_operations).await?;
    store
        .run(|db| {
            db.execute_batch(
                "CREATE TRIGGER fail_quarantine BEFORE UPDATE OF state ON executions
                    BEGIN SELECT RAISE(ABORT, 'quarantine blocked'); END;",
            )?;
            Ok(())
        })
        .await?;
    let before = store.run(operation_snapshot).await?;
    let error = interrupted_operations(&store, records()).await.unwrap_err();
    assert!(format!("{error:#}").contains("quarantine blocked"));
    assert_eq!(store.run(operation_snapshot).await?, before);
    store
        .run(|db| Ok(db.execute_batch("DROP TRIGGER fail_quarantine;")?))
        .await?;
    interrupted_operations(&store, records()).await?;
    assert_eq!(store.run(operation_snapshot).await?, quarantined_snapshot());
    Ok(())
}
