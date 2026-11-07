//! An app's calls through `shoal_client`, as the user and as a
//! workspace's agent, against a daemon on an isolated state directory.
use crate::client::space_names::SpaceSelector;
use crate::daemon::approvals::ApprovalRequestExt;
use crate::{
    client::{
        approvals::ApprovalRequest,
        connection::{self, Connector},
        operations::{Followed, Following},
        questions::Question,
        removal::{RemovalCheck, RemovalResult, UnsavedWork},
    },
    config::repo::ConflictPolicy,
    daemon::{
        approvals::{Lifetime, PortSpecification, Specification, Target},
        operations::fake,
        questions::Ask,
        workspace_files::Attacher,
    },
    forge::items::link::LinkKind,
    model::{
        Completion, Inspection, QueueEntry, Relation, Repository, SyncedRepository, Workspace,
    },
    protocol::{
        ApprovalMethod, AssignmentMethod, Body, ErrorCode, HoldMethod, ItemMethod, OperationMethod,
        QuestionMethod, QueueMethod, RemoteError, RemovalMethod, RepositoryMethod, WorkspaceMethod,
        operations::{
            Ended, Failure, OperationSpec, QueuedLaunch, Report, StartedWorkspace, WorkspaceStart,
        },
    },
};

use super::{HANG, TestDaemon};

/// The code `result` failed with.
fn code<T: std::fmt::Debug>(result: anyhow::Result<T>) -> ErrorCode {
    let error = result.unwrap_err();
    match error.downcast_ref::<RemoteError>() {
        Some(remote) => remote.code.clone(),
        None => panic!("expected a refusal: {error:#}"),
    }
}

async fn inspect(daemon: &Connector, workspace: &str) -> anyhow::Result<Workspace> {
    let method = WorkspaceMethod::InspectWorkspace {
        workspace: workspace.into(),
    };
    Ok(connection::request::<Inspection>(daemon, method)
        .await?
        .workspace)
}

#[tokio::test]
async fn an_app_reads_repositories_and_workspaces_as_the_user() {
    let daemon = TestDaemon::start().await;
    let own = daemon.workspace("own").await;
    let app = daemon.connector();
    let listed: Vec<Repository> = connection::request(&app, RepositoryMethod::ListRepositories)
        .await
        .unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, daemon.repository.id);
    let sync = RepositoryMethod::SyncRepository {
        repository: "missing".into(),
    };
    assert_eq!(
        code(connection::request::<SyncedRepository>(&app, sync).await),
        ErrorCode::NotFound
    );
    assert_eq!(inspect(&app, &own.id).await.unwrap().name, "own");
    assert_eq!(code(inspect(&app, "missing").await), ErrorCode::NotFound);
}

#[tokio::test]
async fn a_scoped_app_reaches_only_its_workspace_tree() {
    let daemon = TestDaemon::start().await;
    let own = daemon.workspace("own").await;
    let other = daemon.workspace("other").await;
    let agent = daemon.scoped(&own).await;
    assert_eq!(inspect(&agent, "own").await.unwrap().id, own.id);
    assert_eq!(
        code(inspect(&agent, &other.id).await),
        ErrorCode::ScopeDenied
    );
    // A workspace's agent asks to hold its own workspace.
    let hold = HoldMethod::HoldAcquire {
        workspace: "own".into(),
        name: "review".into(),
        reason: Some("open in Quay".into()),
    };
    let Body::ApprovalRequest(request) = connection::call(&agent, hold).await.unwrap() else {
        panic!("expected an approval request");
    };
    assert_eq!(request.target, Target::Hold("review".into()));
    let unknown = daemon.connector().scope("unknown");
    assert_eq!(code(inspect(&unknown, "own").await), ErrorCode::ScopeDenied);
}

#[tokio::test]
async fn removal_refuses_unsaved_work_unless_the_app_confirms_it() {
    let daemon = TestDaemon::start().await;
    let workspace = daemon.workspace("dirty").await;
    std::fs::write(workspace.path.join("draft"), "unsaved\n").unwrap();
    let app = daemon.connector();
    let remove = |unsaved| RemovalMethod::RemoveWorkspace {
        workspace: SpaceSelector::id(&workspace.id),
        unsaved,
    };
    assert_eq!(
        code(connection::request::<RemovalResult>(&app, remove(UnsavedWork::default())).await),
        ErrorCode::UnsavedWork
    );
    let check = RemovalMethod::CheckRemoval {
        workspace: "dirty".into(),
        include_changes: false,
    };
    let check: RemovalCheck = connection::request(&app, check).await.unwrap();
    let confirmed = remove(UnsavedWork::confirming(&check));
    let removal: RemovalResult = connection::request(&app, confirmed).await.unwrap();
    assert_eq!(removal.branch.as_deref(), Some("dirty"));
}

