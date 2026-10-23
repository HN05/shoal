use super::*;
use crate::test_support::{manager, repository};

async fn has_work(manager: &Manager) -> bool {
    manager.store.run(agents_have_work).await.unwrap()
}

#[tokio::test]
async fn only_a_working_agent_has_work() {
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "agent".into(), None, None, None)
        .await
        .unwrap();
    assert!(!has_work(&manager).await);
    for (state, work) in [
        (AgentState::Working, true),
        (AgentState::Waiting, false),
        (AgentState::Idle, false),
    ] {
        manager
            .set_agent_state(&workspace.name, state, None, None)
            .await
            .unwrap();
        assert_eq!(has_work(&manager).await, work, "{state}");
    }
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
