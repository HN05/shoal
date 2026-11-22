use shoal_protocol::space_names::SpaceSelector;

use crate::{
    daemon::{
        manager::Manager,
        test_support::{manager, repository},
    },
    state::AgentState,
};

async fn has_work(manager: &Manager) -> bool {
    manager.agents.agents_have_work(manager).await.unwrap()
}

async fn watch_pr(manager: &Manager, workspace: &crate::model::Workspace) {
    let id = workspace.id.clone();
    manager
        .store
        .run(move |db| {
            db.execute(
                "INSERT INTO pr_cleanup(space_id,record) VALUES (?1,?2)",
                rusqlite::params![id, r#"{"urls":["https://example.com/o/r/pulls/1"]}"#],
            )?;
            Ok(())
        })
        .await
        .unwrap();
}

async fn workspace(manager: &Manager, temp: &tempfile::TempDir) -> crate::model::Workspace {
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .repositories
        .register(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    manager
        .create_workspace(&repo.id, "agent".into(), None, None, None)
        .await
        .unwrap()
}

async fn report(manager: &Manager, workspace: &crate::model::Workspace, state: AgentState) {
    manager
        .agents
        .set_agent_state(
            manager,
            &SpaceSelector::id(&workspace.id),
            state,
            None,
            None,
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn an_idle_agent_has_work_while_its_watched_pr_is_open() {
    let (temp, manager) = manager().await;
    let workspace = workspace(&manager, &temp).await;
    report(&manager, &workspace, AgentState::Idle).await;
    watch_pr(&manager, &workspace).await;
    // A PR no poll has seen yet is open.
    assert!(has_work(&manager).await);
    use crate::forge::updates::SnapshotState;
    for (state, work) in [(SnapshotState::Open, true), (SnapshotState::Merged, false)] {
        let id = workspace.id.clone();
        let mut snapshot = shoal_forge::updates::Snapshot::default();
        snapshot.adopt_state(Some(state));
        let record = serde_json::to_string(&snapshot).unwrap();
        manager
            .store
            .run(move |db| {
                db.execute(
                    "INSERT OR REPLACE INTO space_item_activity(space_id,url,record)
                     VALUES (?1,'https://example.com/o/r/pulls/1',?2)",
                    rusqlite::params![id, record],
                )?;
                Ok(())
            })
            .await
            .unwrap();
        assert_eq!(has_work(&manager).await, work, "{state}");
    }
    // An agent waiting for the user cannot take the PR's activity.
    report(&manager, &workspace, AgentState::Waiting).await;
    let id = workspace.id.clone();
    manager
        .store
        .run(move |db| {
            db.execute("DELETE FROM space_item_activity WHERE space_id=?1", [id])?;
            Ok(())
        })
        .await
        .unwrap();
    assert!(!has_work(&manager).await);
}

#[tokio::test]
async fn an_idle_agent_has_work_from_a_child_without_its_own_agent() {
    let (temp, manager) = manager().await;
    let parent = workspace(&manager, &temp).await;
    let child = manager
        .create_child(
            &SpaceSelector::id(&parent.id),
            "child".into(),
            None,
            Default::default(),
        )
        .await
        .unwrap();
    report(&manager, &parent, AgentState::Idle).await;
    watch_pr(&manager, &child).await;
    assert!(has_work(&manager).await);
    // Once the child reports its own agent, that agent receives the PR.
    report(&manager, &child, AgentState::Waiting).await;
    assert!(!has_work(&manager).await);
}

#[tokio::test]
async fn a_chats_agent_has_work_as_a_workspaces_does() {
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .repositories
        .register(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let chat = manager
        .start_chat(repo.id.into(), "talk".into(), "claude".into(), None)
        .await
        .unwrap();
    for (state, work) in [(AgentState::Working, true), (AgentState::Idle, false)] {
        manager
            .agents
            .set_agent_state(&*manager, &SpaceSelector::id(&chat.id), state, None, None)
            .await
            .unwrap();
        assert_eq!(has_work(&manager).await, work, "{state}");
    }
}
