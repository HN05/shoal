use serde_json::json;
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
        queued: vec![entry],
        ..around()
    };
    let summary = summary(Space::from(workspace), None, &around);
    assert_eq!(summary.status, ListStatus::Queued);
}

fn around() -> Around {
    Around {
        stopped: HashSet::new(),
        queued: Vec::new(),
        questions: Vec::new(),
        approvals: Vec::new(),
        parents: HashMap::new(),
        agents: HashSet::new(),
        running: HashMap::new(),
    }
}

/// An agent of the space `space` as the daemon lists it, named `agent`,
/// stopped by Shoal when `stopped`.
fn agent_of(space: &str, agent: Option<&str>, stopped: bool) -> RunningAgent {
    let protection = json!({"reason": "memory", "resumes_when": "memory frees"});
    serde_json::from_value(json!({
        "execution": format!("{space}-execution"), "space_id": space, "agent": agent,
        "restorable": true, "stopped": stopped.then_some(protection),
    }))
    .unwrap()
}

#[test]
fn a_space_reports_the_agent_it_runs_unless_shoal_stopped_it_or_its_name_is_unknown() {
    let running = running_agents(vec![
        agent_of("working", Some("codex"), false),
        agent_of("protected", Some("claude"), true),
        agent_of("disconnected", None, false),
    ]);
    let codex = Agent::from("codex");
    assert_eq!(
        running,
        [(SpaceId::from("working".to_owned()), codex)].into()
    );
}

#[test]
fn a_space_reports_its_parent_on_another_machine_and_its_agent() {
    let workspace = Workspace::new_record(
        "repo".into(),
        "work".into(),
        "/work".into(),
        shoal_protocol::model::Head::Branch("work".into()),
        WorkspaceState::Ready,
    );
    let elsewhere = SpaceId::from("coordinator-elsewhere".to_owned());
    let around = Around {
        parents: [(workspace.id.clone(), elsewhere.clone())].into(),
        agents: [workspace.id.clone()].into(),
        ..around()
    };
    let summary = summary(Space::from(workspace), None, &around);
    assert_eq!(summary.parent, Some(elsewhere));
    assert!(summary.agent);
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
