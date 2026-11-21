use std::{sync::Arc, time::Duration};

use super::{Child, Opening, Preparing};
use crate::{
    config::permissions::PermissionsConfig,
    daemon::{
        manager::Manager,
        notifications::AlertKind,
        operations::fake,
        scope::Caller,
        test_support::{git, global_config, manager, repository, repository_with},
    },
    forge::numbers::PullNumber,
    model::{Repository, Workspace},
    protocol::{
        CopySource, CreationOptions, PullHead,
        operations::{
            Ended, Failure, LaunchRequest, OperationSpec, Report, StartedWorkspace, WorkspaceStart,
        },
    },
    removal::{InspectionPolicy, UnsavedWork},
    state::WorkspaceState,
};
use shoal_ids::space_ids::SpaceNumber;
use shoal_protocol::{
    alerts::Alert,
    space_names::{SpaceName, SpaceSelector},
};

/// A generous guard against a hang; no test waits this long when it passes.
const HANG: Duration = Duration::from_secs(60);

async fn registered(root: &std::path::Path, manager: &Manager) -> Repository {
    let path = repository(root, "repo");
    manager
        .repositories
        .register(path.to_str().unwrap().into(), None, None)
        .await
        .unwrap()
}

/// Start a new workspace on branch `name` from `base`, without an agent.
async fn start(
    manager: &Arc<Manager>,
    repository: &Repository,
    name: &str,
    base: &str,
) -> StartedWorkspace {
    let opening = Opening {
        start: WorkspaceStart::Name(name.into()),
        base: Some(base.into()),
        new: Default::default(),
        parent: None,
    };
    start_opening(manager, repository, opening).await
}

/// Start the workspace `opening` names, without an agent.
async fn start_opening(
    manager: &Arc<Manager>,
    repository: &Repository,
    opening: Opening<'_>,
) -> StartedWorkspace {
    manager
        .start_workspace((&repository.id).into(), opening, without_agent(), None)
        .await
        .unwrap()
}

/// A preparation that queues no agent.
fn without_agent() -> Preparing<LaunchRequest> {
    Preparing {
        issue: None,
        pr: None,
        agent: None,
        waiting: None,
        queue_entry: None,
        blockers: Vec::new(),
        unfollowed: false,
        env: None,
    }
}

async fn read(manager: &Manager, workspace: &Workspace) -> Workspace {
    manager.workspace_by_id(&workspace.id).await.unwrap()
}

#[tokio::test]
async fn a_start_records_the_workspace_and_leaves_its_worktree_to_its_preparation() {
    let (root, manager) = manager().await;
    let repository = registered(root.path(), &manager).await;
    let started = start(&manager, &repository, "child", "HEAD").await;
    let workspace = *started.workspace;
    assert!(workspace.checkout.is_pending());
    assert_eq!(workspace.state, WorkspaceState::Preparing);
    assert!(!workspace.path.exists());
    let worker = fake::take(started.operation.as_deref().unwrap());
    let OperationSpec::Prepare { steps, .. } = &worker.order.spec else {
        panic!("expected a preparation: {:?}", worker.order);
    };
    assert!(steps.setup);
    // The preparation creates it first.
    let selector = SpaceSelector::id(&workspace.id);
    let created = manager.create_pending_worktree(&selector).await.unwrap();
    assert!(!created.checkout.is_pending());
    assert!(created.path.join("tracked").exists());
    assert_eq!(
        git(&created.path, &["branch", "--show-current"]).trim(),
        "child"
    );
}

#[tokio::test]
async fn a_copy_start_leaves_its_worktree_to_its_preparation() {
    let (root, manager) = manager().await;
    let repository = registered(root.path(), &manager).await;
    let source = (manager.create_workspace(&repository.id, "topic".into(), None, None, None))
        .await
        .unwrap();
    std::fs::write(source.path.join("untracked"), "untracked\n").unwrap();
    let opening = Opening {
        start: WorkspaceStart::Copy {
            source: CopySource {
                workspace: SpaceSelector::id(&source.id),
                checkpoint: None,
            },
            branch: None,
            typed: false,
        },
        base: None,
        new: Default::default(),
        parent: None,
    };
    let started = start_opening(&manager, &repository, opening).await;
    let workspace = *started.workspace;
    assert!(workspace.checkout.is_pending());
    assert!(!workspace.path.exists());
    let _worker = fake::take(started.operation.as_deref().unwrap());
    let selector = SpaceSelector::id(&workspace.id);
    let created = manager.create_pending_worktree(&selector).await.unwrap();
    assert!(created.path.join("untracked").exists());
    assert_eq!(
        created.part.copied_from.unwrap().workspace.unwrap().id,
        source.id
    );
}

