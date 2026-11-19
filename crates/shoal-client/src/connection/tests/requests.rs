//! Requests an app sends, as the user and as a workspace's agent, to a
//! daemon on an isolated state directory.
use crate::connection::{self, Connector};
use crate::forge::items::link::LinkKind;
use crate::test_daemon::{Connect as _, code, queued_plan, start_workspace};
use shoal_core::config::repo::ConflictPolicy;
use shoal_daemon::test_daemon::ApprovalRequestExt as _;
use shoal_daemon::test_daemon::{TestDaemon, fake};
use shoal_protocol::approvals::{ApprovalRequest, PortSpecification, Specification, Target};
use shoal_protocol::config::repo::Lifetime;
use shoal_protocol::files::Attacher;
use shoal_protocol::model::{
    Completion, QueueEntry, Relation, Repository, SyncedRepository, Workspace,
};
use shoal_protocol::protocol::operations::LaunchRequest;
use shoal_protocol::protocol::{
    ApprovalMethod, AssignmentMethod, Body, ErrorCode, HoldMethod, ItemMethod, OperationMethod,
    QuestionMethod, QueueMethod, RemovalMethod, RepositoryMethod, WorkspaceMethod,
    operations::{OperationSpec, StartedWorkspace, WorkspaceStart},
};
use shoal_protocol::questions::{Ask, Question};
use shoal_protocol::removal::{RemovalResult, SpaceRemovalCheck, UnsavedWork};
use shoal_protocol::space_names::SpaceSelector;
use shoal_protocol::status::Inspection;
use shoal_refs::BranchName;

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
    assert_eq!(inspect(&app, own.id.as_str()).await.unwrap().name, "own");
    assert_eq!(code(inspect(&app, "missing").await), ErrorCode::NotFound);
}

#[tokio::test]
async fn a_scoped_app_reaches_only_its_workspace_tree() {
    let daemon = TestDaemon::start().await;
    let own = daemon.workspace("own").await;
    let other = daemon.workspace("other").await;
    let agent = daemon.scoped(&own).await;
    assert_eq!(inspect(&agent, own.id.as_str()).await.unwrap().id, own.id);
    assert_eq!(
        code(inspect(&agent, other.id.as_str()).await),
        ErrorCode::ScopeDenied
    );
    // A workspace's agent asks to hold its own workspace.
    let hold = HoldMethod::HoldAcquire {
        space: SpaceSelector::id(&own.id),
        name: "review".into(),
        reason: Some("open in Quay".into()),
    };
    let Body::ApprovalRequest(request) = connection::call(&agent, hold).await.unwrap() else {
        panic!("expected an approval request");
    };
    assert_eq!(request.target, Target::Hold("review".into()));
    let unknown = daemon.connector().scope("unknown".into());
    assert_eq!(code(inspect(&unknown, "own").await), ErrorCode::ScopeDenied);
}

#[tokio::test]
async fn a_chats_agent_asks_to_hold_its_own_chat() {
    let daemon = TestDaemon::start().await;
    let chat = (daemon.manager)
        .start_chat(
            daemon.repository.id.clone().into(),
            "notes".into(),
            "claude".into(),
            None,
        )
        .await
        .unwrap();
    let token = daemon.manager.chat_scope(&chat.id).await.unwrap();
    let agent = daemon.connector().scope(token);
    let hold = HoldMethod::HoldAcquire {
        space: SpaceSelector::id(&chat.id),
        name: "review".into(),
        reason: Some("open in Quay".into()),
    };
    let Body::ApprovalRequest(request) = connection::call(&agent, hold).await.unwrap() else {
        panic!("expected an approval request");
    };
    assert_eq!(
        (request.space_id, request.target),
        (chat.id, Target::Hold("review".into()))
    );
}

