use shoal_protocol::progress::Blocker;
use std::sync::Arc;

use super::super::{Opening, Preparing};
use crate::protocol::QueuedLaunches;
use crate::{
    daemon::{
        manager::Manager,
        test_support::{commit, git, global_config, manager, repository},
        workspace_files::Attacher,
    },
    model::{PreparationWait, Repository, Workspace},
    protocol::operations::{LaunchRequest, StartedWorkspace, WorkspaceStart},
    removal::{InspectionPolicy, UnsavedWork},
    state::{QueueState, WorkspaceState},
};
use shoal_protocol::{assignments::Done, space_names::SpaceSelector};

async fn register(manager: &Manager, temp: &tempfile::TempDir, name: &str) -> Repository {
    let checkout = repository(temp.path(), name);
    manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap()
}

/// A ready workspace on the new branch `name`.
async fn create(manager: &Manager, repository: &Repository, name: &str) -> Workspace {
    manager
        .create_workspace(&repository.id, name.into(), None, None, None)
        .await
        .unwrap()
}

/// Start a new workspace on branch `name` that nothing follows, its agent
/// waiting for `blockers`.
async fn start(
    manager: &Arc<Manager>,
    repository: &Repository,
    name: &str,
    blockers: Vec<Blocker>,
) -> StartedWorkspace {
    let opening = Opening {
        start: WorkspaceStart::Name(name.into()),
        base: None,
        new: Default::default(),
        parent: None,
    };
    let preparing = Preparing {
        issue: None,
        pr: None,
        agent: Some(Box::new(LaunchRequest::new(Some(
            "claude".parse().unwrap(),
        )))),
        waiting: None,
        queue_entry: None,
        blockers,
        unfollowed: true,
        env: None,
    };
    manager
        .start_workspace((&repository.id).into(), opening, preparing, None)
        .await
        .unwrap()
}

async fn read(manager: &Manager, workspace: &Workspace) -> Workspace {
    manager.workspace_by_id(&workspace.id).await.unwrap()
}

async fn done(manager: &Manager, workspace: &Workspace) {
    let selector = SpaceSelector::id(&workspace.id);
    manager
        .mark_space_done(&selector, Done::default(), &Attacher::User)
        .await
        .unwrap();
}

#[tokio::test]
async fn a_blocked_start_creates_its_worktree_on_the_done_blocker_once_it_is_done() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    let api = create(&manager, &repo, "api").await;
    std::fs::write(api.path.join("api"), "api\n").unwrap();
    commit(&api.path, "api");
    let blocker = Blocker::Space(api.id.to_string());
    let started = start(&manager, &repo, "ui", vec![blocker]).await;
    // Recorded at once with its agent, while its preparation waits.
    assert!(started.operation.is_none());
    let entry = started.entry.as_ref().expect("its agent is queued");
    assert_eq!(entry.state, QueueState::Queued);
    let waits = Some(PreparationWait::Blockers {
        blocked_by: vec!["api".into()],
    });
    assert_eq!(started.workspace.preparation_waits, waits);
    let ui = *started.workspace;
    assert_eq!(ui.state, WorkspaceState::Preparing);

    manager.start_waiting_preparations().await.unwrap();
    assert!(!manager.operation_runs_in(&ui.id), "it started early");
    done(&manager, &api).await;
    manager.start_waiting_preparations().await.unwrap();
    assert!(manager.operation_runs_in(&ui.id));
    let ui = read(&manager, &ui).await;
    assert_eq!(ui.preparation_waits, None);
    assert_eq!(
        ui.base_workspace.as_ref().map(|base| &base.id),
        Some(&api.id)
    );
    let selector = SpaceSelector::id(&ui.id);
    let created = manager.create_pending_worktree(&selector).await.unwrap();
    assert!(created.path.join("api").exists());
}