#[tokio::test]
async fn a_start_at_a_prs_head_leaves_its_worktree_to_its_preparation() {
    let (root, manager) = manager().await;
    let origin = repository(root.path(), "origin");
    let clone = root.path().join("clone");
    git(
        root.path(),
        &["clone", "-q", origin.to_str().unwrap(), "clone"],
    );
    let head = git(&origin, &["rev-parse", "HEAD"]).trim().to_owned();
    git(&origin, &["update-ref", "refs/pull/9/head", &head]);
    let repository = (manager
        .repositories
        .register(clone.to_str().unwrap().into(), None, None))
    .await
    .unwrap();
    let opening = Opening {
        start: WorkspaceStart::PullHead {
            url: "https://forge.example/team/repo/pulls/9".parse().unwrap(),
            head: PullHead {
                number: PullNumber::new(9),
                reference: "refs/pull/9/head".into(),
                commit: head.clone().into(),
                base: None,
            },
        },
        base: None,
        new: crate::daemon::workspace::NewWorkspace {
            // Named, so the start reads no PR title from the forge.
            options: CreationOptions {
                name: Some(SpaceName::derive("pr", "workspace")),
                ..Default::default()
            },
            ..Default::default()
        },
        parent: None,
    };
    let started = start_opening(&manager, &repository, opening).await;
    let workspace = *started.workspace;
    assert!(workspace.checkout.is_pending());
    assert!(!workspace.path.exists());
    let _worker = fake::take(started.operation.as_deref().unwrap());
    let selector = SpaceSelector::id(&workspace.id);
    let created = manager.create_pending_worktree(&selector).await.unwrap();
    assert!(!created.checkout.is_pending());
    assert_eq!(git(&created.path, &["rev-parse", "HEAD"]).trim(), head);
    let branch = git(&created.path, &["branch", "--show-current"]);
    assert_eq!(branch.trim(), "");
}

#[tokio::test]
async fn a_failed_worktree_stays_pending_for_a_retried_preparation() {
    let (root, manager) = manager().await;
    let repository = registered(root.path(), &manager).await;
    let started = start(&manager, &repository, "child", "later").await;
    let _worker = fake::take(started.operation.as_deref().unwrap());
    let selector = SpaceSelector::id(&started.workspace.id);
    let failed = manager.create_pending_worktree(&selector).await;
    assert!(failed.is_err(), "{failed:?}");
    let workspace = read(&manager, &started.workspace).await;
    assert_eq!(workspace.state, WorkspaceState::Failed);
    assert!(workspace.checkout.is_pending());
    // Once its base exists, a retry creates it.
    git(&repository.path, &["branch", "later"]);
    let created = manager.create_pending_worktree(&selector).await.unwrap();
    assert!(!created.checkout.is_pending());
    assert!(created.path.exists());
    let again = manager.create_pending_worktree(&selector).await;
    assert!(again.is_err(), "a second creation ran: {again:?}");
}

#[tokio::test]
async fn a_workspace_without_its_worktree_yet_goes_with_its_record() {
    let (root, manager) = manager().await;
    let repository = registered(root.path(), &manager).await;
    let started = start(&manager, &repository, "child", "HEAD").await;
    let _worker = fake::take(started.operation.as_deref().unwrap());
    let selector = SpaceSelector::id(&started.workspace.id);
    manager
        .remove_selected(&selector, UnsavedWork::All, InspectionPolicy::GitOnly)
        .await
        .unwrap();
    assert!(manager.workspace(&selector).await.is_err());
    // Its branch was never created, so no removal kept it.
    let branches = git(&repository.path, &["branch", "--list", "child"]);
    assert_eq!(branches.trim(), "");
}

