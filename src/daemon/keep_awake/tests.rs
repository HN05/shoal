use super::*;
use crate::test_support::{manager, repository};

async fn has_work(manager: &Manager) -> bool {
    manager.agents_have_work().await.unwrap()
}

async fn watch_pr(manager: &Manager, workspace: &crate::model::Workspace) {
    let id = workspace.id.clone();
    manager
        .store
        .run(move |db| {
            db.execute(
                "INSERT INTO pr_cleanup(workspace_id,record) VALUES (?1,?2)",
                rusqlite::params![id, r#"{"url":"https://example.com/o/r/pulls/1"}"#],
            )?;
            Ok(())
        })
        .await
        .unwrap();
}

async fn workspace(manager: &Manager, temp: &tempfile::TempDir) -> crate::model::Workspace {
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    manager
        .create_workspace(&repo.id, "agent".into(), None, None, None)
        .await
        .unwrap()
}

async fn report(manager: &Manager, workspace: &crate::model::Workspace, state: AgentState) {
    manager
        .set_agent_state(&workspace.name, state, None, None)
        .await
        .unwrap();
}

#[tokio::test]
async fn a_working_agent_has_work() {
    let (temp, manager) = manager().await;
    let workspace = workspace(&manager, &temp).await;
    assert!(!has_work(&manager).await);
    for (state, work) in [
        (AgentState::Working, true),
        (AgentState::Waiting, false),
        (AgentState::Idle, false),
    ] {
        report(&manager, &workspace, state).await;
        assert_eq!(has_work(&manager).await, work, "{state}");
    }
}

#[tokio::test]
async fn an_idle_agent_has_work_while_its_watched_pr_is_open() {
    let (temp, manager) = manager().await;
    let workspace = workspace(&manager, &temp).await;
    report(&manager, &workspace, AgentState::Idle).await;
    watch_pr(&manager, &workspace).await;
    // A PR no poll has seen yet is open.
    assert!(has_work(&manager).await);
    for (state, work) in [("open", true), ("merged", false), ("closed", false)] {
        let id = workspace.id.clone();
        let mut snapshot = crate::forge::updates::Snapshot::default();
        snapshot.adopt_state(state);
        let record = serde_json::to_string(&snapshot).unwrap();
        manager
            .store
            .run(move |db| {
                db.execute(
                    "INSERT OR REPLACE INTO workspace_item_activity(workspace_id,url,record)
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
            db.execute(
                "DELETE FROM workspace_item_activity WHERE workspace_id=?1",
                [id],
            )?;
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
        .split_workspace(&parent.name, "child".into(), None)
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
async fn a_failed_inhibitor_is_retried_under_another_config() {
    let (_temp, manager) = manager().await;
    // `cat` holds, reading the input the daemon keeps open.
    let holds: &[&str] = &["sh", "-c", "$0"];
    let config = manager.config();
    let hold = Hold::Failed(config.clone());
    let hold = update(&manager, hold, true, &config, holds).await;
    assert!(matches!(hold, Hold::Failed(_)));
    let reloaded = Arc::new(Config::default());
    let hold = update(&manager, hold, true, &reloaded, holds).await;
    assert!(matches!(hold, Hold::Held(_)));
    let hold = update(&manager, hold, false, &reloaded, holds).await;
    assert!(matches!(hold, Hold::Released));
    // A failure to start notifies and waits for the next config.
    let missing: &[&str] = &["/nonexistent/shoal-inhibitor"];
    let hold = update(&manager, hold, true, &reloaded, missing).await;
    assert!(matches!(hold, Hold::Failed(ref failed) if Arc::ptr_eq(failed, &reloaded)));
    let notifications = manager.notifications(false, 10).await.unwrap();
    assert_eq!(notifications[0].kind, NotificationKind::KeepAwakeFailed);
}

#[tokio::test]
async fn an_inhibitor_lasts_until_its_input_closes() {
    // `cat` reads the input the daemon holds, then the script reports its end.
    let mut inhibitor = Inhibitor::start(&["sh", "-c", "$0; echo released >&2"]).unwrap();
    assert!(inhibitor.child.try_wait().unwrap().is_none());
    drop(inhibitor._input.take());
    let ended = inhibitor.ended().await.to_string();
    assert!(ended.contains("released"), "{ended}");
}

#[tokio::test]
async fn a_failed_inhibitor_reports_why() {
    let mut inhibitor = Inhibitor::start(&["sh", "-c", "echo denied >&2; exit 1"]).unwrap();
    let ended = inhibitor.ended().await.to_string();
    assert!(ended.starts_with("sh ended"), "{ended}");
    assert!(ended.contains("denied"), "{ended}");
    let missing = Inhibitor::start(&["/nonexistent/shoal-inhibitor"])
        .err()
        .unwrap();
    assert!(
        format!("{missing:#}").contains("start /nonexistent/shoal-inhibitor"),
        "{missing:#}"
    );
}
