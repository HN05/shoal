use std::time::Duration;

use crate::{
    daemon::{
        workspace::{ExecutionKind, Manager},
        workspace_files::Attacher,
    },
    model::Workspace,
    protocol::TurnEnd,
    state::AgentState,
    test_support::{manager, repository},
};

/// A tracked agent `name` in `workspace`.
async fn agent(manager: &Manager, workspace: &Workspace, name: &str) -> String {
    let id = manager
        .begin_execution(&workspace.id, None, ExecutionKind::Command, None)
        .await
        .unwrap()
        .plan
        .id;
    manager
        .track_agent(
            &id,
            name,
            &workspace.id,
            &workspace.name,
            true,
            Duration::ZERO,
        )
        .await;
    id
}

async fn workspace(manager: &Manager, root: &std::path::Path, name: &str) -> Workspace {
    let checkout = repository(root, &format!("{name}-repo"));
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    manager
        .create_workspace(&repo.id, name.into(), None, None, None)
        .await
        .unwrap()
}

/// The turn state and interruption of the workspace's only agent.
async fn turn(manager: &Manager, workspace: &Workspace) -> (Option<AgentState>, bool) {
    let agents = manager.workspace_agents(&workspace.name).await.unwrap();
    let [agent] = agents.as_slice() else {
        panic!("expected one agent: {agents:?}");
    };
    (agent.state, agent.interrupted)
}

#[tokio::test]
async fn an_interrupt_leaves_the_agent_idle_until_a_turn_starts_or_ends() {
    let (temp, manager) = manager().await;
    let workspace = workspace(&manager, temp.path(), "fix").await;
    let id = agent(&manager, &workspace, "claude").await;
    let report = |state| manager.set_agent_state(&workspace.id, state, Some(id.clone()), None);
    report(AgentState::Working).await.unwrap();
    assert_eq!(
        turn(&manager, &workspace).await,
        (Some(AgentState::Working), false)
    );

    let status = manager
        .interrupt_turn(&workspace.name, id.clone())
        .await
        .unwrap();
    assert_eq!(status.state, AgentState::Idle);
    assert_eq!(
        turn(&manager, &workspace).await,
        (Some(AgentState::Idle), true)
    );
    assert!(manager.turn_interrupted(&id).await);

    // Claude Code's notice of a waiting prompt reports idle again.
    report(AgentState::Idle).await.unwrap();
    assert!(manager.turn_interrupted(&id).await);
    report(AgentState::Working).await.unwrap();
    assert_eq!(
        turn(&manager, &workspace).await,
        (Some(AgentState::Working), false)
    );

    manager
        .interrupt_turn(&workspace.name, id.clone())
        .await
        .unwrap();
    manager
        .end_agent_turn(&workspace.id, Some(id.clone()), None, TurnEnd::default())
        .await
        .unwrap();
    assert!(!manager.turn_interrupted(&id).await);
}

#[tokio::test]
async fn an_interrupt_names_an_agent_of_the_workspace() {
    let (temp, manager) = manager().await;
    let workspace = workspace(&manager, temp.path(), "fix").await;
    let other = workspace_with_agent(&manager, temp.path()).await;
    let error = manager
        .interrupt_turn(&workspace.name, other)
        .await
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("no connected agent of fix runs execution"),
        "{error:#}"
    );
}

async fn workspace_with_agent(manager: &Manager, root: &std::path::Path) -> String {
    let other = workspace(manager, root, "other").await;
    agent(manager, &other, "claude").await
}

#[tokio::test]
async fn an_interrupted_agent_alone_is_due_a_wake_for_its_interruption() {
    let (temp, manager) = manager().await;
    let workspace = workspace(&manager, temp.path(), "fix").await;
    let id = agent(&manager, &workspace, "claude").await;
    manager
        .end_agent_turn(&workspace.id, Some(id.clone()), None, TurnEnd::default())
        .await
        .unwrap();
    manager
        .send_agent_message(
            &workspace.id,
            "Rebase onto main".into(),
            Vec::new(),
            &Attacher::User,
        )
        .await
        .unwrap();
    // A hang guard only: each wait answers the state it is given.
    let wait = |due| {
        tokio::time::timeout(
            Duration::from_secs(60),
            manager.wait_agent_wake(&workspace.id, due, Some(&id)),
        )
    };
    // Its hook wakes an agent whose turn ended normally.
    wait(false).await.expect("wait ended").unwrap();
    manager
        .set_agent_state(&workspace.id, AgentState::Working, Some(id.clone()), None)
        .await
        .unwrap();
    manager
        .interrupt_turn(&workspace.name, id.clone())
        .await
        .unwrap();
    wait(true).await.expect("wait ended").unwrap();
}
