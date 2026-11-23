use crate::state::WorkspaceState;
use crate::store::Store;

use anyhow::Result;
use rusqlite::Connection;

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

#[tokio::test]
async fn current_schema_reopen_preserves_active_operations() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("state.db");
    let store = Store::open(path.clone()).await.unwrap();
    store.run(seed_interrupted_operations).await.unwrap();
    let before = store.run(operation_snapshot).await.unwrap();
    for _ in 0..2 {
        let reopened = Store::open(path.clone()).await.unwrap();
        assert_eq!(reopened.run(operation_snapshot).await.unwrap(), before);
    }
}
