use shoal_protocol::{model::Workspace, state::WorkspaceState};

use super::*;

#[test]
fn a_workspace_whose_agent_waits_in_the_queue_reports_queued() {
    let workspace = Workspace::new_record(
        "repo".into(),
        "work".into(),
        "/work".into(),
        shoal_protocol::model::Head::Branch("work".into()),
        WorkspaceState::Ready,
    );
    let entry: QueueEntry = serde_json::from_value(serde_json::json!({
        "id": "entry", "kind": "launch", "repository_id": "repo", "name": "work",
        "agent": "claude", "state": "queued", "position": 1, "created_at": 0,
        "space_id": workspace.id,
    }))
    .unwrap();
    let around = Around {
        stopped: HashSet::new(),
        queued: vec![entry],
        questions: Vec::new(),
        approvals: Vec::new(),
    };
    let summary = summary(Space::from(workspace), None, &around);
    assert_eq!(summary.status, ListStatus::Queued);
}

#[tokio::test(start_paused = true)]
async fn a_changed_origin_reaches_reports_once_the_read_one_goes_stale() {
    let mut repository = Repository {
        id: "repo".into(),
        path: "/repo".into(),
        source: "https://forge.example/me/old.git".into(),
        last_used: 0,
        name: Some("app".into()),
        workspaces_dir: "/shoal/app".into(),
        contribution: false,
    };
    let mut origins = Origins::default();
    let first = origins.of(&repository).await.origin;
    repository.source = "https://forge.example/me/new.git".into();
    assert_eq!(origins.of(&repository).await.origin, first);
    tokio::time::advance(ORIGIN_FRESH).await;
    assert_ne!(origins.of(&repository).await.origin, first);
}
