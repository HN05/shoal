use std::sync::Arc;

use super::super::{ExecutionKind, Manager};
use crate::client::space_names::SpaceSelector;
use crate::{
    client::list_status::ListStatus,
    daemon::test_support::{ProcessGroup, manager, repository},
    execution::TrackedAgent,
    model::{CleanupReason, Workspace},
    state::AgentState,
};

/// A workspace `parent` and a workspace split from it.
async fn split_child(manager: &Manager, root: &std::path::Path) -> (Workspace, Workspace) {
    let checkout = repository(root, "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let parent = manager
        .create_workspace(&repo.id, "parent".into(), None, None, None)
        .await
        .unwrap();
    let child = manager
        .split_workspace(
            &SpaceSelector::id(&parent.id),
            "child".into(),
            None,
            Default::default(),
        )
        .await
        .unwrap();
    (parent, child)
}

/// Run an agent the daemon tracks in `workspace` until the returned
/// execution finishes.
async fn start_agent(manager: &Manager, workspace: &Workspace) -> super::super::StartedExecution {
    let started = manager
        .begin_execution(&workspace.id, None, ExecutionKind::Command, None)
        .await
        .unwrap();
    manager
        .track_agent(
            &started.plan.id,
            &TrackedAgent::named("claude"),
            &workspace.id,
            &workspace.name,
            false,
            std::time::Duration::ZERO,
        )
        .await;
    started
}

/// End the agent `started` runs.
async fn finish(manager: &Manager, started: super::super::StartedExecution) {
    manager
        .finish_execution(started.plan.id.clone(), ExecutionKind::Command, Some(0))
        .await
        .unwrap();
}

/// Why idle cleanup keeps `workspace`, or `None` when it is a candidate.
async fn kept(manager: &Manager, workspace: &Workspace) -> Option<CleanupReason> {
    manager
        .cleanup_candidacy(&workspace.id)
        .await
        .unwrap()
        .err()
}

/// The status a list shows `workspace`.
async fn listed(manager: &Manager, workspace: &Workspace) -> ListStatus {
    let listed = manager.visible_workspaces(None).await.unwrap();
    let found = listed.iter().find(|w| w.id == workspace.id).unwrap();
    ListStatus::of_workspace(found, false)
}

async fn report(manager: &Manager, workspace: &Workspace, state: AgentState) {
    manager
        .set_agent_state(&SpaceSelector::id(&workspace.id), state, None, None)
        .await
        .unwrap();
}

#[tokio::test]
async fn a_child_without_an_agent_lists_as_its_coordinators_work() {
    let (root, manager) = manager().await;
    let (parent, child) = split_child(&manager, root.path()).await;
    assert_eq!(listed(&manager, &child).await, ListStatus::Inactive);
    report(&manager, &parent, AgentState::Working).await;
    assert_eq!(listed(&manager, &child).await, ListStatus::ParentWorking);
    // A grandchild split from it is the same agent's work.
    let grandchild = manager
        .split_workspace(
            &SpaceSelector::id(&child.id),
            "grandchild".into(),
            None,
            Default::default(),
        )
        .await
        .unwrap();
    assert_eq!(
        listed(&manager, &grandchild).await,
        ListStatus::ParentWorking
    );
    report(&manager, &parent, AgentState::Waiting).await;
    assert_eq!(listed(&manager, &child).await, ListStatus::Inactive);
}

#[tokio::test]
async fn a_child_that_had_its_own_agent_lists_as_before() {
    let (root, manager) = manager().await;
    let (parent, child) = split_child(&manager, root.path()).await;
    report(&manager, &parent, AgentState::Working).await;
    finish(&manager, start_agent(&manager, &child).await).await;
    assert_eq!(listed(&manager, &child).await, ListStatus::Inactive);
    // The daemon keeps that the child had an agent across restarts.
    let reopened: Arc<Manager> = Manager::open(manager.paths.clone()).await.unwrap();
    assert_eq!(listed(&reopened, &child).await, ListStatus::Inactive);
}

#[tokio::test]
async fn idle_cleanup_keeps_a_child_without_an_agent_while_its_coordinator_runs() {
    let (root, manager) = manager().await;
    let (parent, child) = split_child(&manager, root.path()).await;
    assert_eq!(kept(&manager, &child).await, None);
    // In any turn state, since its subagents may work on past its turn.
    let started = start_agent(&manager, &parent).await;
    report(&manager, &parent, AgentState::Idle).await;
    assert_eq!(
        kept(&manager, &child).await,
        Some(CleanupReason::ParentAgentRunning)
    );
    finish(&manager, started).await;
    assert_eq!(kept(&manager, &child).await, None);
}

#[tokio::test]
async fn idle_cleanup_takes_a_child_that_had_its_own_agent_while_its_coordinator_runs() {
    let (root, manager) = manager().await;
    let (parent, child) = split_child(&manager, root.path()).await;
    let _parent_agent = start_agent(&manager, &parent).await;
    finish(&manager, start_agent(&manager, &child).await).await;
    assert_eq!(kept(&manager, &child).await, None);
}

#[tokio::test]
async fn idle_cleanup_keeps_a_child_without_an_agent_while_its_chat_runs() {
    let (root, manager) = manager().await;
    let checkout = repository(root.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    // Not this test's own process, whose children the other tests start.
    let process = ProcessGroup::spawn(std::process::Command::new("sleep").arg("600"));
    let chat = manager
        .start_chat(
            repo.id.clone().into(),
            "planning".into(),
            "claude".into(),
            process.id(),
            None,
            false,
        )
        .await
        .unwrap();
    let child = manager
        .create_workspace_below(
            repo.id.into(),
            "child".into(),
            None,
            Default::default(),
            Some(chat.id.clone()),
        )
        .await
        .unwrap();
    assert_eq!(
        kept(&manager, &child).await,
        Some(CleanupReason::ParentAgentRunning)
    );
    manager.stop_chat(&chat.id).await.unwrap();
    assert_eq!(kept(&manager, &child).await, None);
}