#[tokio::test]
async fn a_stopped_waiting_start_waits_for_its_agent_until_prepared_explicitly() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    let api = create(&manager, &repo, "api").await;
    let blocker = Blocker::Space(api.id.to_string());
    let ui = *start(&manager, &repo, "ui", vec![blocker]).await.workspace;
    let prepare = crate::protocol::operations::OperationSpec::Prepare {
        workspace: SpaceSelector::id(&ui.id),
        steps: Default::default(),
    };
    // While it waits for work, only that work starts it.
    let refused = manager.admit_explicit_preparation(&prepare).await;
    assert!(refused.is_err(), "{refused:?}");

    // Its stop cancels the agent waiting there and keeps the workspace.
    let selector = SpaceSelector::id(&ui.id);
    let stopped = manager
        .stop_space_manually(&selector, false, QueuedLaunches::Cancel)
        .await
        .unwrap();
    assert_eq!(stopped.cancelled.len(), 1);
    // The stop records that it waits for its agent, so a restore at once
    // prepares it, which no pass then starts again.
    let stopped = read(&manager, &ui).await;
    assert_eq!(stopped.preparation_waits, Some(PreparationWait::Stopped));
    manager.admit_explicit_preparation(&prepare).await.unwrap();
    manager.start_waiting_preparations().await.unwrap();
    assert!(!manager.operation_runs_in(&ui.id));
    assert_eq!(read(&manager, &ui).await.preparation_waits, None);
}

#[tokio::test]
async fn a_start_waits_for_disk_space_before_creating_its_worktree() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    let low = "[machine.overload.warning]\ndisk_free_gib = 1000000000\n";
    manager.publish_config(global_config(low));
    let started = start(&manager, &repo, "ui", Vec::new()).await;
    assert!(started.operation.is_none());
    let ui = *started.workspace;
    assert_eq!(ui.preparation_waits, Some(PreparationWait::Disk));

    manager.publish_config(global_config(""));
    manager.start_waiting_preparations().await.unwrap();
    assert!(manager.operation_runs_in(&ui.id));
    // Its branch was never created, so the worktree comes from the default
    // branch as it is now.
    let branches = git(&repo.path, &["branch", "--list", "ui"]);
    assert_eq!(branches.trim(), "");
}

#[tokio::test]
async fn a_waiting_start_goes_with_its_record() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    let api = create(&manager, &repo, "api").await;
    let blocker = Blocker::Space(api.id.to_string());
    let ui = *start(&manager, &repo, "ui", vec![blocker]).await.workspace;
    manager
        .remove_selected(
            &SpaceSelector::id(&ui.id),
            UnsavedWork::All,
            InspectionPolicy::GitOnly,
        )
        .await
        .unwrap();
    assert!(manager.queue_entries().await.unwrap().is_empty());
    done(&manager, &api).await;
    manager.start_waiting_preparations().await.unwrap();
    assert!(!manager.operation_runs_in(&ui.id));
}

#[tokio::test]
async fn a_release_after_its_agent_was_stopped_keeps_the_preparation_waiting() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    let api = create(&manager, &repo, "api").await;
    let blocker = Blocker::Space(api.id.to_string());
    let ui = *start(&manager, &repo, "ui", vec![blocker]).await.workspace;
    done(&manager, &api).await;
    // The stop lands after the pass found the blocker done.
    manager.cancel_waiting_launches(&ui.id).await.unwrap();
    manager.release_preparation(&ui.id, None).await;
    assert!(!manager.operation_runs_in(&ui.id));
    assert!(read(&manager, &ui).await.preparation_waits.is_some());
}

#[tokio::test]
async fn a_stop_after_the_release_leaves_the_launch_to_the_started_preparation() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    let api = create(&manager, &repo, "api").await;
    let blocker = Blocker::Space(api.id.to_string());
    let ui = *start(&manager, &repo, "ui", vec![blocker]).await.workspace;
    done(&manager, &api).await;
    manager.start_waiting_preparations().await.unwrap();
    assert!(manager.operation_runs_in(&ui.id));
    // A stop that read the wait before the release cancels nothing.
    let cancelled = manager
        .cancel_launches_while_preparation_waits(&ui.id)
        .await
        .unwrap();
    assert!(cancelled.is_none());
    assert_eq!(manager.queue_entries().await.unwrap().len(), 1);
}