#[tokio::test]
async fn a_workspace_whose_worktree_failed_goes_with_its_record() {
    let (root, manager) = manager().await;
    let repository = registered(root.path(), &manager).await;
    let started = start(&manager, &repository, "child", "later").await;
    let _worker = fake::take(started.operation.as_deref().unwrap());
    let selector = SpaceSelector::id(&started.workspace.id);
    let failed = manager.create_pending_worktree(&selector).await;
    assert!(failed.is_err(), "{failed:?}");
    manager
        .remove_selected(&selector, UnsavedWork::All, InspectionPolicy::GitOnly)
        .await
        .unwrap();
    assert!(manager.workspace(&selector).await.is_err());
}

/// The notification a start's preparation ending with `failure` records,
/// and the started workspace's number.
async fn reported(failure: Failure) -> (Alert, SpaceNumber) {
    let (root, manager) = manager().await;
    let repository = registered(root.path(), &manager).await;
    let started = start(&manager, &repository, "child", "HEAD").await;
    reported_for(&manager, started, failure).await
}

/// The notification the preparation `started` ending with `failure`
/// records, and the started workspace's number.
async fn reported_for(
    manager: &Manager,
    started: StartedWorkspace,
    failure: Failure,
) -> (Alert, SpaceNumber) {
    let worker = fake::take(started.operation.as_deref().unwrap());
    let failed = Report::Ended(Ended::Failed(failure));
    (manager.report_operation(started.operation.as_deref().unwrap(), failed, None))
        .await
        .unwrap();
    worker.exit(0);
    let notification = tokio::time::timeout(HANG, async {
        loop {
            let notifications = manager.notifications.list(false, 10).await.unwrap();
            let found = (notifications.into_iter())
                .find(|notification| notification.kind == AlertKind::PreparationFailed);
            match found {
                Some(found) => return found,
                None => tokio::task::yield_now().await,
            }
        }
    })
    .await
    .expect("the failure was reported");
    assert_eq!(notification.space_id.as_ref(), Some(&started.workspace.id));
    (notification, started.workspace.number)
}

#[tokio::test]
async fn a_failed_preparation_reaches_the_user_with_what_retries_it() {
    let message = "fatal: no space left".to_owned();
    let (notification, number) = reported(Failure::WorktreeFailed { message }).await;
    let message = &notification.message;
    assert!(message.contains("fatal: no space left"), "{message}");
    assert!(
        message.contains(&format!("`shoal setup {number}`")),
        "{message}"
    );
}

#[tokio::test]
async fn a_split_reports_its_failed_preparation_too() {
    let (root, manager) = manager().await;
    let repository = registered(root.path(), &manager).await;
    let parent = manager
        .create_workspace(
            &repository.id,
            "parent".into(),
            Some("HEAD".into()),
            None,
            None,
        )
        .await
        .unwrap();
    let split = Child {
        name: Some("child".into()),
        copy: None,
        base: None,
        options: Default::default(),
    };
    let selector = SpaceSelector::id(&parent.id);
    let started = (manager.start_child(&selector, split, without_agent(), None))
        .await
        .unwrap();
    let message = "setup exited with status 1".to_owned();
    let (notification, number) =
        reported_for(&manager, started, Failure::SetupFailed { message }).await;
    let message = &notification.message;
    assert!(
        message.contains(&format!("`shoal setup {number}`")),
        "{message}"
    );
}

#[tokio::test]
async fn a_chat_splits_a_child_workspace_below_itself() {
    let (root, manager) = manager().await;
    let repository = registered(root.path(), &manager).await;
    let chat = (manager.start_chat(
        repository.id.clone().into(),
        "talk".into(),
        "claude".into(),
        None,
    ))
    .await
    .unwrap();
    let split = Child {
        name: Some("child".into()),
        copy: None,
        base: Some("HEAD".into()),
        options: Default::default(),
    };
    let selector = SpaceSelector::id(&chat.id);
    let started = (manager.start_child(&selector, split, without_agent(), None))
        .await
        .unwrap();
    let _worker = fake::take(started.operation.as_deref().unwrap());
    assert_eq!(started.workspace.repository(), &repository.id);
    let parent = started.workspace.parent.as_ref().map(|parent| &parent.id);
    assert_eq!(parent, Some(&chat.id));
    assert!(started.entry.is_none(), "a split queues no agent");
}

