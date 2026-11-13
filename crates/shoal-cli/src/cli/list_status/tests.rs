use clap::ValueEnum;

use super::*;
use crate::model::ReviewMark;

#[test]
fn value_names_spell_the_labels() {
    for status in ListStatus::value_variants() {
        let name = status.to_possible_value().unwrap();
        assert_eq!(
            name.get_name(),
            status
                .label()
                .replace(" (", " ")
                .replace(')', "")
                .replace(' ', "-")
        );
    }
}

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
fn a_done_workspace_with_stopped_work_is_finished() {
    let mut workspace = coordinated(AgentState::Idle);
    assert_eq!(
        ListStatus::of_workspace(&workspace, true),
        ListStatus::Stopped
    );
    workspace.done = true;
    assert_eq!(
        ListStatus::of_workspace(&workspace, true),
        ListStatus::Finished
    );
}

#[test]
fn a_done_chat_that_is_stopped_is_finished() {
    let chat = |completion: serde_json::Value| -> Chat {
        serde_json::from_value(serde_json::json!({
            "id": "c", "name": "talk", "agent": "claude",
            "started_at": 0, "completion": completion,
        }))
        .expect("a chat record")
    };
    assert_eq!(
        ListStatus::of_chat(&chat(serde_json::Value::Null)),
        ListStatus::Stopped
    );
    let done = serde_json::json!({"head": "abc", "cleanup": true, "error": null});
    assert_eq!(ListStatus::of_chat(&chat(done)), ListStatus::Finished);
}

#[test]
fn only_an_idle_agent_shows_its_sleep() {
    let status = |state: AgentState, sleep: Option<Sleep>| {
        let mut workspace = coordinated(AgentState::Idle);
        workspace.agent_state = Some(crate::model::AgentStatus {
            state,
            since: crate::time::UnixTime::EPOCH,
            prompt_cache_expires_at: None,
        });
        workspace.sleep = sleep;
        ListStatus::of_workspace(&workspace, false)
    };
    assert_eq!(status(AgentState::Idle, None), ListStatus::Waiting);
    assert_eq!(
        status(AgentState::Idle, Some(Sleep::Turn)),
        ListStatus::Asleep
    );
    // A permanent sleep outlasts the turns the agent works.
    assert_eq!(
        status(AgentState::Working, Some(Sleep::Permanent)),
        ListStatus::Working
    );
}

#[test]
fn an_agent_shoal_brings_back_shows_asleep_rather_than_stopped_or_queued() {
    let mut workspace = coordinated(AgentState::Idle);
    workspace.asleep = Some(crate::model::Asleep {
        reason: "critical memory pressure".into(),
        wakes_when: "an update arrives for it".into(),
    });
    // Its stop saved work, which comes back on its own.
    assert_eq!(
        ListStatus::of_workspace(&workspace, true),
        ListStatus::Asleep
    );
    let mut restore: QueueEntry = serde_json::from_value(serde_json::json!({
        "id": "entry", "kind": "restore", "repository_id": "repo", "name": "work",
        "agent": "claude", "issue_title": null, "state": "queued", "position": 1,
        "created_at": 0, "space_id": workspace.id,
    }))
    .unwrap();
    let statuses = |restore: &QueueEntry| crate::cli::ui::Statuses {
        stopped: [workspace.id.clone()].into(),
        queued: vec![restore.clone()],
    };
    assert_eq!(statuses(&restore).shown(&workspace), [ListStatus::Asleep]);
    restore.state = QueueState::Starting;
    assert_eq!(statuses(&restore).of(&workspace), ListStatus::Starting);
}

#[test]
fn a_blocked_workspace_ranks_below_needs_input_and_above_stopped_work() {
    let mut workspace = coordinated(AgentState::Idle);
    workspace.progress = Some(crate::client::progress::Progress {
        status: None,
        steps: Vec::new(),
        current: None,
        blockers: vec![crate::client::progress::RecordedBlocker {
            kind: crate::state::BlockerKind::Issue,
            target: "https://forge.example/o/r/issues/1".into(),
            name: "https://forge.example/o/r/issues/1".into(),
            reason: "Needs it".into(),
            lookup_failed: None,
        }],
        updated_at: crate::time::UnixTime::EPOCH,
    });
    assert_eq!(
        ListStatus::of_workspace(&workspace, false),
        ListStatus::Blocked
    );
    // Shoal stops a blocked agent, which stays blocked.
    assert_eq!(
        ListStatus::of_workspace(&workspace, true),
        ListStatus::Blocked
    );
    workspace.agent_state = Some(crate::model::AgentStatus {
        state: AgentState::Waiting,
        since: crate::time::UnixTime::EPOCH,
        prompt_cache_expires_at: None,
    });
    assert_eq!(
        ListStatus::of_workspace(&workspace, false),
        ListStatus::NeedsInput
    );
}

#[test]
fn a_queued_agent_gives_its_status_to_a_workspace_where_nothing_runs() {
    let entry: QueueEntry = serde_json::from_value(serde_json::json!({
        "id": "entry", "kind": "restore", "repository_id": "repo", "name": "child",
        "agent": "claude", "state": "queued", "position": 2, "created_at": 0,
        "workspace_id": "child",
    }))
    .expect("a queue entry");
    let mut workspace = coordinated(AgentState::Idle);
    let status = |workspace: &Workspace, stopped| {
        ListStatus::of_listed_workspace(workspace, stopped, Some(&entry))
    };
    assert_eq!(status(&workspace, true), ListStatus::Queued);
    workspace.running = true;
    assert_eq!(status(&workspace, false), ListStatus::ProcessRunning);
    workspace.running = false;
    workspace.state = WorkspaceState::Removing;
    assert_eq!(status(&workspace, true), ListStatus::Removing);
}