#[tokio::test]
async fn done_renames_and_links_go_through_the_protocol() {
    let daemon = TestDaemon::start().await;
    let workspace = daemon.workspace("first").await;
    daemon.workspace("second").await;
    let app = daemon.connector();
    let done = AssignmentMethod::MarkDone {
        space: SpaceSelector::id(&workspace.id),
        done: Default::default(),
        sender_chat: None,
    };
    let completion: Completion = connection::request(&app, done).await.unwrap();
    assert!(!completion.head.is_empty());
    assert!(inspect(&app, "first").await.unwrap().done);
    let rename = |branch: &str| WorkspaceMethod::RenameWorkspace {
        workspace: "first".into(),
        branch: branch.into(),
    };
    assert_eq!(
        code(connection::request::<Workspace>(&app, rename("second")).await),
        ErrorCode::NameTaken
    );
    let link = ItemMethod::LinkItem {
        space: SpaceSelector::id(&workspace.id),
        kind: LinkKind::Issue,
        url: "40".into(),
        relation: Relation::Related,
        title: None,
    };
    connection::request::<()>(&app, link).await.unwrap();
    let unlink = || ItemMethod::UnlinkItems {
        space: SpaceSelector::id(&workspace.id),
        relation: Relation::Related,
        kind: Some(LinkKind::Issue),
        url: Some("40".into()),
    };
    connection::request::<()>(&app, unlink()).await.unwrap();
    assert_eq!(
        code(connection::request::<()>(&app, unlink()).await),
        ErrorCode::NotFound
    );
}

#[tokio::test]
async fn answers_and_decisions_bind_to_the_record_the_app_showed() {
    let daemon = TestDaemon::start().await;
    let workspace = daemon.workspace("asker").await;
    let ask = Ask {
        question: "Which database?".into(),
        recommendation: "SQLite".into(),
        reason: None,
        choices: Vec::new(),
    };
    let question = daemon
        .manager
        .ask_question(
            &SpaceSelector::from(&workspace.name),
            ask,
            Vec::new(),
            &Attacher::User,
        )
        .await
        .unwrap();
    let app = daemon.connector();
    let answer = |shown| QuestionMethod::AnswerQuestion {
        number: question.number,
        answer: "Postgres".into(),
        shown,
    };
    let stale = Question {
        ask: Ask {
            question: "Which cache?".into(),
            ..question.ask.clone()
        },
        ..question.clone()
    };
    assert_eq!(
        code(connection::request::<Question>(&app, answer(Some(stale))).await),
        ErrorCode::NotFound
    );
    let answered: Question = connection::request(&app, answer(Some(question.clone())))
        .await
        .unwrap();
    assert_eq!(answered.status.answer(), Some("Postgres"));

    let request = ApprovalRequest::new(
        &workspace.id,
        Target::Port("web".into()),
        "web",
        Specification::Port(PortSpecification {
            env: "PORT".into(),
            on_conflict: ConflictPolicy::Suggest,
            preferred: None,
            range: [4000, 4100],
        }),
        Lifetime::Workspace,
        Some("serve the app"),
    );
    let pending = daemon
        .manager
        .request_approval(request)
        .await
        .unwrap()
        .unwrap();
    let decide = |shown: Option<ApprovalRequest>| ApprovalMethod::DecideApproval {
        number: pending.number,
        approve: true,
        shown: shown.map(Box::new),
        details: Default::default(),
        env: None,
    };
    let agent = daemon.scoped(&workspace).await;
    assert_eq!(
        code(connection::call(&agent, decide(None)).await),
        ErrorCode::ScopeDenied
    );
    let stale = ApprovalRequest {
        key: "a released request's key".into(),
        ..pending.clone()
    };
    assert_eq!(
        code(connection::call(&app, decide(Some(stale))).await),
        ErrorCode::NotFound
    );
    let Body::ApprovalRequest(decided) = connection::call(&app, decide(Some(pending.clone())))
        .await
        .unwrap()
    else {
        panic!("expected the decided request");
    };
    assert_eq!(decided.number, pending.number);
}

