use std::sync::Arc;

use shoal_ids::space_ids::SpaceId;
use shoal_protocol::alerts::AlertKind;

use crate::{
    agents::hook_reports::{Report, Sending},
    daemon::{
        manager::Manager,
        test_support::{manager, repository},
    },
    protocol::TurnEnd,
    state::AgentState,
};

fn ended_turn() -> Report {
    Report::State {
        state: AgentState::Idle,
        session: None,
        turn_end: Some(TurnEnd::default()),
    }
}

/// How many failed state reports Shoal told the user of.
async fn failures(manager: &Manager) -> usize {
    let notifications = manager.notifications.list(true, 50).await.unwrap();
    (notifications.into_iter())
        .filter(|notification| notification.kind == AlertKind::StateReportFailed)
        .count()
}

#[tokio::test]
async fn a_chats_lost_report_counts_only_from_the_agent_it_runs() {
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
    let working = Report::State {
        state: AgentState::Working,
        session: None,
        turn_end: None,
    };
    let current = |manager: Arc<Manager>, id: SpaceId| async move {
        manager
            .store
            .run(move |db| crate::daemon::turn_state::current(db, &id))
            .await
            .unwrap()
            .map(|status| status.state)
    };
    let execution = |manager: Arc<Manager>, id: SpaceId| async move {
        let executions = manager.spaces.workspace_executions(&id).await.unwrap();
        Some(executions[0].id.clone())
    };
    let first = execution(manager.clone(), chat.id.clone()).await;
    Sending::keep(&manager.paths, &chat.id, working, first)
        .unwrap()
        .failed("timed out".into());
    manager.agents.recover_lost_reports(&*manager).await;
    assert_eq!(
        current(manager.clone(), chat.id.clone()).await,
        Some(AgentState::Working)
    );

    let first = execution(manager.clone(), chat.id.clone()).await;
    Sending::keep(&manager.paths, &chat.id, ended_turn(), first)
        .unwrap()
        .failed("refused".into());
    // A restore runs another agent.
    manager.agents.end_executions(&*manager, &chat.id).await;
    let chat = manager.spaces.chat_by_id(&chat.id).await.unwrap().unwrap();
    let mut restored = chat.clone();
    restored.agent = Some("claude".into());
    manager.run_chat_agent(restored, None).await.unwrap();
    manager.agents.recover_lost_reports(&*manager).await;

    // The state ended with the agent, and the earlier agent's report does
    // not count for the one running now.
    assert_eq!(current(manager.clone(), chat.id.clone()).await, None);
    assert_eq!(failures(&manager).await, 2);
}
