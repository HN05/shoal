use super::*;

#[tokio::test]
async fn restart_waits_for_background_work_and_executions_that_cannot_reattach() {
    let (_root, manager) = crate::test_support::manager().await;
    assert!(quiesce(&manager).is_some());
    let operation = manager.background_operations.read().await;
    assert!(quiesce(&manager).is_none());
    drop(operation);
    let quiet = quiesce(&manager).unwrap();
    assert!(manager.background_operations.try_read().is_err());
    drop(quiet);
    // A restart leaves disconnected records as they are, including
    // commands from releases before reattachment.
    manager.store.run(|db| {
            db.execute_batch(
                "INSERT INTO repositories(id,path,source,last_used) VALUES ('repo','/test','/test',0);
                 INSERT INTO workspaces(id,repository_id,name,path,branch,state)
                    VALUES ('workspace','repo','work','/test/work','work','ready');
                 INSERT INTO executions(id,workspace_id,state) VALUES ('legacy','workspace','running');
                 INSERT INTO executions(id,workspace_id,state) VALUES ('unknown','workspace','unknown');"
            )?;
            Ok(())
        }).await.unwrap();
    assert!(quiesce(&manager).is_some());
    // Connected setup and landing stop with the daemon.
    manager.mark_unattachable("setup");
    assert!(quiesce(&manager).is_none());
}
