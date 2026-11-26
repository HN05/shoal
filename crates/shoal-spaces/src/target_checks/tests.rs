use crate::target_checks::{Facts, TargetCheck, Verdict};

trait JudgeAlone {
    /// Judges with no facts beyond the space.
    fn judge_alone(self, space: &Space) -> Verdict;
}

impl JudgeAlone for TargetCheck {
    fn judge_alone(self, space: &Space) -> Verdict {
        self.judge(space, &Facts::default())
    }
}
use crate::{
    model::{Space, SpacePart},
    refusal::Reason,
    state::WorkspaceState,
};

fn workspace(state: WorkspaceState) -> Space {
    let path = std::path::Path::new("/work");
    let mut workspace = shoal_protocol::test_support::workspace("w1", "r1", "fix", path);
    workspace.part.state = state;
    Space::from(workspace)
}

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

/// Some only when the verdict refused rather than left the space out.
fn refused(verdict: Verdict) -> Option<Reason> {
    match verdict {
        Verdict::Refused(error) => Reason::of(&error),
        other => panic!("not refused: {other:?}"),
    }
}

#[test]
fn a_chat_lacks_the_worktree_a_workspace_request_acts_on() {
    let verdict = TargetCheck::ReadyWorkspace.judge_alone(&chat());
    let Verdict::Lacking(Some(refusal)) = verdict else {
        panic!("a chat is not left out with a refusal: {verdict:?}");
    };
    assert_eq!(Reason::of(&refusal), Some(Reason::NotFound));
}

#[test]
fn a_ready_request_refuses_a_workspace_in_another_state_as_not_ready() {
    let ready = TargetCheck::ReadyWorkspace;
    assert!(matches!(
        ready.judge_alone(&workspace(WorkspaceState::Ready)),
        Verdict::Accepted
    ));
    let preparing = ready.judge_alone(&workspace(WorkspaceState::Preparing));
    assert_eq!(refused(preparing), Some(Reason::NotReady));
    let failed = ready.judge_alone(&workspace(WorkspaceState::Failed));
    assert_eq!(refused(failed), Some(Reason::NotReady));
}

#[test]
fn a_settled_request_takes_a_failed_workspace_but_not_a_stopping_one() {
    let settled = TargetCheck::SettledWorkspace;
    assert!(matches!(
        settled.judge_alone(&workspace(WorkspaceState::Failed)),
        Verdict::Accepted
    ));
    let stopping = settled.judge_alone(&workspace(WorkspaceState::Stopping));
    assert_eq!(refused(stopping), Some(Reason::NotReady));
}

#[test]
fn a_checkpoint_takes_a_stopping_workspace_but_not_a_preparing_one() {
    let checkpointed = TargetCheck::CheckpointedWorkspace;
    assert!(matches!(
        checkpointed.judge_alone(&workspace(WorkspaceState::Stopping)),
        Verdict::Accepted
    ));
    let preparing = checkpointed.judge_alone(&workspace(WorkspaceState::Preparing));
    assert!(matches!(preparing, Verdict::Refused(_)));
}

#[test]
fn a_chat_takes_link_changes_while_a_removing_workspace_refuses_them() {
    assert!(matches!(
        TargetCheck::LinkingSpace.judge_alone(&chat()),
        Verdict::Accepted
    ));
    let removing = workspace(WorkspaceState::Removing);
    assert!(matches!(
        TargetCheck::LinkingSpace.judge_alone(&removing),
        Verdict::Refused(_)
    ));
    assert!(matches!(
        TargetCheck::LinkingSpace.judge_alone(&workspace(WorkspaceState::Preparing)),
        Verdict::Accepted
    ));
}

#[test]
fn a_stop_leaves_a_stopped_space_to_its_handler() {
    let stopped = workspace(WorkspaceState::Ready);
    let verdict = TargetCheck::RunningSpace.judge_alone(&stopped);
    assert!(matches!(verdict, Verdict::Lacking(None)));
    assert!(verdict.require().is_ok());
    let mut running = stopped;
    running.running = true;
    assert!(matches!(
        TargetCheck::RunningSpace.judge_alone(&running),
        Verdict::Accepted
    ));
}

