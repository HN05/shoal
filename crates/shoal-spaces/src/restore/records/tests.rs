use crate::model::RemovedSpace;
use crate::stand_in::Fixture;
use shoal_core::test_support::repository;
use shoal_ids::space_ids::SpaceId;

use crate::removal::{InspectionPolicy, UnsavedWork};
use shoal_protocol::space_names::SpaceSelector;

#[tokio::test]
async fn a_converted_workspace_keeps_its_record_beside_the_chat_it_became() {
    let manager = Fixture::new().await;
    let root = &manager.root;
    let checkout = repository(root.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "fix-login".into(), None, None, None)
        .await
        .unwrap();
    let converted = crate::conversions::pending::ConvertedChat {
        agent: "claude".into(),
        execution: shoal_ids::execution_ids::ExecutionId::generate(),
        origin: None,
        mode: crate::model::chat_mode(),
        narrowing: Default::default(),
    };
    (manager
        .spaces
        .convert_worktree_to_chat(&*manager, &workspace.id, &converted))
    .await
    .unwrap();
    let chat = SpaceSelector::id(&workspace.id);
    manager
        .spaces
        .remove_selected(
            &*manager,
            &chat,
            UnsavedWork::default(),
            InspectionPolicy::GitOnly,
        )
        .await
        .unwrap();

    // Each record has its own ID: the chat the workspace's, the workspace
    // a new one, which its checkpoints moved to.
    let removed = manager.spaces.removed_spaces().await.unwrap();
    let ids = |kind: fn(&RemovedSpace) -> bool| -> Vec<SpaceId> {
        (removed.iter().filter(|removed| kind(removed)))
            .map(|removed| removed.space_id.clone())
            .collect()
    };
    assert_eq!(
        ids(|removed| removed.chat().is_some()),
        std::slice::from_ref(&workspace.id)
    );
    let workspaces = ids(|removed| removed.workspace().is_some());
    let [record] = workspaces.as_slice() else {
        panic!("{removed:?}");
    };
    let record = record.clone();
    let owner = record.clone();
    let checkpoints: i64 = (manager.store)
        .run(move |db| {
            Ok(db.query_row(
                "SELECT COUNT(*) FROM checkpoints WHERE workspace_id=?1",
                [&owner],
                |row| row.get(0),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(checkpoints, 1);

    let restored = manager
        .spaces
        .restore_space(&*manager, &chat, None)
        .await
        .unwrap();
    assert!(matches!(restored, crate::model::RestoredSpace::Chat(_)));
    // The workspace restores beside the chat, under its record's ID.
    let restored = (manager
        .spaces
        .restore_space(&*manager, &SpaceSelector::id(&record), None))
    .await
    .unwrap();
    let crate::model::RestoredSpace::Workspace(restored) = restored else {
        panic!("{restored:?}");
    };
    assert_eq!(restored.opened.workspace.id, record);
    assert!(manager.spaces.removed_spaces().await.unwrap().is_empty());
}

/// A record of `kind`'s fields, removed by hand and kept long.
fn removed(id: &str, kind: serde_json::Value) -> RemovedSpace {
    let mut record = serde_json::json!({
        "workspace_id": id, "repository_id": "repo", "name": id,
        "links": {"issues": [], "prs": [], "opened": [], "related": []},
        "cause": "manual", "removed_at": 1, "expires_at": 4_000_000_000_i64,
    });
    record
        .as_object_mut()
        .unwrap()
        .extend(kind.as_object().unwrap().clone());
    serde_json::from_value(record).unwrap()
}

#[test]
fn removal_records_round_trip_through_their_columns_and_json() -> anyhow::Result<()> {
    let db = shoal_store::test_support::migrated()?;
    let repository = shoal_protocol::test_support::repository("repo", std::path::Path::new("/r"));
    shoal_store::test_support::seeds::repository(&db, &repository)?;
    let checkpoint = serde_json::json!({
        "workspace_id": "work", "repository_id": "repo", "number": 2, "cause": "removal",
        "branch": "fix", "head": "a".repeat(40), "commit": "a".repeat(40),
        "files_omitted": false, "created_at": 1, "expires_at": 4_000_000_000_i64,
    });
    let workspace = removed(
        "work",
        serde_json::json!({"checkpoint": checkpoint, "branch": "fix"}),
    );
    let chat = removed(
        "chat",
        serde_json::json!({"chat": {"mode": "chat", "workspace": {
            "id": "work", "name": "fix", "branch": "fix"}}}),
    );
    for record in [&workspace, &chat] {
        super::record(&db, record)?;
        assert_eq!(super::find(&db, &record.space_id)?.as_ref(), Some(record));
    }
    let mut records = db.prepare("SELECT record FROM removed_spaces ORDER BY space_id")?;
    let records = records
        .query_map([], |row| row.get::<_, String>(0))?
        .map(|record| Ok(serde_json::from_str(&record?)?))
        .collect::<anyhow::Result<Vec<serde_json::Value>>>()?;
    let [chat, workspace] = records.as_slice() else {
        panic!("{records:?}");
    };
    for moved in ["workspace_id", "repository_id", "name", "expires_at"] {
        assert!(workspace.get(moved).is_none(), "{workspace}");
    }
    assert!(
        workspace["checkpoint"].get("expires_at").is_none(),
        "{workspace}"
    );
    assert!(chat["chat"]["workspace"].get("id").is_none(), "{chat}");
    Ok(())
}
