use crate::stand_in::Fixture;

#[tokio::test]
async fn records_report_only_running_executions() {
    let manager = Fixture::new().await;
    let root = &manager.root;
    let path = crate::stand_in::repository(root.path(), "repo");
    let repo = manager
        .register_repository(path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "busy".into(), None, None, None)
        .await
        .unwrap();
    assert!(
        !manager
            .spaces
            .workspace_by_id(&workspace.id)
            .await
            .unwrap()
            .running
    );
    for (state, running) in [("running", true), ("unknown", false)] {
        let id = workspace.id.clone();
        manager
            .store
            .run(move |db| {
                db.execute(
                    "INSERT OR REPLACE INTO executions(id,space_id,state) VALUES ('run',?1,?2)",
                    [id.as_str(), state],
                )?;
                Ok(())
            })
            .await
            .unwrap();
        assert_eq!(
            manager
                .spaces
                .workspace_by_id(&workspace.id)
                .await
                .unwrap()
                .running,
            running
        );
        assert_eq!(
            manager.spaces.list_workspaces().await.unwrap()[0].running,
            running
        );
    }
    manager.store.shutdown().await;
}