#[tokio::test]
async fn a_restart_keeps_a_waiting_preparation_waiting() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    let api = create(&manager, &repo, "api").await;
    let blocker = Blocker::Space(api.id.to_string());
    let ui = *start(&manager, &repo, "ui", vec![blocker]).await.workspace;
    manager
        .store
        .quarantine_interrupted_operations()
        .await
        .unwrap();
    let after = read(&manager, &ui).await;
    assert_eq!(after.state, WorkspaceState::Preparing);
    assert_eq!(after.error, None);
    assert!(after.preparation_waits.is_some());
}

#[tokio::test]
async fn stopping_everything_stops_a_waiting_workspace() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    let api = create(&manager, &repo, "api").await;
    let blocker = Blocker::Space(api.id.to_string());
    let ui = *start(&manager, &repo, "ui", vec![blocker]).await.workspace;
    let targets = manager.stop_targets(QueuedLaunches::Cancel).await.unwrap();
    assert!(targets.iter().any(|space| space.id == ui.id), "{targets:?}");
}

#[tokio::test]
async fn a_pass_that_read_the_wait_before_a_stop_keeps_the_stop() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    let api = create(&manager, &repo, "api").await;
    let blocker = Blocker::Space(api.id.to_string());
    let ui = *start(&manager, &repo, "ui", vec![blocker]).await.workspace;
    let read_before = read(&manager, &ui).await.part.preparation_waits.unwrap();
    manager
        .cancel_launches_while_preparation_waits(&ui.id)
        .await
        .unwrap();
    let pass = (&read_before, &PreparationWait::Disk);
    manager.note_preparation_wait(&ui.id, pass).await.unwrap();
    let waits = read(&manager, &ui).await.part.preparation_waits;
    assert_eq!(waits, Some(PreparationWait::Stopped));
}

#[tokio::test]
async fn a_released_preparation_never_stacks_its_blocker_on_itself() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    let api = create(&manager, &repo, "api").await;
    std::fs::write(api.path.join("api"), "api\n").unwrap();
    commit(&api.path, "api");
    let blocker = Blocker::Space(api.id.to_string());
    let ui = *start(&manager, &repo, "ui", vec![blocker]).await.workspace;
    // Meanwhile the blocker came to build on the waiting workspace.
    let ui_selector = SpaceSelector::id(&ui.id).to_string();
    let api_selector = SpaceSelector::id(&api.id);
    (manager.set_base_workspace(&api_selector, Some(ui_selector)))
        .await
        .unwrap();
    done(&manager, &api).await;
    manager.start_waiting_preparations().await.unwrap();
    assert!(manager.operation_runs_in(&ui.id));
    // It holds the blocker's work without stacking on it.
    assert_eq!(read(&manager, &ui).await.part.base_workspace, None);
    let selector = SpaceSelector::id(&ui.id);
    let created = manager.create_pending_worktree(&selector).await.unwrap();
    assert!(created.path.join("api").exists());
}

#[tokio::test]
async fn a_cancelled_queue_entry_leaves_its_preparation_to_a_restore() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    let api = create(&manager, &repo, "api").await;
    let blocker = Blocker::Space(api.id.to_string());
    let started = start(&manager, &repo, "ui", vec![blocker]).await;
    let entry = started.entry.as_ref().unwrap().id.clone();
    manager.queue_remove(entry.as_str(), None).await.unwrap();
    let waits = read(&manager, &started.workspace)
        .await
        .part
        .preparation_waits;
    assert_eq!(waits, Some(PreparationWait::Stopped));
}

#[tokio::test]
async fn a_stopped_start_without_an_agent_stays_stopped_once_disk_allows() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    let low = "[machine.overload.warning]\ndisk_free_gib = 1000000000\n";
    manager.publish_config(global_config(low));
    let opening = Opening {
        start: WorkspaceStart::Name("ui".into()),
        base: None,
        new: Default::default(),
        parent: None,
    };
    let preparing = Preparing {
        issue: None,
        pr: None,
        agent: None,
        waiting: None,
        queue_entry: None,
        blockers: Vec::new(),
        unfollowed: true,
        env: None,
    };
    let started = manager
        .start_workspace((&repo.id).into(), opening, preparing, None)
        .await
        .unwrap();
    let ui = *started.workspace;
    let targets = manager.stop_targets(QueuedLaunches::Cancel).await.unwrap();
    assert!(targets.iter().any(|space| space.id == ui.id), "{targets:?}");
    let selector = SpaceSelector::id(&ui.id);
    (manager.stop_space_manually(&selector, false, QueuedLaunches::Cancel))
        .await
        .unwrap();
    manager.publish_config(global_config(""));
    manager.start_waiting_preparations().await.unwrap();
    assert!(!manager.operation_runs_in(&ui.id));
    let waits = read(&manager, &ui).await.part.preparation_waits;
    assert_eq!(waits, Some(PreparationWait::Stopped));
}

