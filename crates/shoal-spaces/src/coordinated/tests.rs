use crate::stand_in::Fixture;
use crate::{model::Workspace, state::AgentState};
use shoal_protocol::space_names::SpaceSelector;

/// A workspace `parent`, whose agent runs, and a workspace split from it.
async fn split_child(manager: &Fixture) -> (Workspace, Workspace) {
    let repo = manager.repository("repo").await;
    let parent = manager
        .create_workspace(&repo.id, "parent".into(), None, None, None)
        .await
        .unwrap();
    manager.run_agent(&parent.id).await;
    let child = manager
        .spaces
        .create_child(
            &**manager,
            &SpaceSelector::id(&parent.id),
            "child".into(),
            None,
            Default::default(),
        )
        .await
        .unwrap();
    (parent, child)
}

/// Whether lists show `workspace` as its coordinator's work: it runs no
/// agent or command of its own, and the agent above it that does its work is
/// working.
async fn parent_working(manager: &Fixture, workspace: &Workspace) -> bool {
    let listed = manager
        .spaces
        .listed_spaces(&**manager, None)
        .await
        .unwrap();
    let found = (listed.into_iter())
        .find_map(|space| space.into_workspace().ok().filter(|w| w.id == workspace.id))
        .unwrap();
    found.agent_state.is_none()
        && !found.running
        && found.coordinator_state == Some(AgentState::Working)
}

#[tokio::test]
async fn a_child_without_an_agent_lists_as_its_coordinators_work() {
    let manager = Fixture::new().await;
    let (parent, child) = split_child(&manager).await;
    assert!(!parent_working(&manager, &child).await);
    manager.report(&parent.id, AgentState::Working);
    assert!(parent_working(&manager, &child).await);
    // A grandchild split from it is the same agent's work.
    let grandchild = manager
        .spaces
        .create_child(
            &*manager,
            &SpaceSelector::id(&child.id),
            "grandchild".into(),
            None,
            Default::default(),
        )
        .await
        .unwrap();
    assert!(parent_working(&manager, &grandchild).await);
    manager.report(&parent.id, AgentState::Waiting);
    assert!(!parent_working(&manager, &child).await);
}