#[tokio::test]
async fn an_app_queues_an_agent_once_it_can_run() {
    let daemon = TestDaemon::start().await;
    // An agent whose command is installed wherever the tests run.
    crate::daemon::test_support::configure(
        &daemon.manager,
        "[agents.probe]\ncommand = [\"true\"]\n",
    )
    .await;
    daemon.workspace("worker").await;
    let app = daemon.connector();
    let queue = |agent: &str| QueueMethod::QueueAgent {
        workspace: "worker".into(),
        agent: agent.into(),
        prompt: Some("Fix the build".into()),
    };
    assert!(
        connection::request::<QueueEntry>(&app, queue("undefined"))
            .await
            .is_err()
    );
    let queued: QueueEntry = connection::request(&app, queue("probe")).await.unwrap();
    let plan = daemon.manager.queued_plan(&queued.id).await.unwrap().plan;
    let plan: serde_json::Value = serde_json::from_str(&plan).unwrap();
    // A launcher's `{prompt}` receives it rather than its forwarded arguments.
    assert_eq!(plan["prompt"], "Fix the build");
    assert_eq!(queued.agent, "probe");

    // A workspace's agent queues agents only in the workspaces below its own.
    let parent = daemon.workspace("parent").await;
    let split = WorkspaceMethod::SplitWorkspace {
        workspace: SpaceSelector::id(&parent.id),
        name: "child".into(),
        base: Some("HEAD".into()),
        options: Default::default(),
    };
    connection::request::<Workspace>(&app, split).await.unwrap();
    let agent = daemon.scoped(&parent).await;
    let queue_in = |workspace: &str| QueueMethod::QueueAgent {
        workspace: workspace.into(),
        agent: "probe".into(),
        prompt: None,
    };
    assert_eq!(
        code(connection::request::<QueueEntry>(&agent, queue_in("parent")).await),
        ErrorCode::ScopeDenied
    );
    let child: QueueEntry = connection::request(&agent, queue_in("child"))
        .await
        .unwrap();
    assert_eq!(child.agent, "probe");
}

/// Follow operation `id` from its start to its end.
async fn follow(app: &Connector, id: &str) -> Ended {
    let mut following = Following::open(app, id.into(), 0).await.unwrap();
    tokio::time::timeout(HANG, async {
        loop {
            if let Followed::Ended(ended) = following.next().await.unwrap() {
                return ended;
            }
        }
    })
    .await
    .unwrap()
}

/// A request to start workspace `start` of the test repository from HEAD.
fn start_workspace(daemon: &TestDaemon, start: WorkspaceStart) -> OperationMethod {
    OperationMethod::StartWorkspace {
        repository: daemon.repository.id.clone().into(),
        start,
        base: Some("HEAD".into()),
        path: None,
        git_profile: None,
        parent: None,
        options: Default::default(),
        issue: None,
        pr: None,
        agent: None,
        env: None,
    }
}

#[tokio::test]
async fn a_created_workspace_is_prepared_in_an_operation_the_app_follows() {
    let daemon = TestDaemon::start().await;
    let app = daemon.connector();
    let create = start_workspace(&daemon, WorkspaceStart::Name("api".into()));
    let started: StartedWorkspace = connection::request(&app, create).await.unwrap();
    assert_eq!(started.workspace.name, "api");
    assert!(!started.reused);
    let worker = fake::take(&started.operation);
    let OperationSpec::Prepare { workspace, steps } = worker.order.spec.clone() else {
        panic!("expected a preparation: {:?}", worker.order);
    };
    assert_eq!(workspace, SpaceSelector::id(&started.workspace.id));
    assert!(steps.post_setup && steps.pr.is_none() && steps.agent.is_none());
    let failure = Failure::SetupFailed {
        message: "setup command exited with status 2".into(),
    };
    let report = Report::Ended(Ended::Failed(failure));
    daemon
        .manager
        .report_operation(&started.operation, report, None)
        .await
        .unwrap();
    worker.exit(0);
    let Ended::Failed(failure) = follow(&app, &started.operation).await else {
        panic!("expected the failure");
    };
    assert_eq!(failure.code(), ErrorCode::SetupFailed);
}

#[tokio::test]
async fn an_existing_branch_reopens_its_workspace_without_its_post_setup_hook() {
    let daemon = TestDaemon::start().await;
    let app = daemon.connector();
    let existing = daemon.workspace("api").await;
    let mut open = start_workspace(&daemon, WorkspaceStart::Branch("api".into()));
    if let OperationMethod::StartWorkspace { base, pr, .. } = &mut open {
        *base = None;
        *pr = Some("https://github.com/team/repo/pull/7".into());
    }
    let started: StartedWorkspace = connection::request(&app, open).await.unwrap();
    assert!(started.reused);
    assert_eq!(started.workspace.id, existing.id);
    let worker = fake::take(&started.operation);
    let OperationSpec::Prepare { steps, .. } = worker.order.spec.clone() else {
        panic!("expected a preparation: {:?}", worker.order);
    };
    assert!(!steps.setup && !steps.post_setup);
    assert_eq!(
        steps.pr.as_deref(),
        Some("https://github.com/team/repo/pull/7")
    );
    worker.exit(1);
}