/// Start a copy of `source`, which `selector` names, that waits for `api`,
/// with `git_profile`.
async fn start_copy(
    manager: &Arc<Manager>,
    repo: &Repository,
    (selector, api): (SpaceSelector, &Workspace),
    git_profile: Option<&str>,
) -> Workspace {
    let opening = Opening {
        start: WorkspaceStart::Copy {
            source: crate::protocol::CopySource {
                workspace: selector,
                checkpoint: None,
            },
            branch: Some("copy".into()),
            typed: false,
        },
        base: None,
        new: crate::daemon::workspace::NewWorkspace {
            git_profile,
            ..Default::default()
        },
        parent: None,
    };
    let preparing = Preparing {
        issue: None,
        pr: None,
        agent: Some(Box::new(LaunchRequest::new(Some(
            "claude".parse().unwrap(),
        )))),
        waiting: None,
        queue_entry: None,
        blockers: vec![Blocker::Space(api.id.to_string())],
        unfollowed: false,
        env: None,
    };
    let started = manager
        .start_workspace((&repo.id).into(), opening, preparing, None)
        .await
        .unwrap();
    assert!(started.operation.is_none());
    *started.workspace
}

#[tokio::test]
async fn a_blocked_copy_takes_its_source_as_it_is_once_its_blockers_clear() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    let api = create(&manager, &repo, "api").await;
    let source = create(&manager, &repo, "source").await;
    // Named by its number, which the wait may free and give another.
    let selector = SpaceSelector::parse(&source.number.to_string()).unwrap();
    let copy = start_copy(&manager, &repo, (selector, &api), None).await;
    assert!(copy.pending_worktree && !copy.path.exists());
    let id = copy.id.clone();
    let plan: String = (manager.store)
        .run(move |db| {
            let sql = "SELECT plan FROM pending_worktrees WHERE workspace_id=?1";
            Ok(db.query_row(sql, [&id], |row| row.get(0))?)
        })
        .await
        .unwrap();
    assert!(plan.contains(source.id.as_str()), "{plan}");
    // The source's work goes on while the copy waits.
    std::fs::write(source.path.join("later"), "later\n").unwrap();
    done(&manager, &api).await;
    manager.start_waiting_preparations().await.unwrap();
    assert!(manager.operation_runs_in(&copy.id));
    let selector = SpaceSelector::id(&copy.id);
    let created = manager.create_pending_worktree(&selector).await.unwrap();
    assert!(created.path.join("later").exists());
    assert!(!created.pending_worktree);
    assert!(created.copied_from.is_some());
}

#[tokio::test]
async fn a_start_waiting_for_no_such_space_records_nothing() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    let blocker = Blocker::Space(shoal_ids::space_ids::SpaceId::generate().to_string());
    let opening = Opening {
        start: WorkspaceStart::Name("ui".into()),
        base: None,
        new: Default::default(),
        parent: None,
    };
    let preparing = Preparing {
        issue: None,
        pr: None,
        agent: Some(Box::new(LaunchRequest::new(Some(
            "claude".parse().unwrap(),
        )))),
        waiting: None,
        queue_entry: None,
        blockers: vec![blocker],
        unfollowed: true,
        env: None,
    };
    let refused = manager
        .start_workspace((&repo.id).into(), opening, preparing, None)
        .await;
    assert!(refused.is_err(), "{refused:?}");
    assert!(manager.list_workspaces().await.unwrap().is_empty());
    assert!(manager.queue_entries().await.unwrap().is_empty());
}
