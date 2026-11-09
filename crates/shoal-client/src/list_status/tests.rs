use super::*;
use crate::model::ReviewMark;

/// A ready workspace whose coordinator's agent does its work and reports
/// `coordinator`.
fn coordinated(coordinator: AgentState) -> Workspace {
    let mut workspace = Workspace::new_record(
        "repo".into(),
        "child".into(),
        "/child".into(),
        Some("child".into()),
        WorkspaceState::Ready,
    );
    workspace.coordinator_state = Some(coordinator);
    workspace
}

fn mark(stale: bool) -> ReviewMark {
    ReviewMark {
        kind: None,
        url: None,
        head: "abc".into(),
        created_at: crate::time::UnixTime::EPOCH,
        stale: Some(stale),
        warnings: Vec::new(),
    }
}

#[test]
fn a_working_coordinator_replaces_only_inactive_and_outdated_marks() {
    let status = |workspace: &Workspace| ListStatus::of_workspace(workspace, false);
    let mut workspace = coordinated(AgentState::Working);
    assert_eq!(status(&workspace), ListStatus::ParentWorking);
    workspace.review = vec![mark(true)];
    assert_eq!(status(&workspace), ListStatus::ParentWorking);
    workspace.review = vec![mark(false)];
    assert_eq!(status(&workspace), ListStatus::Ready);
    workspace.review.clear();
    workspace.running = true;
    assert_eq!(status(&workspace), ListStatus::ProcessRunning);
    assert_eq!(
        ListStatus::of_workspace(&workspace, true),
        ListStatus::Stopped
    );
    for idle in [AgentState::Idle, AgentState::Waiting] {
        assert_eq!(status(&coordinated(idle)), ListStatus::Inactive);
    }
}

#[test]
fn a_done_workspace_whose_agent_works_again_shows_it_working() {
    let mut workspace = coordinated(AgentState::Idle);
    workspace.done = true;
    let status = |workspace: &Workspace| ListStatus::of_workspace(workspace, false);
    assert_eq!(status(&workspace), ListStatus::Finished);
    workspace.agent_state = Some(crate::model::AgentStatus {
        state: AgentState::Working,
        since: crate::time::UnixTime::EPOCH,
        prompt_cache_expires_at: None,
    });
    assert_eq!(status(&workspace), ListStatus::Working);
}

#[test]
fn a_done_chat_whose_agent_works_again_shows_it_working() {
    let mut chat: Chat = serde_json::from_value(serde_json::json!({
        "id": "c",
        "name": "talk",
        "agent": "claude",
        "pid": 1,
        "started_at": 0,
        "completion": {"head": "abc", "cleanup": true, "error": null},
    }))
    .expect("a chat record");
    assert_eq!(ListStatus::of_chat(&chat), ListStatus::Finished);
    chat.agent_state = Some(crate::model::AgentStatus {
        state: AgentState::Working,
        since: crate::time::UnixTime::EPOCH,
        prompt_cache_expires_at: None,
    });
    assert_eq!(ListStatus::of_chat(&chat), ListStatus::Working);
}
