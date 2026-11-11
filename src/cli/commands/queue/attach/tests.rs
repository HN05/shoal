use super::*;
use crate::model::Workspace;
use crate::state::{QueueState, WorkspaceState};
use crate::time::UnixTime;

fn workspace(repository: &str, name: &str, number: u32) -> Workspace {
    Workspace {
        number: number.into(),
        ..Workspace::new_record(
            repository.into(),
            name.into(),
            format!("/work/{number}").into(),
            Some(format!("branch-{number}")),
            WorkspaceState::Ready,
        )
    }
}

fn launch(name: &str) -> QueueEntry {
    QueueEntry {
        narrowing: Default::default(),
        mode: None,
        id: "entry".into(),
        kind: QueueKind::Launch,
        repository_id: "repo".into(),
        name: name.into(),
        agent: "claude".into(),
        issue_title: None,
        state: QueueState::Starting,
        position: None,
        created_at: UnixTime::from_seconds(0),
        workspace_id: None,
        execution: None,
        opens: None,
        waiting_for: Vec::new(),
        waits_for_update: false,
        base: None,
        job: None,
        held_by: None,
    }
}

#[test]
fn a_finished_launch_is_found_among_the_workspaces_opened_while_waiting() {
    let older = workspace("repo", "Fix login", 1);
    let other_repo = workspace("other", "Fix login", 2);
    let opened = workspace("repo", "Fix login", 3);
    let existing = [older.id.clone(), other_repo.id.clone()];
    let workspaces = vec![older, other_repo, opened.clone()];

    // An older workspace with the launch's name is not the one it opened.
    let found = opened_workspace(workspaces, &launch("Fix login"), &existing).unwrap();

    assert_eq!(found, Some(opened.id));
}

#[test]
fn a_finished_launch_whose_name_two_new_workspaces_share_is_refused() {
    let workspaces = vec![
        workspace("repo", "Fix login", 1),
        workspace("repo", "Fix login", 2),
    ];

    let refused = opened_workspace(workspaces, &launch("Fix login"), &[]).unwrap_err();

    assert!(refused.to_string().contains("1, 2"), "{refused}");
}
