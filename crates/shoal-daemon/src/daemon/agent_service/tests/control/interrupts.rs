use shoal_ids::execution_ids::ExecutionId;
use shoal_ids::space_ids::SpaceId;
use std::time::Duration;

use crate::{
    daemon::{
        executions::ExecutionKind,
        manager::Manager,
        test_support::{manager, repository},
    },
    model::{Chat, Workspace},
    protocol::TurnEnd,
    state::AgentState,
};
use shoal_protocol::space_names::SpaceSelector;

/// A tracked agent `name` in `workspace`.
async fn agent(manager: &Manager, workspace: &Workspace, name: &str) -> ExecutionId {
    let id = manager
        .agents
        .begin_execution(
            manager,
            workspace.id.as_str(),
            None,
            ExecutionKind::Command,
            None,
        )
        .await
        .unwrap()
        .plan
        .id;
    manager
        .agents
        .track_agent(
            manager,
            &id,
            &shoal_protocol::executions::TrackedAgent::named(name),
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
        .repositories
        .register(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    manager
        .create_workspace(&repo.id, name.into(), None, None, None)
        .await
        .unwrap()
}

/// The turn state and interruption of the workspace's only agent.
async fn turn(manager: &Manager, workspace: &Workspace) -> (Option<AgentState>, bool) {
    let agents = manager
        .agents
        .space_agents(manager, &SpaceSelector::id(&workspace.id))
        .await
        .unwrap();
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
    let selected = SpaceSelector::id(&workspace.id);
    let report = |state| {
        manager
            .agents
            .set_agent_state(&*manager, &selected, state, Some(id.clone()), None)
    };
    report(AgentState::Working).await.unwrap();
    assert_eq!(
        turn(&manager, &workspace).await,
        (Some(AgentState::Working), false)
    );

    let status = manager
        .agents
        .interrupt_turn(&*manager, &SpaceSelector::id(&workspace.id), id.clone())
        .await
        .unwrap();
    assert_eq!(status.state, AgentState::Idle);
    assert_eq!(
        turn(&manager, &workspace).await,
        (Some(AgentState::Idle), true)
    );
    assert!(manager.agents.turn_interrupted(&id).await);

    // Claude Code's notice of a waiting prompt reports idle again.
    report(AgentState::Idle).await.unwrap();
    assert!(manager.agents.turn_interrupted(&id).await);
    report(AgentState::Working).await.unwrap();
    assert_eq!(
        turn(&manager, &workspace).await,
        (Some(AgentState::Working), false)
    );

    manager
        .agents
        .interrupt_turn(&*manager, &SpaceSelector::id(&workspace.id), id.clone())
        .await
        .unwrap();
    manager
        .agents
        .end_agent_turn(
            &*manager,
            &SpaceSelector::id(&workspace.id),
            Some(id.clone()),
            None,
            TurnEnd::default(),
        )
        .await
        .unwrap();
    assert!(!manager.agents.turn_interrupted(&id).await);
}

#[tokio::test]
async fn an_interrupt_names_an_agent_of_the_workspace() {
    let (temp, manager) = manager().await;
    let workspace = workspace(&manager, temp.path(), "fix").await;
    let other = workspace_with_agent(&manager, temp.path()).await;
    manager
        .agents
        .interrupt_turn(&*manager, &SpaceSelector::id(&workspace.id), other)
        .await
        .unwrap_err();
}

async fn workspace_with_agent(manager: &Manager, root: &std::path::Path) -> ExecutionId {
    let other = workspace(manager, root, "other").await;
    agent(manager, &other, "claude").await
}

#[tokio::test]
async fn an_interrupted_agent_alone_is_due_a_wake_for_its_interruption() {
    let (temp, manager) = manager().await;
    let workspace = workspace(&manager, temp.path(), "fix").await;
    let id = agent(&manager, &workspace, "claude").await;
    manager
        .agents
        .end_agent_turn(
            &*manager,
            &SpaceSelector::id(&workspace.id),
            Some(id.clone()),
            None,
            TurnEnd::default(),
        )
        .await
        .unwrap();
    manager
        .send_space_message(
            None,
            &SpaceSelector::id(&workspace.id),
            "Rebase onto main".into(),
            Vec::new(),
        )
        .await
        .unwrap();
    // A hang guard only: each wait answers the state it is given.
    let selected = SpaceSelector::id(&workspace.id);
    let wait = |due| {
        tokio::time::timeout(
            Duration::from_secs(60),
            manager.wait_agent_wake(&selected, due, Some(&id)),
        )
    };
    // Its hook wakes an agent whose turn ended normally.
    wait(false).await.expect("wait ended").unwrap();
    manager
        .agents
        .set_agent_state(
            &*manager,
            &SpaceSelector::id(&workspace.id),
            AgentState::Working,
            Some(id.clone()),
            None,
        )
        .await
        .unwrap();
    manager
        .agents
        .interrupt_turn(&*manager, &SpaceSelector::id(&workspace.id), id.clone())
        .await
        .unwrap();
    wait(true).await.expect("wait ended").unwrap();
}

#[tokio::test]
async fn a_chats_interrupted_agent_is_due_its_wrappers_wake_for_a_message() {
    let (temp, manager) = manager().await;
    let chat = chat(&manager, temp.path()).await;
    let selected = SpaceSelector::id(&chat.id);
    let id = manager.workspace_executions(&chat.id).await.unwrap()[0]
        .id
        .clone();
    manager
        .agents
        .end_agent_turn(
            &*manager,
            &selected,
            Some(id.clone()),
            None,
            TurnEnd::default(),
        )
        .await
        .unwrap();
    manager
        .send_space_message(None, &selected, "Rebase onto main".into(), Vec::new())
        .await
        .unwrap();
    // A hang guard only: each wait answers the state it is given.
    let wait = |due, hooked| {
        tokio::time::timeout(
            Duration::from_secs(60),
            manager.wait_agent_wake(&selected, due, hooked),
        )
    };
    // Its hook wakes an agent whose turn ended normally, while an agent
    // without one is due its wrapper's prompt.
    wait(false, Some(&id)).await.expect("wait ended").unwrap();
    wait(true, None).await.expect("wait ended").unwrap();
    manager
        .agents
        .set_agent_state(
            &*manager,
            &selected,
            AgentState::Working,
            Some(id.clone()),
            None,
        )
        .await
        .unwrap();
    manager
        .agents
        .interrupt_turn(&*manager, &selected, id.clone())
        .await
        .unwrap();
    // After an interrupt, which its hook does not follow, the wrapper
    // prompts it, until it takes the message.
    wait(true, Some(&id)).await.expect("wait ended").unwrap();
    let messages = manager
        .agents
        .agent_messages(&*manager, &SpaceSelector::id(&chat.id))
        .await
        .unwrap();
    let delivered = messages.iter().map(|message| message.id).collect();
    (manager.agents.mark_agent_messages_delivered(
        &*manager,
        &SpaceSelector::id(&chat.id),
        delivered,
    ))
    .await
    .unwrap();
    wait(false, Some(&id)).await.expect("wait ended").unwrap();
}

/// A running chat with a Claude Code agent in a repository under `root`.
async fn chat(manager: &std::sync::Arc<Manager>, root: &std::path::Path) -> Chat {
    let checkout = repository(root, "chat-repo");
    let repo = manager
        .repositories
        .register(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    manager
        .start_chat(repo.id.into(), "talk".into(), "claude".into(), None)
        .await
        .unwrap()
}

/// The turn state, interruption and listed execution of the space's only
/// agent.
async fn listed(manager: &Manager, space: &SpaceId) -> (Option<AgentState>, bool, ExecutionId) {
    let agents = manager
        .agents
        .space_agents(manager, &SpaceSelector::id(space))
        .await
        .unwrap();
    let [agent] = agents.as_slice() else {
        panic!("expected one agent: {agents:?}");
    };
    (agent.state, agent.interrupted, agent.execution.clone())
}

#[tokio::test]
async fn an_interrupt_leaves_a_chats_agent_idle_until_a_turn_starts() {
    let (temp, manager) = manager().await;
    let chat = chat(&manager, temp.path()).await;
    let selected = SpaceSelector::id(&chat.id);
    let agent = manager.workspace_executions(&chat.id).await.unwrap()[0]
        .id
        .clone();
    let report = |state| {
        manager
            .agents
            .set_agent_state(&*manager, &selected, state, Some(agent.clone()), None)
    };
    report(AgentState::Working).await.unwrap();
    let (state, interrupted, execution) = listed(&manager, &chat.id).await;
    assert_eq!((state, interrupted), (Some(AgentState::Working), false));

    let status = manager
        .agents
        .interrupt_turn(&*manager, &SpaceSelector::id(&chat.id), execution.clone())
        .await
        .unwrap();
    assert_eq!(status.state, AgentState::Idle);
    // The chat's state shows, which a report from an execution it does not
    // run would hide.
    let recorded = manager.chats().await.unwrap();
    assert_eq!(
        recorded[0].agent_state.as_ref().map(|status| status.state),
        Some(AgentState::Idle)
    );
    assert_eq!(
        listed(&manager, &chat.id).await,
        (Some(AgentState::Idle), true, execution.clone())
    );

    report(AgentState::Idle).await.unwrap();
    assert!(listed(&manager, &chat.id).await.1);
    report(AgentState::Working).await.unwrap();
    assert!(!listed(&manager, &chat.id).await.1);
}

#[tokio::test]
async fn an_interrupt_names_a_chats_agent_as_it_is_listed() {
    let (temp, manager) = manager().await;
    let chat = chat(&manager, temp.path()).await;
    let other = workspace_with_agent(&manager, temp.path()).await;
    manager
        .agents
        .interrupt_turn(&*manager, &SpaceSelector::id(&chat.id), other)
        .await
        .unwrap_err();
}
