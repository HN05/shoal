//! An app's calls through `shoal::client`, as the user and as a
//! workspace's agent, against a daemon on an isolated state directory.
use crate::{
    client::{
        approvals::ApprovalRequest,
        connection::{self, Connector},
        questions::Question,
        removal::{RemovalCheck, RemovalResult, UnsavedWork},
    },
    config::repo::ConflictPolicy,
    daemon::{
        approvals::{Lifetime, PortSpecification, Specification, Target},
        operations::fake,
        questions::Ask,
    },
    forge::items::link::LinkKind,
    model::{
        Completion, Inspection, QueueEntry, Relation, Repository, SyncedRepository, Workspace,
    },
    protocol::{
        ApprovalMethod, AssignmentMethod, Body, ErrorCode, HoldMethod, ItemMethod, OperationMethod,
        QuestionMethod, QueueMethod, RemoteError, RemovalMethod, RepositoryMethod, WorkspaceMethod,
        operations::{Ended, Failure, OperationSpec, QueuedLaunch, Report, StartedWorkspace},
    },
};

use super::{HANG, TestDaemon};

/// The next response on `stream`.
async fn protocol_read(
    stream: &mut tokio::io::BufReader<tokio::net::UnixStream>,
) -> crate::protocol::Response {
    crate::protocol::read_response(stream).await.unwrap()
}

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
        workspace: workspace.id.clone(),
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
    let done = AssignmentMethod::WorkspaceDone {
        workspace: workspace.id.clone(),
        cleanup: false,
        integrated: false,
        results: Vec::new(),
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
        workspace: workspace.id.clone(),
        kind: LinkKind::Issue,
        url: "40".into(),
        relation: Relation::Related,
        title: None,
    };
    connection::request::<()>(&app, link).await.unwrap();
    let unlink = || ItemMethod::UnlinkItems {
        workspace: workspace.id.clone(),
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
        .ask_question(&workspace.name, ask)
        .await
        .unwrap();
    let app = daemon.connector();
    let answer = |shown| QuestionMethod::AnswerQuestion {
        id: question.id,
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
    assert_eq!(answered.answer.as_deref(), Some("Postgres"));

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
        id: pending.id,
        approve: true,
        shown: shown.map(Box::new),
        details: Default::default(),
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
    assert_eq!(decided.subject(), "port web (workspace)");
}

#[tokio::test]
async fn an_app_queues_an_agent_once_it_can_run() {
    let daemon = TestDaemon::start().await;
    // An agent whose command is installed wherever the tests run.
    crate::test_support::configure(&daemon.manager, "[agents.probe]\ncommand = [\"true\"]\n").await;
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
        workspace: parent.id.clone(),
        name: "child".into(),
        base: Some("HEAD".into()),
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
    let method = OperationMethod::FollowOperation {
        id: id.into(),
        after: 0,
    };
    let (mut stream, body) = connection::open_buffered(app, method).await.unwrap();
    body.into_result().unwrap();
    tokio::time::timeout(HANG, async {
        loop {
            let response = protocol_read(&mut stream).await;
            if let Body::OperationEnded(ended) = response.body.into_result().unwrap() {
                return ended;
            }
        }
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn a_created_workspace_is_prepared_in_an_operation_the_app_follows() {
    let daemon = TestDaemon::start().await;
    let app = daemon.connector();
    let create = OperationMethod::StartWorkspace {
        repository: daemon.repository.id.clone(),
        name: "api".into(),
        base: Some("HEAD".into()),
        issue: None,
        pr: None,
        agent: None,
    };
    let started: StartedWorkspace = connection::request(&app, create).await.unwrap();
    assert_eq!(started.workspace.name, "api");
    let worker = fake::take(&started.operation);
    let OperationSpec::Prepare { workspace, steps } = worker.order.spec.clone() else {
        panic!("expected a preparation: {:?}", worker.order);
    };
    assert_eq!(workspace, started.workspace.id);
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
async fn creation_refuses_a_bad_agent_or_issue_without_leaving_a_workspace() {
    let daemon = TestDaemon::start().await;
    let app = daemon.connector();
    let create = |issue: Option<&str>, agent: Option<&str>| OperationMethod::StartWorkspace {
        repository: daemon.repository.id.clone(),
        name: "api".into(),
        base: Some("HEAD".into()),
        issue: issue.map(Into::into),
        pr: None,
        agent: agent.map(|agent| QueuedLaunch {
            agent: agent.into(),
            prompt: None,
        }),
    };
    for refused in [
        create(None, Some("undefined")),
        create(Some("not an issue"), None),
    ] {
        assert!(
            connection::request::<StartedWorkspace>(&app, refused)
                .await
                .is_err()
        );
    }
    assert_eq!(code(inspect(&app, "api").await), ErrorCode::NotFound);
    // Workspace processes create only the workspaces they split.
    let parent = daemon.workspace("parent").await;
    let agent = daemon.scoped(&parent).await;
    assert_eq!(
        code(connection::request::<StartedWorkspace>(&agent, create(None, None)).await),
        ErrorCode::ScopeDenied
    );
    let split = OperationMethod::StartSplit {
        workspace: "parent".into(),
        name: "child".into(),
        base: Some("HEAD".into()),
        issue: None,
        agent: None,
    };
    let started: StartedWorkspace = connection::request(&agent, split).await.unwrap();
    let worker = fake::take(&started.operation);
    assert!(
        worker.token.is_some(),
        "the worker acts with the caller's scope"
    );
    worker.exit(1);
}

#[tokio::test]
async fn following_ends_once_the_followers_scope_token_is_revoked() {
    let daemon = TestDaemon::start().await;
    let own = daemon.workspace("own").await;
    let land = OperationMethod::StartOperation {
        spec: OperationSpec::Land {
            workspace: "own".into(),
            push: None,
        },
        env: None,
    };
    let started: crate::protocol::operations::Started =
        connection::request(&daemon.connector(), land)
            .await
            .unwrap();
    let mut worker = fake::take(&started.id);
    let token = daemon.scope_token(&own).await;
    let agent = daemon.connector().scope(token.clone());
    let method = OperationMethod::FollowOperation {
        id: started.id.clone(),
        after: 0,
    };
    let (mut stream, body) = connection::open_buffered(&agent, method).await.unwrap();
    body.into_result().unwrap();
    worker.stdout("one").await;
    let first = protocol_read(&mut stream).await;
    assert!(matches!(first.body, Body::OperationEvent(_)), "{first:?}");
    daemon
        .manager
        .revoke_workspace_environment(&own.id, token)
        .await
        .unwrap();
    worker.stdout("two").await;
    let ended = protocol_read(&mut stream).await;
    let error = ended.body.into_result().unwrap_err();
    assert_eq!(
        error.downcast_ref::<RemoteError>().unwrap().code,
        ErrorCode::ScopeDenied
    );
    worker.exit(0);
}