#[tokio::test]
async fn a_chat_without_a_repository_splits_a_copy_in_the_copied_repository() {
    let (root, manager) = manager().await;
    let repository = registered(root.path(), &manager).await;
    let copied = (manager.create_workspace(&repository.id, "copied".into(), None, None, None))
        .await
        .unwrap();
    let new = crate::daemon::chats::NewChat {
        priority: None,
        name: "master".into(),
        agent: "claude".into(),
        mode: "chat".into(),
        narrowing: Default::default(),
        origin: None,
        parent: None,
    };
    let chat = manager.start_new_chat(None, None, new).await.unwrap();
    let selector = SpaceSelector::id(&chat.id);
    let split = |copy| Child {
        name: Some("child".into()),
        copy,
        base: None,
        options: Default::default(),
    };
    // A branch split names no repository for the child.
    let refused = manager.start_child(&selector, split(None), without_agent(), None);
    assert!(refused.await.is_err());
    let copy = CopySource {
        workspace: SpaceSelector::id(&copied.id),
        checkpoint: None,
    };
    // Its agent's mode resolves in the copied repository, which defines it.
    let config = "[modes.local]\nkind = 'workspace'\n";
    (manager.write_local_config(&repository.id, Some(config.into())))
        .await
        .unwrap();
    let preparing = Preparing {
        agent: Some(Box::new(LaunchRequest {
            mode: Some("local".into()),
            ..LaunchRequest::new(Some("claude".parse().unwrap()))
        })),
        ..without_agent()
    };
    let started = (manager.start_child(&selector, split(Some(copy)), preparing, None))
        .await
        .unwrap();
    let _worker = fake::take(started.operation.as_deref().unwrap());
    assert_eq!(started.workspace.repository(), &repository.id);
    let parent = started.workspace.parent.as_ref().map(|parent| &parent.id);
    assert_eq!(parent, Some(&chat.id));
}

#[tokio::test]
async fn a_failed_post_setup_hook_is_retried_by_setup() {
    let message = "hook exited with status 3".to_owned();
    let (notification, number) = reported(Failure::HookFailed { message }).await;
    let message = &notification.message;
    assert!(
        message.contains(&format!("`shoal setup {number}`")),
        "{message}"
    );
}