#[tokio::test]
async fn only_the_user_reopens_a_recorded_workspace_for_a_launch() {
    let daemon = TestDaemon::start().await;
    let existing = daemon.workspace("api").await;
    let mut reopen = start_workspace(&daemon, WorkspaceStart::Workspace("api".into()));
    if let OperationMethod::StartWorkspace { base, .. } = &mut reopen {
        *base = None;
    }
    let agent = daemon.scoped(&existing).await;
    assert_eq!(
        code(connection::request::<StartedWorkspace>(&agent, reopen).await),
        ErrorCode::ScopeDenied
    );
    let app = daemon.connector();
    let based = start_workspace(&daemon, WorkspaceStart::Workspace("api".into()));
    assert!(
        connection::request::<StartedWorkspace>(&app, based)
            .await
            .is_err(),
        "a recorded workspace takes no base"
    );
    let mut reopen = start_workspace(&daemon, WorkspaceStart::Workspace("api".into()));
    if let OperationMethod::StartWorkspace { base, .. } = &mut reopen {
        *base = None;
    }
    let started: StartedWorkspace = connection::request(&app, reopen).await.unwrap();
    assert!(started.reused);
    assert_eq!(started.workspace.id, existing.id);
    let worker = fake::take(&started.operation);
    let OperationSpec::Prepare { steps, .. } = worker.order.spec.clone() else {
        panic!("expected a preparation: {:?}", worker.order);
    };
    assert!(!steps.setup && !steps.post_setup);
    worker.exit(1);
}

#[tokio::test]
async fn creation_refuses_a_bad_agent_or_issue_without_leaving_a_workspace() {
    let daemon = TestDaemon::start().await;
    let app = daemon.connector();
    let launch = |agent: &str, mode: Option<&str>| {
        Box::new(QueuedLaunch {
            agent: agent.into(),
            mode: mode.map(Into::into),
            ..Default::default()
        })
    };
    let create = |issue: Option<&str>, agent: Option<Box<QueuedLaunch>>| {
        let mut create = start_workspace(&daemon, WorkspaceStart::Name("api".into()));
        if let OperationMethod::StartWorkspace {
            issue: requested,
            agent: queued,
            ..
        } = &mut create
        {
            *requested = issue.map(Into::into);
            *queued = agent;
        }
        create
    };
    for refused in [
        create(None, Some(launch("undefined", None))),
        create(None, Some(launch("claude", Some("triage")))),
        create(Some("not an issue"), None),
    ] {
        assert!(
            connection::request::<StartedWorkspace>(&app, refused)
                .await
                .is_err()
        );
    }
    assert_eq!(code(inspect(&app, "api").await), ErrorCode::NotFound);
    let split = |agent| OperationMethod::StartSplit {
        workspace: "parent".into(),
        name: Some("child".into()),
        copy: None,
        base: Some("HEAD".into()),
        options: Default::default(),
        issue: None,
        agent,
        env: None,
    };
    // A workspace's agent starts workspaces below its own.
    let parent = daemon.workspace("parent").await;
    let agent = daemon.scoped(&parent).await;
    let started: StartedWorkspace = connection::request(&agent, create(None, None))
        .await
        .unwrap();
    let below = started.workspace.parent.as_ref().map(|space| &space.id);
    assert_eq!(below, Some(&parent.id));
    fake::take(&started.operation).exit(1);
    let undefined_mode = split(Some(launch("claude", Some("triage"))));
    let error = connection::request::<StartedWorkspace>(&agent, undefined_mode)
        .await
        .unwrap_err();
    assert!(format!("{error:#}").contains("no mode triage"), "{error:#}");
    assert_eq!(code(inspect(&app, "child").await), ErrorCode::NotFound);
    // A mode only a file beside the global config defines is one.
    let paths = &daemon.manager.paths;
    crate::test_support::write_global_config(paths, "[agents.fake]\ncommand = ['sh']\n");
    crate::test_support::write_beside_global_config(paths, "modes/triage.md", "Label it.");
    daemon.manager.reload_config().await.unwrap();
    let in_file_mode = split(Some(launch("fake", Some("triage"))));
    let started: StartedWorkspace = connection::request(&agent, in_file_mode).await.unwrap();
    let worker = fake::take(&started.operation);
    assert!(
        worker.token.is_some(),
        "the worker acts with the caller's scope"
    );
    worker.exit(1);
}

