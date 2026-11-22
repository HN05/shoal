use crate::execution::recovery;
use crate::executions::ExecutionKind;
use crate::model::Space;
use crate::paths::StateFiles;
use crate::shutdown_stop::*;
use crate::stand_in::Fixture;
use shoal_ids::execution_ids::ExecutionId;
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

#[tokio::test]
async fn suspending_waits_for_an_admission_pass_in_progress() {
    use futures_util::FutureExt;
    let queue = crate::queue::AgentQueue::default();
    let admitting = queue.lock_admission().await;
    let mut suspending = Box::pin(queue.suspend(true));
    assert!(suspending.as_mut().now_or_never().is_none());
    assert!(!queue.suspended());
    drop(admitting);
    suspending.await;
    assert!(queue.suspended());
}

fn workspace(root: &std::path::Path, name: &str, done: bool) -> Space {
    let mut workspace = crate::model::Workspace::new_record(
        "repo".into(),
        name.into(),
        root.join(name),
        crate::model::Head::Branch(name.into()),
        crate::state::WorkspaceState::Ready,
    );
    workspace.done = done;
    workspace.number = if done { 7 } else { 3 }.into();
    Space::from(workspace)
}

#[test]
fn only_requested_or_queued_restores_continue_without_the_user() {
    let root = tempfile::tempdir_in("/tmp").unwrap();
    let paths = crate::paths::Paths::for_test(root.path());
    let open = workspace(root.path(), "open", false);
    let none = HashSet::new();
    assert!(!waits_for_resume(&paths, &open, &none).unwrap());
    recovery::save_record(
        &paths,
        &open.id,
        &ExecutionId::from("requested"),
        &shoal_protocol::executions::TrackedAgent::named("claude"),
        None,
        None,
    )
    .unwrap();
    let request = paths
        .space_state(&open.id)
        .join("requested.restore-request.json");
    std::fs::write(&request, "{}").unwrap();
    assert!(!waits_for_resume(&paths, &open, &none).unwrap());
    // A done workspace's request waits for `shoal resume` to name it.
    let mut done = open.clone();
    if let crate::model::SpacePart::Workspace(worktree) = &mut done.part {
        worktree.done = true;
    }
    assert!(waits_for_resume(&paths, &done, &none).unwrap());
    recovery::save_record(
        &paths,
        &open.id,
        &ExecutionId::from("unattended"),
        &shoal_protocol::executions::TrackedAgent::named("claude"),
        None,
        None,
    )
    .unwrap();
    assert!(waits_for_resume(&paths, &open, &none).unwrap());
    let queued = HashSet::from([ExecutionId::from("unattended")]);
    assert!(!waits_for_resume(&paths, &open, &queued).unwrap());
    // A last session, as `shoal daemon stop` keeps a stopped agent, waits.
    let kept = workspace(root.path(), "kept", false);
    let state = paths.space_state(&kept.id);
    std::fs::create_dir_all(&state).unwrap();
    std::fs::write(
        state.join("kept.last-session.json"),
        r#"{"agent":"claude","mode":"worker"}"#,
    )
    .unwrap();
    assert!(waits_for_resume(&paths, &kept, &none).unwrap());
}

#[test]
fn the_notification_names_where_each_space_went_and_continues_only_those_left() {
    use super::{handovers, staying};
    use shoal_protocol::hub::{packages::Handed, placement::Decision};
    use shoal_schedule::placement::{Policy, Reason};

    let root = tempfile::tempdir_in("/tmp").unwrap();
    let (gone, left) = (
        workspace(root.path(), "gone", false),
        workspace(root.path(), "left", false),
    );
    let decision = Decision {
        machine: "desk-id".into(),
        name: "desk".into(),
        reason: Reason::NoHeavyAgents,
        policy: Policy::Balanced,
        unanswered: None,
        skipped: Vec::new(),
    };
    let unsure = workspace(root.path(), "unsure", false);
    let handed = [
        (gone.clone(), Handed::Went(decision.clone())),
        (unsure.clone(), Handed::Unconfirmed(decision)),
        (left.clone(), Handed::Stayed("the hub is offline".into())),
    ];
    let told = handovers(&handed);
    let [went, maybe, stayed] = told.split("; ").collect::<Vec<_>>()[..] else {
        panic!("{told}");
    };
    assert!(went.starts_with("gone") && went.contains("desk"), "{told}");
    assert!(
        maybe.starts_with("unsure") && maybe.contains("desk"),
        "{told}"
    );
    assert!(
        stayed.starts_with("left") && stayed.contains("the hub is offline"),
        "{told}"
    );
    // A space this machine left to the hub keeps no restore here, confirmed
    // or not, so only the one that stays continues here.
    let here = staying(&[gone, unsure, left.clone()], &handed);
    let here: Vec<_> = here.iter().map(|space| &space.id).collect();
    assert_eq!(here, [&left.id]);
}

#[test]
fn done_workspaces_are_named_since_resume_all_skips_them() {
    let root = tempfile::tempdir_in("/tmp").unwrap();
    let (open, done) = (
        workspace(root.path(), "open", false),
        workspace(root.path(), "finished", true),
    );
    let (open_only, done_only) = (continuation(&[&open]), continuation(&[&done]));
    assert!(open_only.contains("--all") && !open_only.contains("resume 7"));
    assert!(done_only.contains("resume 7") && !done_only.contains("--all"));
    let both = continuation(&[&open, &done]);
    assert!(
        both.contains("--all") && both.contains("resume 7"),
        "{both}"
    );
}

#[tokio::test]
async fn a_shutdown_keeps_a_protection_stopped_agent_from_resuming() {
    let fixture = Arc::new(Fixture::new().await);
    let workspace = fixture.workspace("worker").await;
    let execution = fixture
        .agents
        .begin_execution(
            &*fixture.daemon,
            workspace.id.as_str(),
            None,
            ExecutionKind::Command,
            None,
        )
        .await
        .unwrap();
    let id = execution.plan.id.clone();
    fixture
        .agents
        .track_agent(
            &*fixture.daemon,
            &id,
            &shoal_protocol::executions::TrackedAgent::named("claude"),
            &workspace.id,
            &workspace.name,
            true,
            Duration::ZERO,
        )
        .await;
    assert!(
        fixture
            .agents
            .stop_for_overload(&*fixture.daemon, crate::overload::Pressure::Memory)
            .await
    );
    fixture
        .agents
        .queue_agent_restore(&*fixture.daemon, &id)
        .await
        .unwrap();
    // Its waiting wrapper restores it, not the queue, and ends with the system.
    assert!(
        !fixture
            .agents
            .queued_stopped_restores()
            .await
            .unwrap()
            .contains(&id)
    );

    let mut stop = execution.stop.clone();
    assert!(*stop.borrow_and_update());
    fixture.agents.agent_queue().suspend(true).await;
    assert!(
        fixture
            .agents
            .resume_agent_execution(&*fixture.daemon, &id, None)
            .await
            .is_err()
    );
    assert!(*stop.borrow());
    fixture.agents.agent_queue().suspend(false).await;
    assert!(
        fixture
            .agents
            .resume_agent_execution(&*fixture.daemon, &id, None)
            .await
            .unwrap()
    );
    assert!(!*stop.borrow_and_update());
}