#[tokio::test]
async fn removal_refuses_unsaved_work_unless_the_app_confirms_it() {
    let daemon = TestDaemon::start().await;
    let workspace = daemon.workspace("dirty").await;
    std::fs::write(workspace.path.join("draft"), "unsaved\n").unwrap();
    let app = daemon.connector();
    let remove = |unsaved| RemovalMethod::RemoveSpace {
        space: SpaceSelector::id(&workspace.id),
        unsaved,
        keep_user_holds: false,
        above: None,
    };
    assert_eq!(
        code(connection::request::<RemovalResult>(&app, remove(UnsavedWork::default())).await),
        ErrorCode::UnsavedWork
    );
    let check = RemovalMethod::CheckRemoval {
        space: SpaceSelector::id(&workspace.id),
        include_changes: false,
    };
    let check: Box<SpaceRemovalCheck> = connection::request(&app, check).await.unwrap();
    let confirmed = remove(UnsavedWork::confirming(&check));
    let removal: RemovalResult = connection::request(&app, confirmed).await.unwrap();
    assert_eq!(
        removal.branch.as_ref().map(BranchName::as_str),
        Some("dirty")
    );
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
    };
    let completion: Completion = connection::request(&app, done).await.unwrap();
    assert!(!completion.head.as_str().is_empty());
    assert!(inspect(&app, workspace.id.as_str()).await.unwrap().done);
    let rename = |branch: &str| WorkspaceMethod::RenameBranch {
        workspace: SpaceSelector::id(&workspace.id),
        branch: branch.into(),
    };
    assert_eq!(
        code(connection::request::<Workspace>(&app, rename("second")).await),
        ErrorCode::NameTaken
    );
    let link = ItemMethod::LinkItem {
        space: SpaceSelector::id(&workspace.id),
        kind: LinkKind::Issue,
        item: "40".parse().unwrap(),
        relation: Relation::Related,
        title: Some("Report".into()),
    };
    connection::request::<shoal_protocol::links::Linked>(&app, link)
        .await
        .unwrap();
    let unlink = || ItemMethod::UnlinkItems {
        space: SpaceSelector::id(&workspace.id),
        relation: Relation::Related,
        kind: Some(LinkKind::Issue),
        item: Some("40".parse().unwrap()),
    };
    connection::request::<shoal_protocol::links::Unlinked>(&app, unlink())
        .await
        .unwrap();
    assert_eq!(
        code(connection::request::<shoal_protocol::links::Unlinked>(&app, unlink()).await),
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
            &SpaceSelector::id(&workspace.id),
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

/// A request to queue the worker of `workspace` with `launch`.
fn queue_worker(workspace: &str, launch: LaunchRequest) -> QueueMethod {
    QueueMethod::QueueAgent {
        workspace: workspace.into(),
        launch: Box::new(launch),
        blockers: Vec::new(),
        pr: None,
    }
}

#[tokio::test]
async fn an_app_queues_an_agent_once_it_can_run() {
    let daemon = TestDaemon::start().await;
    // An agent whose command is installed wherever the tests run.
    shoal_daemon::test_daemon::configure(&daemon.manager, "[agents.probe]\ncommand = [\"true\"]\n")
        .await;
    let worker = daemon.workspace("worker").await;
    let app = daemon.connector();
    let queue = |agent: &str| {
        let launch = LaunchRequest {
            prompt: Some("Fix the build".into()),
            ..LaunchRequest::new(Some(agent.into()))
        };
        queue_worker(worker.id.as_str(), launch)
    };
    assert!(
        connection::request::<QueueEntry>(&app, queue("undefined"))
            .await
            .is_err()
    );
    let queued: QueueEntry = connection::request(&app, queue("probe")).await.unwrap();
    let plan = queued_plan(&daemon, &queued).await;
    // A launcher's `{prompt}` receives it rather than its forwarded arguments.
    assert_eq!(plan["launch"]["prompt"], "Fix the build");
    assert_eq!(queued.agent, "probe");

    // A workspace's agent queues agents only in the workspaces below its own,
    // and regardless of the limits as its limits permission allows.
    let parent = daemon.workspace("parent").await;
    let split = WorkspaceMethod::CreateChild {
        parent: SpaceSelector::id(&parent.id),
        name: "child".into(),
        base: Some("HEAD".into()),
        options: Default::default(),
    };
    let child = connection::request::<Workspace>(&app, split).await.unwrap();
    let agent = daemon.scoped(&parent).await;
    let probe = || LaunchRequest::new(Some("probe".into()));
    assert_eq!(
        code(
            connection::request::<QueueEntry>(&agent, queue_worker(parent.id.as_str(), probe()))
                .await
        ),
        ErrorCode::ScopeDenied
    );
    let now = LaunchRequest {
        now: true,
        ..probe()
    };
    let error = (connection::request::<QueueEntry>(&agent, queue_worker(child.id.as_str(), now)))
        .await
        .unwrap_err();
    let Some(shoal_protocol::approvals::Undecided(request)) = error.downcast_ref() else {
        panic!("expected an approval request: {error:#}");
    };
    assert_eq!(
        request.target,
        shoal_protocol::approvals::Target::Action(
            shoal_core::config::permissions::PermissionKey::Limits
        )
    );
    let child: QueueEntry = connection::request(&agent, queue_worker(child.id.as_str(), probe()))
        .await
        .unwrap();
    assert_eq!(child.agent, "probe");
}

#[tokio::test]
async fn a_worker_without_an_agent_or_task_takes_the_default_on_its_issue() {
    let daemon = TestDaemon::start().await;
    shoal_daemon::test_daemon::configure(
        &daemon.manager,
        "[agent]\ndefault = \"probe\"\n[agents.probe]\ncommand = [\"true\"]\n",
    )
    .await;
    let worker = daemon.workspace("worker").await;
    let app = daemon.connector();
    let link = ItemMethod::LinkItem {
        space: SpaceSelector::id(&worker.id),
        kind: LinkKind::Issue,
        item: "40".parse().unwrap(),
        relation: Relation::Assigned,
        title: Some("Report".into()),
    };
    connection::request::<shoal_protocol::links::Linked>(&app, link)
        .await
        .unwrap();
    let launch = LaunchRequest::new(None);
    let queued: QueueEntry = connection::request(&app, queue_worker(worker.id.as_str(), launch))
        .await
        .unwrap();
    assert_eq!(queued.agent, "probe");
    assert_eq!(queued.issue_title.as_deref(), Some("Report"));
    let plan = queued_plan(&daemon, &queued).await;
    assert!(
        plan["launch"]["issue"]["url"]
            .as_str()
            .is_some_and(|url| url.ends_with("/40")),
        "{plan}"
    );
}

#[tokio::test]
async fn an_existing_branch_reopens_its_workspace_without_its_post_setup_hook() {
    let daemon = TestDaemon::start().await;
    let app = daemon.connector();
    let existing = daemon.workspace("api").await;
    let mut open = start_workspace(&daemon, WorkspaceStart::Branch("api".into()));
    if let OperationMethod::StartWorkspace { base, pr, .. } = &mut open {
        *base = None;
        *pr = Some("https://github.com/team/repo/pull/7".parse().unwrap());
    }
    let started: StartedWorkspace = connection::request(&app, open).await.unwrap();
    assert!(started.reused);
    assert_eq!(started.workspace.id, existing.id);
    let worker = fake::take(started.operation.as_deref().unwrap());
    let OperationSpec::Prepare { steps, .. } = worker.order.spec.clone() else {
        panic!("expected a preparation: {:?}", worker.order);
    };
    assert!(!steps.setup && !steps.post_setup);
    assert_eq!(
        steps.pr.as_ref().map(ToString::to_string).as_deref(),
        Some("https://github.com/team/repo/pull/7")
    );
    worker.exit(1);
}

#[tokio::test]
async fn a_scoped_start_reopens_only_a_workspace_below_its_own() {
    let daemon = TestDaemon::start().await;
    let existing = daemon.workspace("api").await;
    let child = daemon
        .manager
        .create_child(
            &SpaceSelector::id(&existing.id),
            "child".into(),
            None,
            Default::default(),
        )
        .await
        .unwrap();
    let reopen = |workspace: &Workspace| {
        let mut reopen = start_workspace(
            &daemon,
            WorkspaceStart::Workspace(SpaceSelector::id(&workspace.id)),
        );
        if let OperationMethod::StartWorkspace { base, .. } = &mut reopen {
            *base = None;
        }
        reopen
    };
    let agent = daemon.scoped(&existing).await;
    let own = connection::request::<StartedWorkspace>(&agent, reopen(&existing)).await;
    let refusal = format!("{:#}", own.unwrap_err());
    assert!(refusal.contains("below theirs"), "{refusal}");
    let started: StartedWorkspace = connection::request(&agent, reopen(&child)).await.unwrap();
    assert!(started.reused);
    assert_eq!(started.workspace.id, child.id);
    fake::take(started.operation.as_deref().unwrap()).exit(1);
    let app = daemon.connector();
    let based = start_workspace(
        &daemon,
        WorkspaceStart::Workspace(SpaceSelector::id(&existing.id)),
    );
    assert!(
        connection::request::<StartedWorkspace>(&app, based)
            .await
            .is_err(),
        "a recorded workspace takes no base"
    );
    let mut reopen = start_workspace(
        &daemon,
        WorkspaceStart::Workspace(SpaceSelector::id(&existing.id)),
    );
    if let OperationMethod::StartWorkspace { base, .. } = &mut reopen {
        *base = None;
    }
    let started: StartedWorkspace = connection::request(&app, reopen).await.unwrap();
    assert!(started.reused);
    assert_eq!(started.workspace.id, existing.id);
    let worker = fake::take(started.operation.as_deref().unwrap());
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
        Box::new(LaunchRequest {
            mode: mode.map(Into::into),
            ..LaunchRequest::new(Some(agent.into()))
        })
    };
    let create = |issue: Option<&str>, agent: Option<Box<LaunchRequest>>| {
        let mut create = start_workspace(&daemon, WorkspaceStart::Name("api".into()));
        if let OperationMethod::StartWorkspace {
            issue: requested,
            agent: queued,
            ..
        } = &mut create
        {
            *requested = issue.map(|issue| issue.parse().unwrap());
            *queued = agent;
        }
        create
    };
    for refused in [
        create(None, Some(launch("undefined", None))),
        create(None, Some(launch("claude", Some("triage")))),
        create(Some("https://forge.example/team/other/issues/1"), None),
    ] {
        assert!(
            connection::request::<StartedWorkspace>(&app, refused)
                .await
                .is_err()
        );
    }
    assert_eq!(code(inspect(&app, "api").await), ErrorCode::NotFound);
    // A workspace's agent starts workspaces below its own.
    let parent = daemon.workspace("parent").await;
    let split = |agent| OperationMethod::StartChild {
        parent: SpaceSelector::id(&parent.id),
        name: Some("child".into()),
        copy: None,
        base: Some("HEAD".into()),
        options: Default::default(),
        issue: None,
        agent,
        env: None,
    };
    let agent = daemon.scoped(&parent).await;
    let started: StartedWorkspace = connection::request(&agent, create(None, None))
        .await
        .unwrap();
    let below = started.workspace.parent.as_ref().map(|space| &space.id);
    assert_eq!(below, Some(&parent.id));
    fake::take(started.operation.as_deref().unwrap()).exit(1);
    let undefined_mode = split(Some(launch("claude", Some("triage"))));
    let error = connection::request::<StartedWorkspace>(&agent, undefined_mode)
        .await
        .unwrap_err();
    assert!(format!("{error:#}").contains("no mode triage"), "{error:#}");
    // Nor does a split on previous-checkout syntax for its branch.
    let mut previous = split(None);
    if let OperationMethod::StartChild { name, .. } = &mut previous {
        *name = Some("@{-1}".into());
    }
    let error = connection::request::<StartedWorkspace>(&agent, previous)
        .await
        .unwrap_err();
    assert!(format!("{error:#}").contains("Git branch"), "{error:#}");
    assert_eq!(code(inspect(&app, "child").await), ErrorCode::NotFound);
    // A mode only a file beside the global config defines is one.
    let paths = &daemon.manager.paths;
    shoal_core::test_support::write_global_config(paths, "[agents.fake]\ncommand = ['sh']\n");
    shoal_core::test_support::write_beside_global_config(paths, "modes/triage.md", "Label it.");
    daemon.manager.reload_config().await.unwrap();
    let in_file_mode = split(Some(launch("fake", Some("triage"))));
    let started: StartedWorkspace = connection::request(&agent, in_file_mode).await.unwrap();
    let worker = fake::take(started.operation.as_deref().unwrap());
    assert!(
        worker.token.is_some(),
        "the worker acts with the caller's scope"
    );
    worker.exit(1);
}

