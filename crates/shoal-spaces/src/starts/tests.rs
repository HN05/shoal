use crate::scope::Caller;
use crate::stand_in::Fixture;
use crate::starts::{Child, Opening, Preparing};
use crate::{
    config::permissions::PermissionsConfig,
    model::{Repository, Workspace},
    protocol::{
        CopySource,
        operations::{LaunchRequest, OperationSpec, StartedWorkspace, WorkspaceStart},
    },
    removal::{InspectionPolicy, UnsavedWork},
    state::WorkspaceState,
};
use shoal_core::test_support::git;
use shoal_daemon_base::callers::Access;
use shoal_daemon_base::operations::fake;
use shoal_protocol::space_names::SpaceSelector;

/// Start a new workspace on branch `name` from `base`, without an agent.
async fn start(
    manager: &Fixture,
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
    manager: &Fixture,
    repository: &Repository,
    opening: Opening<'_>,
) -> StartedWorkspace {
    manager
        .spaces
        .start_workspace(
            &manager.daemon(),
            (&repository.id).into(),
            opening,
            without_agent(),
            None,
        )
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

async fn read(manager: &Fixture, workspace: &Workspace) -> Workspace {
    manager.spaces.workspace_by_id(&workspace.id).await.unwrap()
}

/// Record `assignment` as the `--permission` given to the worker of
/// `workspace`.
async fn give(manager: &Fixture, workspace: &Workspace, assignment: &str) {
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
        access: Access::Full,
        vouched: None,
    }
}

#[tokio::test]
async fn a_start_records_the_workspace_and_leaves_its_worktree_to_its_preparation() {
    let manager = Fixture::new().await;
    let repository = manager.repository("repo").await;
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
    let created = manager
        .spaces
        .create_pending_worktree(&*manager, &selector)
        .await
        .unwrap();
    assert!(!created.checkout.is_pending());
    assert!(created.path.join("tracked").exists());
    assert_eq!(
        git(&created.path, &["branch", "--show-current"]).trim(),
        "child"
    );
}

#[tokio::test]
async fn a_copy_start_leaves_its_worktree_to_its_preparation() {
    let manager = Fixture::new().await;
    let repository = manager.repository("repo").await;
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
    let created = manager
        .spaces
        .create_pending_worktree(&*manager, &selector)
        .await
        .unwrap();
    assert!(created.path.join("untracked").exists());
    assert_eq!(
        created.part.copied_from.unwrap().workspace.unwrap().id,
        source.id
    );
}

#[tokio::test]
async fn a_failed_worktree_stays_pending_for_a_retried_preparation() {
    let manager = Fixture::new().await;
    let repository = manager.repository("repo").await;
    let started = start(&manager, &repository, "child", "later").await;
    let _worker = fake::take(started.operation.as_deref().unwrap());
    let selector = SpaceSelector::id(&started.workspace.id);
    let failed = manager
        .spaces
        .create_pending_worktree(&*manager, &selector)
        .await;
    assert!(failed.is_err(), "{failed:?}");
    let workspace = read(&manager, &started.workspace).await;
    assert_eq!(workspace.state, WorkspaceState::Failed);
    assert!(workspace.checkout.is_pending());
    // Once its base exists, a retry creates it.
    git(&repository.path, &["branch", "later"]);
    let created = manager
        .spaces
        .create_pending_worktree(&*manager, &selector)
        .await
        .unwrap();
    assert!(!created.checkout.is_pending());
    assert!(created.path.exists());
    let again = manager
        .spaces
        .create_pending_worktree(&*manager, &selector)
        .await;
    assert!(again.is_err(), "a second creation ran: {again:?}");
}

#[tokio::test]
async fn a_workspace_without_its_worktree_yet_goes_with_its_record() {
    let manager = Fixture::new().await;
    let repository = manager.repository("repo").await;
    let started = start(&manager, &repository, "child", "HEAD").await;
    let _worker = fake::take(started.operation.as_deref().unwrap());
    let selector = SpaceSelector::id(&started.workspace.id);
    manager
        .spaces
        .remove_selected(
            &*manager,
            &selector,
            UnsavedWork::All,
            InspectionPolicy::GitOnly,
        )
        .await
        .unwrap();
    assert!(manager.spaces.workspace(&selector).await.is_err());
    // Its branch was never created, so no removal kept it.
    let branches = git(&repository.path, &["branch", "--list", "child"]);
    assert_eq!(branches.trim(), "");
}

#[tokio::test]
async fn a_workspace_whose_worktree_failed_goes_with_its_record() {
    let manager = Fixture::new().await;
    let repository = manager.repository("repo").await;
    let started = start(&manager, &repository, "child", "later").await;
    let _worker = fake::take(started.operation.as_deref().unwrap());
    let selector = SpaceSelector::id(&started.workspace.id);
    let failed = manager
        .spaces
        .create_pending_worktree(&*manager, &selector)
        .await;
    assert!(failed.is_err(), "{failed:?}");
    manager
        .spaces
        .remove_selected(
            &*manager,
            &selector,
            UnsavedWork::All,
            InspectionPolicy::GitOnly,
        )
        .await
        .unwrap();
    assert!(manager.spaces.workspace(&selector).await.is_err());
}

#[tokio::test]
async fn a_chat_splits_a_child_workspace_below_itself() {
    let manager = Fixture::new().await;
    let repository = manager.repository("repo").await;
    let chat = manager.chat("talk", "chat", Some(&repository), None).await;
    let split = Child {
        name: Some("child".into()),
        copy: None,
        base: Some("HEAD".into()),
        options: Default::default(),
    };
    let selector = SpaceSelector::id(&chat.id);
    let started =
        (manager
            .spaces
            .start_child(&manager.daemon(), &selector, split, without_agent(), None))
        .await
        .unwrap();
    let _worker = fake::take(started.operation.as_deref().unwrap());
    assert_eq!(started.workspace.repository(), &repository.id);
    let parent = started.workspace.parent.as_ref().map(|parent| &parent.id);
    assert_eq!(parent, Some(&chat.id));
    assert!(started.entry.is_none(), "a split queues no agent");
}

#[tokio::test]
async fn a_scoped_start_gives_its_agent_only_permissions_the_caller_holds() {
    let manager = Fixture::new().await;
    let repository = manager.repository("repo").await;
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
        .spaces
        .start_workspace(
            &manager.daemon(),
            (&repository.id).into(),
            opening,
            preparing,
            Some(&caller),
        )
        .await;
    assert!(refused.is_err(), "{refused:?}");
    // Refused before anything was created.
    let workspaces = manager.spaces.list_workspaces().await.unwrap();
    assert_eq!(workspaces.len(), 1, "{workspaces:?}");
}

#[tokio::test]
async fn a_retried_preparation_checks_out_the_branch_a_cut_short_one_created() {
    let manager = Fixture::new().await;
    let repository = manager.repository("repo").await;
    let started = start(&manager, &repository, "child", "HEAD").await;
    let _worker = fake::take(started.operation.as_deref().unwrap());
    // As a preparation a restart cut short after Git created the branch
    // leaves it.
    git(&repository.path, &["branch", "child"]);
    let selector = SpaceSelector::id(&started.workspace.id);
    let created = manager
        .spaces
        .create_pending_worktree(&*manager, &selector)
        .await
        .unwrap();
    let branch = git(&created.path, &["branch", "--show-current"]);
    assert_eq!(branch.trim(), "child");
}
