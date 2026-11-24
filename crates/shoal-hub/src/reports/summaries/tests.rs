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
        saved: HashSet::new(),
        stoppable: HashSet::new(),
        queued: Vec::new(),
        threads: Vec::new(),
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

/// A ready workspace named `name`, done when `done`.
fn workspace(name: &str, done: bool) -> Workspace {
    let mut workspace = Workspace::new_record(
        "repo".into(),
        name.into(),
        format!("/{name}").into(),
        shoal_protocol::model::Head::Branch(name.into()),
        WorkspaceState::Ready,
    );
    workspace.done = done;
    workspace
}

/// The launch of an agent in `space` that failed to start.
fn failed_launch(space: &SpaceId) -> QueueEntry {
    serde_json::from_value(json!({
        "id": format!("{space}-launch"), "kind": "launch", "repository_id": "repo",
        "name": "work", "agent": "claude", "state": "failed", "created_at": 0,
        "space_id": space,
    }))
    .unwrap()
}

#[test]
fn a_space_reports_whether_stop_all_would_stop_it() {
    let (running, idle) = (workspace("running", false), workspace("idle", false));
    let around = Around {
        stoppable: [running.id.clone()].into(),
        ..around()
    };
    assert!(summary(Space::from(running), None, &around).stoppable);
    assert!(!summary(Space::from(idle), None, &around).stoppable);
}

#[test]
fn a_space_reports_saved_work_before_a_failed_launch_unless_its_assignment_is_done() {
    let saved = workspace("saved", false);
    let done = workspace("done", true);
    let failed = workspace("failed", false);
    let done_and_failed = workspace("done-and-failed", true);
    let nothing = workspace("nothing", false);
    let around = Around {
        saved: [&saved, &done, &done_and_failed]
            .map(|workspace| workspace.id.clone())
            .into(),
        queued: [&saved, &failed, &done_and_failed]
            .map(|workspace| failed_launch(&workspace.id))
            .into(),
        ..around()
    };
    let resumable = |workspace: Workspace| summary(Space::from(workspace), None, &around).resumable;
    assert_eq!(resumable(saved), Some(Resumable::Saved));
    assert_eq!(resumable(done), Some(Resumable::Done));
    assert_eq!(resumable(failed), Some(Resumable::FailedLaunch));
    assert_eq!(resumable(done_and_failed), Some(Resumable::FailedLaunch));
    assert_eq!(resumable(nothing), None);
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
        shared_url: None,
    };
    let mut origins = Origins::default();
    let first = origins.of(&repository).await.origin;
    repository.source = "https://forge.example/me/new.git".into();
    assert_eq!(origins.of(&repository).await.origin, first);
    tokio::time::advance(ORIGIN_FRESH).await;
    assert_ne!(origins.of(&repository).await.origin, first);
}