#[tokio::test]
async fn a_chat_parent_in_another_repository_is_refused_before_the_chat_starts() {
    let daemon = TestDaemon::start().await;
    let app = daemon.connector();
    let parent = daemon.workspace("parent").await;
    let root = tempfile::tempdir().unwrap();
    let other = crate::daemon::test_support::repository(root.path(), "other");
    let other = daemon
        .manager
        .register_repository(other.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let check = |repository: &Repository, workspace: Option<&str>, parent: &str| {
        crate::protocol::ChatMethod::CheckChatParent {
            repository: Some(repository.id.clone().into()),
            workspace: workspace.map(SpaceSelector::id),
            parent: Some(SpaceSelector::parse(parent)),
        }
    };
    let refused = |method| async { connection::request::<()>(&app, method).await.is_err() };
    connection::request::<()>(&app, check(&daemon.repository, None, &parent.id))
        .await
        .unwrap();
    assert!(refused(check(&other, None, &parent.id)).await);
    // Nor does a chat go below the workspace it runs in.
    assert!(refused(check(&daemon.repository, Some(&parent.id), &parent.id)).await);
    // A workspace that is not ready takes no child, chat or workspace.
    let failed = daemon.workspace("failed").await;
    (daemon.manager)
        .set_state(&failed.id, crate::state::WorkspaceState::Failed, None)
        .await
        .unwrap();
    assert!(refused(check(&daemon.repository, None, &failed.id)).await);
    let creation = WorkspaceMethod::CheckCreation {
        repository: daemon.repository.id.clone().into(),
        branch: Some("child".into()),
        existing: None,
        base: None,
        git_profile: None,
        path: None,
        options: Default::default(),
        parent: Some(SpaceSelector::id(&failed.id)),
    };
    assert!(connection::request::<()>(&app, creation).await.is_err());
    let mut child = start_workspace(&daemon, WorkspaceStart::Name("child".into()));
    if let OperationMethod::StartWorkspace { parent, .. } = &mut child {
        *parent = Some(SpaceSelector::id(&failed.id));
    }
    assert!(
        connection::request::<StartedWorkspace>(&app, child)
            .await
            .is_err()
    );
    assert_eq!(code(inspect(&app, "child").await), ErrorCode::NotFound);
}

#[tokio::test]
async fn a_chats_agent_starts_workspaces_below_its_chat() {
    let daemon = TestDaemon::start().await;
    let chat = daemon
        .manager
        .start_chat(
            daemon.repository.id.clone().into(),
            "planner".into(),
            "claude".into(),
            std::process::id(),
            None,
            false,
        )
        .await
        .unwrap();
    let token = daemon.manager.chat_scope(&chat.id).await.unwrap();
    let agent = daemon.connector().scope(token);
    let create = start_workspace(&daemon, WorkspaceStart::Name("api".into()));
    let started: StartedWorkspace = connection::request(&agent, create).await.unwrap();
    let parent = started.workspace.parent.as_ref().map(|parent| &parent.id);
    assert_eq!(parent, Some(&chat.id));
    let worker = fake::take(&started.operation);
    assert!(
        worker.token.is_some(),
        "the worker acts with the chat's scope"
    );
    worker.exit(1);
}

#[tokio::test]
async fn following_ends_once_the_followers_scope_token_is_revoked() {
    let daemon = TestDaemon::start().await;
    let own = daemon.workspace("own").await;
    let merge = OperationMethod::StartOperation {
        spec: OperationSpec::Merge {
            workspace: "own".into(),
            options: Default::default(),
        },
        env: None,
    };
    let started: crate::protocol::operations::Started =
        connection::request(&daemon.connector(), merge)
            .await
            .unwrap();
    let mut worker = fake::take(&started.id);
    let token = daemon.scope_token(&own).await;
    let agent = daemon.connector().scope(token.clone());
    let mut following = Following::open(&agent, started.id.clone(), 0)
        .await
        .unwrap();
    worker.stdout("one").await;
    let first = following.next().await.unwrap();
    assert!(matches!(first, Followed::Event(_)), "{first:?}");
    daemon
        .manager
        .revoke_workspace_environment(&SpaceSelector::id(&own.id), token)
        .await
        .unwrap();
    worker.stdout("two").await;
    let error = following.next().await.unwrap_err();
    assert_eq!(
        error.downcast_ref::<RemoteError>().unwrap().code,
        ErrorCode::ScopeDenied
    );
    worker.exit(0);
}