#[tokio::test]
async fn a_start_without_an_agent_queues_the_configured_default() {
    let daemon = TestDaemon::start().await;
    let app = daemon.connector();
    let create = |name: &str| {
        let mut create = start_workspace(&daemon, WorkspaceStart::Name(name.into()));
        if let OperationMethod::StartWorkspace { agent, .. } = &mut create {
            *agent = Some(Box::new(LaunchRequest::new(None)));
        }
        create
    };
    let error = connection::request::<StartedWorkspace>(&app, create("unnamed"))
        .await
        .unwrap_err();
    assert!(format!("{error:#}").contains("agent.default"), "{error:#}");
    assert_eq!(code(inspect(&app, "unnamed").await), ErrorCode::NotFound);
    shoal_daemon::test_daemon::configure(
        &daemon.manager,
        "[agent]\ndefault = \"probe\"\n[agents.probe]\ncommand = [\"true\"]\n",
    )
    .await;
    let started: StartedWorkspace = connection::request(&app, create("api")).await.unwrap();
    let worker = fake::take(started.operation.as_deref().unwrap());
    let entry = started.entry.expect("the start queued its agent");
    assert_eq!(entry.agent, "probe");
    worker.exit(1);
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
            None,
        )
        .await
        .unwrap();
    let token = daemon.manager.chat_scope(&chat.id).await.unwrap();
    let agent = daemon.connector().scope(token);
    let create = start_workspace(&daemon, WorkspaceStart::Name("api".into()));
    let started: StartedWorkspace = connection::request(&agent, create).await.unwrap();
    let parent = started.workspace.parent.as_ref().map(|parent| &parent.id);
    assert_eq!(parent, Some(&chat.id));
    let worker = fake::take(started.operation.as_deref().unwrap());
    assert!(
        worker.token.is_some(),
        "the worker acts with the chat's scope"
    );
    worker.exit(1);
}