#[tokio::test]
async fn a_retry_configures_the_worktree_an_earlier_creation_left() {
    let (root, manager) = manager().await;
    let path = repository_with(
        root.path(),
        "repo",
        &[(".shoal.toml", "git_profile = 'work'\n")],
    );
    let repository = manager
        .repositories
        .register(path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let started = start(&manager, &repository, "child", "HEAD").await;
    let _worker = fake::take(started.operation.as_deref().unwrap());
    let selector = SpaceSelector::id(&started.workspace.id);
    // Git creates the worktree, but the profile it names is not defined.
    let failed = manager.create_pending_worktree(&selector).await;
    assert!(failed.is_err(), "{failed:?}");
    let workspace = read(&manager, &started.workspace).await;
    assert!(workspace.checkout.is_pending() && workspace.path.exists());
    manager.publish_config(global_config(
        "[machine.git_profiles.work]\nuser.email = 'work@example.invalid'\n",
    ));
    let created = manager.create_pending_worktree(&selector).await.unwrap();
    assert!(!created.checkout.is_pending());
    let email = git(&created.path, &["config", "user.email"]);
    assert_eq!(email.trim(), "work@example.invalid");
}

/// Record `assignment` as the `--permission` given to the worker of
/// `workspace`.
async fn give(manager: &Manager, workspace: &Workspace, assignment: &str) {
    let given = PermissionsConfig::assigned(&[assignment.to_owned()]).unwrap();
    let given = serde_json::to_string(&given).unwrap();
    let id = workspace.id.clone();
    (manager.store)
        .run(move |db| {
            let sql = "UPDATE spaces SET mode='worker',narrowing=?2 WHERE id=?1";
            Ok(db.execute(sql, rusqlite::params![id, given])?)
        })
        .await
        .unwrap();
}

/// The scoped caller of `workspace`'s agent.
fn scoped(workspace: &Workspace) -> Caller {
    Caller {
        space_id: workspace.id.clone(),
        execution: None,
        granted: None,
        read_only: false,
        vouched: None,
    }
}

#[tokio::test]
async fn a_scoped_start_gives_its_agent_only_permissions_the_caller_holds() {
    let (root, manager) = manager().await;
    let repository = registered(root.path(), &manager).await;
    let parent = manager
        .create_workspace(
            &repository.id,
            "parent".into(),
            Some("HEAD".into()),
            None,
            None,
        )
        .await
        .unwrap();
    give(&manager, &parent, "push=deny").await;
    let caller = scoped(&parent);
    let opening = Opening {
        start: WorkspaceStart::Name("child".into()),
        base: Some("HEAD".into()),
        new: Default::default(),
        parent: Some(parent.id.clone()),
    };
    let agent = LaunchRequest {
        narrowing: PermissionsConfig::assigned(&["push=direct".to_owned()]).unwrap(),
        ..LaunchRequest::new(Some("claude".parse().unwrap()))
    };
    let preparing = Preparing {
        issue: None,
        pr: None,
        agent: Some(Box::new(agent)),
        waiting: None,
        queue_entry: None,
        blockers: Vec::new(),
        unfollowed: false,
        env: None,
    };
    let refused = manager
        .start_workspace((&repository.id).into(), opening, preparing, Some(&caller))
        .await;
    assert!(refused.is_err(), "{refused:?}");
    // Refused before anything was created.
    let workspaces = manager.list_workspaces().await.unwrap();
    assert_eq!(workspaces.len(), 1, "{workspaces:?}");
}

#[tokio::test]
async fn a_scoped_start_reopening_a_workspace_keeps_what_its_agent_was_given() {
    let (root, manager) = manager().await;
    let repository = registered(root.path(), &manager).await;
    let create = async |name: &str, parent: Option<&Workspace>| {
        let workspace = manager
            .create_workspace(&repository.id, name.into(), Some("HEAD".into()), None, None)
            .await
            .unwrap();
        let parent = parent.map(|parent| parent.id.clone());
        manager.assign_parent(&workspace.id, parent).await.unwrap();
        workspace
    };
    // The caller holds `propose`; the user gave the space below it
    // `direct`, and the workspace below that `propose`.
    let top = create("top", None).await;
    give(&manager, &top, "push=propose").await;
    let middle = create("middle", Some(&top)).await;
    give(&manager, &middle, "push=direct").await;
    let reopened = create("reopened", Some(&middle)).await;
    give(&manager, &reopened, "push=propose").await;
    let opening = Opening {
        start: WorkspaceStart::Branch("reopened".into()),
        base: None,
        new: Default::default(),
        parent: None,
    };
    // Leaving `push` out would give it the space above's `direct`.
    let agent = LaunchRequest {
        mode: Some("worker".into()),
        ..LaunchRequest::new(Some("claude".parse().unwrap()))
    };
    let preparing = Preparing {
        issue: None,
        pr: None,
        agent: Some(Box::new(agent)),
        waiting: None,
        queue_entry: None,
        blockers: Vec::new(),
        unfollowed: false,
        env: None,
    };
    let refused = manager
        .start_workspace(
            (&repository.id).into(),
            opening,
            preparing,
            Some(&scoped(&top)),
        )
        .await;
    assert!(refused.is_err(), "{refused:?}");
    assert!(manager.queue_entries().await.unwrap().is_empty());
}

#[tokio::test]
async fn a_retried_preparation_checks_out_the_branch_a_cut_short_one_created() {
    let (root, manager) = manager().await;
    let repository = registered(root.path(), &manager).await;
    let started = start(&manager, &repository, "child", "HEAD").await;
    let _worker = fake::take(started.operation.as_deref().unwrap());
    // As a preparation a restart cut short after Git created the branch
    // leaves it.
    git(&repository.path, &["branch", "child"]);
    let selector = SpaceSelector::id(&started.workspace.id);
    let created = manager.create_pending_worktree(&selector).await.unwrap();
    let branch = git(&created.path, &["branch", "--show-current"]);
    assert_eq!(branch.trim(), "child");
}
