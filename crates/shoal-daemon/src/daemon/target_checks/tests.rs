use super::{TargetCheck, Verdict};
use crate::{
    model::{Space, SpacePart},
    refusal::Reason,
    state::WorkspaceState,
};

/// A workspace in `state`.
fn workspace(state: WorkspaceState) -> Space {
    let path = std::path::Path::new("/work");
    let mut workspace = shoal_protocol::test_support::workspace("w1", "r1", "fix", path);
    workspace.part.state = state;
    Space::from(workspace)
}

/// A chat without a repository.
fn chat() -> Space {
    serde_json::from_value(serde_json::json!({
        "id": "c1",
        "number": 2,
        "name": "notes",
        "repository_id": null,
        "workdir": {"kind": "root_dir", "path": "/root"},
        "kind": "chat",
        "started_at": 0,
    }))
    .unwrap()
}

/// The reason `verdict` refuses with, which says it was refused rather
/// than left out.
fn refused(verdict: Verdict) -> Option<Reason> {
    match verdict {
        Verdict::Refused(error) => Reason::of(&error),
        other => panic!("not refused: {other:?}"),
    }
}

#[test]
fn a_chat_lacks_the_worktree_a_workspace_request_acts_on() {
    let verdict = TargetCheck::ReadyWorkspace.judge(&chat());
    let Verdict::Lacking(Some(refusal)) = verdict else {
        panic!("a chat is not left out with a refusal: {verdict:?}");
    };
    assert_eq!(Reason::of(&refusal), Some(Reason::NotFound));
}

#[test]
fn a_ready_request_refuses_a_workspace_in_another_state_as_not_ready() {
    let ready = TargetCheck::ReadyWorkspace;
    assert!(matches!(
        ready.judge(&workspace(WorkspaceState::Ready)),
        Verdict::Accepted
    ));
    let preparing = ready.judge(&workspace(WorkspaceState::Preparing));
    assert_eq!(refused(preparing), Some(Reason::NotReady));
    let failed = ready.judge(&workspace(WorkspaceState::Failed));
    assert_eq!(refused(failed), Some(Reason::NotReady));
}

#[test]
fn a_settled_request_takes_a_failed_workspace_but_not_a_stopping_one() {
    let settled = TargetCheck::SettledWorkspace;
    assert!(matches!(
        settled.judge(&workspace(WorkspaceState::Failed)),
        Verdict::Accepted
    ));
    let stopping = settled.judge(&workspace(WorkspaceState::Stopping));
    assert_eq!(refused(stopping), Some(Reason::NotReady));
}

#[test]
fn a_checkpoint_takes_a_stopping_workspace_but_not_a_preparing_one() {
    let checkpointed = TargetCheck::CheckpointedWorkspace;
    assert!(matches!(
        checkpointed.judge(&workspace(WorkspaceState::Stopping)),
        Verdict::Accepted
    ));
    let preparing = checkpointed.judge(&workspace(WorkspaceState::Preparing));
    assert!(matches!(preparing, Verdict::Refused(_)));
}

#[test]
fn a_chat_takes_link_changes_while_a_removing_workspace_refuses_them() {
    assert!(matches!(
        TargetCheck::LinkingSpace.judge(&chat()),
        Verdict::Accepted
    ));
    let removing = workspace(WorkspaceState::Removing);
    assert!(matches!(
        TargetCheck::LinkingSpace.judge(&removing),
        Verdict::Refused(_)
    ));
    assert!(matches!(
        TargetCheck::LinkingSpace.judge(&workspace(WorkspaceState::Preparing)),
        Verdict::Accepted
    ));
}

#[test]
fn a_stop_leaves_a_stopped_space_to_its_handler() {
    let stopped = workspace(WorkspaceState::Ready);
    let verdict = TargetCheck::RunningSpace.judge(&stopped);
    assert!(matches!(verdict, Verdict::Lacking(None)));
    assert!(verdict.require().is_ok());
    let mut running = stopped;
    running.running = true;
    assert!(matches!(
        TargetCheck::RunningSpace.judge(&running),
        Verdict::Accepted
    ));
}

#[test]
fn withdrawing_completion_takes_a_done_space_that_is_ready_or_a_chat() {
    let check = TargetCheck::DoneSpace;
    assert!(matches!(
        check.judge(&workspace(WorkspaceState::Ready)),
        Verdict::Lacking(None)
    ));
    let mut done = workspace(WorkspaceState::Failed);
    if let SpacePart::Workspace(worktree) = &mut done.part {
        worktree.done = true;
    }
    assert_eq!(refused(check.judge(&done)), Some(Reason::NotReady));
    let mut chat = chat();
    if let SpacePart::Chat(part) = &mut chat.part {
        part.completion = Some(
            serde_json::from_value(serde_json::json!({"head": "abc", "cleanup": true})).unwrap(),
        );
    }
    assert!(matches!(check.judge(&chat), Verdict::Accepted));
}

#[test]
fn a_failed_workspace_is_refused_with_the_setup_that_retries_it() {
    let mut failed = workspace(WorkspaceState::Failed);
    failed.number = 7.into();
    let Verdict::Refused(refusal) = TargetCheck::ReadyWorkspace.judge(&failed) else {
        panic!("a failed workspace is not refused");
    };
    let message = format!("{refusal:#}");
    assert!(message.contains("shoal setup 7"), "{message}");
}