#[tokio::test]
async fn a_scoped_start_takes_no_queue_entry_of_another_launch() {
    let daemon = TestDaemon::start().await;
    daemon
        .manager
        .publish_config(shoal_core::test_support::global_config(
            "[machine]\nmax_heavy_agents = 1\n[machine.overload.disk]\nenabled = false",
        ));
    let parent = daemon.workspace("parent").await;
    let child = daemon
        .manager
        .create_child(
            &SpaceSelector::id(&parent.id),
            "child".into(),
            None,
            Default::default(),
        )
        .await
        .unwrap();
    // The child's agent holds a slot, which the parent's scope reaches.
    let request = shoal_protocol::model::QueueRequest {
        source: None,
        issue_labels: Vec::new(),
        narrowing: Default::default(),
        mode: None,
        name: "child".into(),
        agent: "claude".into(),
        issue_title: None,
        plan: "{}".into(),
        env: Default::default(),
        now: false,
        space: SpaceSelector::id(&child.id),
        starts_in_caller: true,
        blockers: Vec::new(),
    };
    let theirs = (daemon.manager.queue_add(request))
        .await
        .unwrap()
        .entry
        .unwrap();
    let agent = daemon.scoped(&parent).await;
    let mut create = start_workspace(&daemon, WorkspaceStart::Name("api".into()));
    if let OperationMethod::StartWorkspace { queue_entry, .. } = &mut create {
        *queue_entry = Some(theirs.id.clone());
    }
    let refused = connection::request::<StartedWorkspace>(&agent, create).await;
    let refusal = format!("{:#}", refused.unwrap_err());
    assert!(refusal.contains("another launch's"), "{refusal}");
    let entries = daemon.manager.queue_entries().await.unwrap();
    let entry = entries.iter().find(|entry| entry.id == theirs.id).unwrap();
    assert_eq!(entry.space_id, None, "{entry:?}");
}

