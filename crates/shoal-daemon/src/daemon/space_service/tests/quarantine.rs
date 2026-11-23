use crate::daemon::quarantine::*;
use crate::daemon::store::Store;
use crate::state::WorkspaceState;

use crate::state::ExecutionState;
use anyhow::Result;
use rusqlite::Connection;
use std::sync::Arc;

/// The records of the daemon's other subsystems, which quarantine also
/// marks.
fn records() -> Arc<dyn shoal_spaces::daemon::Records> {
    Arc::new(crate::daemon::space_service::SpaceRecords {
        coordination: crate::daemon::coordination::CoordinationRecords(
            crate::daemon::spaces::Removing::default(),
        ),
    })
}

fn seed_interrupted_operations(db: &mut Connection) -> Result<()> {
    db.execute_batch(
        "INSERT INTO repositories(id,path,source,last_used) VALUES ('repo','/repo','/repo',1);",
    )?;
    for state in WorkspaceState::ALL {
        db.execute(
            "INSERT INTO spaces(id,kind,repository_id,name) VALUES (?1,'workspace','repo',?1)",
            [state],
        )?;
        db.execute(
            "INSERT INTO workspaces(id,path,branch,state,error,base_commit,git_dir)
                VALUES (?1,?1,?1,?1,'original error','abc','/admin')",
            [state],
        )?;
    }
    db.execute_batch("INSERT INTO executions(id,space_id,state) VALUES ('execution','preparing','running');
            INSERT INTO simulator_clean_requests(request_id,workspace_id,record)
                VALUES ('clean','preparing','{\"status\":\"requested\",\"reason\":\"keep audit\"}');")?;
    Ok(())
}

fn operation_snapshot(db: &mut Connection) -> Result<Vec<String>> {
    Ok(db
        .prepare(
            "SELECT json_array(id,state,error) FROM workspaces
            UNION ALL SELECT json_array(id,state) FROM executions
            UNION ALL SELECT record FROM simulator_clean_requests ORDER BY 1",
        )?
        .query_map([], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?)
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
    rows.push(r#"{"status":"interrupted","reason":"keep audit"}"#.into());
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