#[test]
fn withdrawing_completion_takes_a_done_space_that_is_ready_or_a_chat() {
    let check = TargetCheck::DoneSpace;
    assert!(matches!(
        check.judge_alone(&workspace(WorkspaceState::Ready)),
        Verdict::Lacking(None)
    ));
    let mut done = workspace(WorkspaceState::Failed);
    if let SpacePart::Workspace(worktree) = &mut done.part {
        worktree.done = true;
    }
    assert_eq!(refused(check.judge_alone(&done)), Some(Reason::NotReady));
    let mut chat = chat();
    if let SpacePart::Chat(part) = &mut chat.part {
        part.completion = Some(
            serde_json::from_value(serde_json::json!({"head": "abc", "cleanup": true})).unwrap(),
        );
    }
    assert!(matches!(check.judge_alone(&chat), Verdict::Accepted));
}

#[test]
fn a_failed_workspace_is_refused_with_the_setup_that_retries_it() {
    let mut failed = workspace(WorkspaceState::Failed);
    failed.number = 7.into();
    let Verdict::Refused(refusal) = TargetCheck::ReadyWorkspace.judge_alone(&failed) else {
        panic!("a failed workspace is not refused");
    };
    let message = format!("{refusal:#}");
    assert!(message.contains("shoal setup 7"), "{message}");
}

#[test]
fn a_request_that_waits_takes_a_workspace_an_operation_passes_through() {
    let awaited = TargetCheck::AwaitedReadySpace;
    for state in [WorkspaceState::Ready, WorkspaceState::Stopping] {
        assert!(matches!(
            awaited.judge_alone(&workspace(state)),
            Verdict::Accepted
        ));
    }
    let preparing = awaited.judge_alone(&workspace(WorkspaceState::Preparing));
    assert_eq!(refused(preparing), Some(Reason::NotReady));
    assert!(matches!(awaited.judge_alone(&chat()), Verdict::Accepted));
}

#[test]
fn setup_is_refused_where_work_other_than_the_callers_runs() {
    let setup = TargetCheck::SetupWorkspace;
    let failed = workspace(WorkspaceState::Failed);
    assert!(matches!(setup.judge_alone(&failed), Verdict::Accepted));
    let busy = Facts {
        busy: [failed.id.clone()].into(),
        ..Facts::default()
    };
    assert!(matches!(setup.judge(&failed, &busy), Verdict::Refused(_)));
    let preparing = workspace(WorkspaceState::Preparing);
    assert!(matches!(setup.judge_alone(&preparing), Verdict::Accepted));
    let removing = setup.judge_alone(&workspace(WorkspaceState::Removing));
    assert_eq!(refused(removing), Some(Reason::NotReady));
}

#[test]
fn attaching_leaves_out_a_space_without_an_agent_session() {
    let space = workspace(WorkspaceState::Ready);
    let attach = TargetCheck::SessionSpace;
    assert!(matches!(attach.judge_alone(&space), Verdict::Lacking(None)));
    let sessions = Facts {
        sessions: [space.id.clone()].into(),
        ..Facts::default()
    };
    assert!(matches!(attach.judge(&space, &sessions), Verdict::Accepted));
}

#[test]
fn entering_leaves_out_a_space_whose_directory_is_missing() {
    let space = workspace(WorkspaceState::Ready);
    let enter = TargetCheck::DirectorySpace;
    assert!(matches!(enter.judge_alone(&space), Verdict::Lacking(None)));
    let directories = Facts {
        directories: [space.id.clone()].into(),
        ..Facts::default()
    };
    assert!(matches!(
        enter.judge(&space, &directories),
        Verdict::Accepted
    ));
}

#[test]
fn stopping_processes_takes_a_failed_workspace_or_a_chat_but_not_a_stopping_one() {
    let stop = TargetCheck::SettledSpace;
    let failed = workspace(WorkspaceState::Failed);
    assert!(matches!(stop.judge_alone(&failed), Verdict::Accepted));
    assert!(matches!(stop.judge_alone(&chat()), Verdict::Accepted));
    let stopping = workspace(WorkspaceState::Stopping);
    assert_eq!(refused(stop.judge_alone(&stopping)), Some(Reason::NotReady));
}