#[tokio::test]
async fn a_workspaces_agent_renames_the_spaces_its_scope_reaches() {
    let daemon = TestDaemon::start().await;
    let app = daemon.connector();
    let parent = daemon.workspace("parent").await;
    let outside = daemon.workspace("outside").await;
    let split = WorkspaceMethod::CreateChild {
        parent: SpaceSelector::id(&parent.id),
        name: "child".into(),
        base: Some("HEAD".into()),
        options: Default::default(),
    };
    let child = connection::request::<Workspace>(&app, split).await.unwrap();
    let agent = daemon.scoped(&parent).await;
    let rename = |space: &Workspace, name: &str| {
        crate::workspaces::rename_space(&agent, SpaceSelector::id(&space.id), name.into())
    };

    let renamed = rename(&parent, "Login retries").await.unwrap();
    assert_eq!(&renamed.space.id, &parent.id);
    assert_eq!(&renamed.space.name, "Login retries");
    assert_eq!(renamed.previous_name, "parent");
    let renamed = rename(&child, "Retry backoff").await.unwrap();
    assert_eq!(&renamed.space.id, &child.id);
    assert_eq!(renamed.previous_name, "child");
    // A space outside its scope keeps its name.
    assert_eq!(
        code(rename(&outside, "Other").await),
        ErrorCode::ScopeDenied
    );
    assert_eq!(
        inspect(&app, outside.id.as_str()).await.unwrap().name,
        "outside"
    );
}
