use crate::{protocol::operations::QueuedLaunch, start_plan::QueuedStart};

use super::*;

/// The plan of a start of an agent told to fix the login in `workspace`.
fn opening(workspace: &Workspace) -> String {
    let launch = QueuedLaunch {
        prompt: Some("Fix the login".into()),
        ..QueuedLaunch::new("claude".parse().unwrap())
    };
    let request = crate::start_plan::queued_agent_request(workspace, launch, None).unwrap();
    request.plan
}

/// A start of the plan `opening` gives a new `child` workspace, admitted
/// to the only slot, which took its entry and then failed its setup.
async fn failed_start(fixture: &Fixture) -> (Workspace, QueueEntry) {
    limit_globally(fixture, 1);
    let daemon = &*fixture.daemon;
    let child = fixture.workspace("child").await;
    let request = QueueRequest {
        plan: opening(&child),
        ..launch_in(&child, false)
    };
    let starting = fixture.agents.queue_add(daemon, request).await;
    let starting = starting.unwrap().entry.unwrap();
    assert_eq!(starting.state, QueueState::Starting);
    (fixture.agents)
        .queue_attach(daemon, &starting.id, &SpaceSelector::id(&child.id))
        .await
        .unwrap();
    let failed = WorkspaceState::Failed;
    (fixture.set_state(&child.id, failed, Some("setup failed"))).await;
    (child, starting)
}

async fn plan(fixture: &Fixture, entry: &QueueEntry) -> QueuedStart {
    let id = entry.id.clone();
    let plan: String = (fixture.agents.store)
        .run(move |db| {
            Ok(
                db.query_row("SELECT plan FROM agent_queue WHERE id=?1", [id], |row| {
                    row.get(0)
                })?,
            )
        })
        .await
        .unwrap();
    serde_json::from_str(&plan).unwrap()
}

/// Queue the agent of the new `workspace`, starting `now` or not, to
/// wait for its preparation.
async fn awaiting_preparation(fixture: &Fixture, workspace: &Workspace, now: bool) -> QueueEntry {
    let launch = QueuedLaunch {
        now,
        ..QueuedLaunch::new("claude".parse().unwrap())
    };
    let space = fixture.space(&workspace.id);
    (fixture.agents)
        .queue_start_agent(&*fixture.daemon, (&space, true), launch, (None, Vec::new()))
        .await
        .unwrap()
        .entry
        .unwrap()
}

/// A new workspace `child` whose preparation has yet to set it up.
async fn preparing(fixture: &Fixture) -> Workspace {
    let workspace = fixture.workspace("child").await;
    let preparing = WorkspaceState::Preparing;
    fixture.set_state(&workspace.id, preparing, None).await;
    workspace
}

/// Let the launches waiting for the setup of `workspace` go.
async fn release(fixture: &Fixture, workspace: &Workspace) {
    (fixture
        .agents
        .release_setup_waits(&*fixture.daemon, &workspace.id))
    .await
    .unwrap();
}

/// The entry `id` as the queue lists it.
async fn listed(fixture: &Fixture, id: &QueueEntryId) -> QueueEntry {
    let entries = live(fixture).await;
    entries.into_iter().find(|queued| queued.id == *id).unwrap()
}

#[tokio::test]
async fn a_start_whose_setup_failed_waits_for_a_retried_setup_without_its_slot() {
    let fixture = Fixture::new().await;
    let (child, starting) = failed_start(&fixture).await;
    // The CLI that started it still holds the old entry.
    fixture.agents.hold_queue_entry(&starting.id).await.unwrap();
    dispatch(&fixture).await;

    let entries = fixture
        .agents
        .queue_entries(&*fixture.daemon)
        .await
        .unwrap();
    let [waiting] = entries.as_slice() else {
        panic!("{entries:?}");
    };
    assert_ne!(waiting.id, starting.id);
    assert_eq!(waiting.state, QueueState::Queued);
    assert_eq!(waiting.opens.as_ref(), Some(&child.id));
    assert_eq!(waiting.blockers, ["setup of child"]);
    let plan = plan(&fixture, waiting).await;
    assert_eq!(plan.launch.prompt.as_deref(), Some("Fix the login"));
    assert!(admitted(&add(&fixture, "other").await));
}

#[tokio::test]
async fn a_start_now_waits_for_its_preparation_then_starts_past_the_limits() {
    let fixture = Fixture::new().await;
    limit_globally(&fixture, 1);
    add(&fixture, "first").await;
    let workspace = preparing(&fixture).await;
    let entry = awaiting_preparation(&fixture, &workspace, true).await;
    assert_eq!(entry.state, QueueState::Queued);
    assert_eq!(entry.blockers, vec!["setup of child".to_owned()]);
    release(&fixture, &workspace).await;
    assert_eq!(
        listed(&fixture, &entry.id).await.state,
        QueueState::Starting
    );
}

#[tokio::test]
async fn a_queued_start_waits_for_its_preparation_then_for_the_limits() {
    let fixture = Fixture::new().await;
    limit_globally(&fixture, 1);
    add(&fixture, "first").await;
    let workspace = preparing(&fixture).await;
    let entry = awaiting_preparation(&fixture, &workspace, false).await;
    release(&fixture, &workspace).await;
    let waiting = listed(&fixture, &entry.id).await;
    assert_eq!(waiting.state, QueueState::Queued);
    assert!(waiting.blockers.is_empty(), "{waiting:?}");
}

#[tokio::test]
async fn a_new_workspace_ready_at_once_holds_its_agent_until_its_preparation_lets_it() {
    let fixture = Fixture::new().await;
    // Created ready, as a copy with nothing to set up is.
    let workspace = fixture.workspace("child").await;
    assert_eq!(workspace.state, WorkspaceState::Ready);
    let entry = awaiting_preparation(&fixture, &workspace, false).await;
    assert_eq!(entry.state, QueueState::Queued);
    assert_eq!(entry.blockers, vec!["setup of child".to_owned()]);
}

#[tokio::test]
async fn a_held_start_now_is_left_to_its_holder_when_its_setup_releases_it() {
    let fixture = Fixture::new().await;
    limit_globally(&fixture, 1);
    add(&fixture, "first").await;
    let workspace = preparing(&fixture).await;
    let entry = awaiting_preparation(&fixture, &workspace, true).await;
    fixture.agents.agent_queue().hold(&entry.id);
    release(&fixture, &workspace).await;
    assert_eq!(listed(&fixture, &entry.id).await.state, QueueState::Queued);
}
