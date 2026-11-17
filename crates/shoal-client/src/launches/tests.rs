use crate::connection::{self};
use crate::test_daemon::{Connect as _, code, queued_plan};
use shoal_daemon::test_daemon::{TestDaemon, fake};
use shoal_protocol::launches::{Launch, Launched, SpaceChoice, StartTarget};
use shoal_protocol::model::Workspace;
use shoal_protocol::protocol::{ChatStart, ErrorCode, WorkspaceMethod, operations::OperationSpec};
use shoal_protocol::space_names::{SpaceName, SpaceSelector};

#[tokio::test]
async fn a_launch_in_a_workspace_queues_its_worker_on_the_launchs_task() {
    let daemon = TestDaemon::start().await;
    shoal_daemon::test_daemon::configure(
        &daemon.manager,
        "[agent]\ndefault = \"probe\"\n[agents.probe]\ncommand = [\"true\"]\n",
    )
    .await;
    let parent = daemon.workspace("parent").await;
    let app = daemon.connector();
    let launch = |workspace: &Workspace, agent: Option<&str>| Launch {
        in_workspace: Some(SpaceChoice::Space(SpaceSelector::id(&workspace.id))),
        agent: agent.map(Into::into),
        task: vec!["Fix the build".into()],
        ..Launch::default()
    };
    let worker = |launched| match launched {
        Launched::Worker(worker) => *worker,
        launched => panic!("expected a worker: {launched:?}"),
    };
    let send = |connector, launch| async move { crate::launches::launch(connector, launch).await };
    let queued = worker(send(&app, launch(&parent, Some("probe"))).await.unwrap());
    assert_eq!(queued.workspace.id, parent.id);
    let entry = queued.entry.unwrap();
    assert_eq!(entry.agent, "probe");
    let plan: shoal_core::start_plan::AddPlan =
        serde_json::from_value(queued_plan(&daemon, &entry).await).unwrap();
    assert_eq!(plan.args, ["Fix the build"]);
    // Without an agent of its own, it starts none, as a start without a
    // terminal to pick one does, whatever `agent.default` names.
    let none = worker(send(&app, launch(&parent, None)).await.unwrap());
    assert!(none.entry.is_none());

    // A workspace's agent starts the workers of the workspaces below its
    // own only.
    let split = WorkspaceMethod::SplitWorkspace {
        workspace: SpaceSelector::id(&parent.id),
        name: "child".into(),
        base: Some("HEAD".into()),
        options: Default::default(),
    };
    let child = connection::request::<Workspace>(&app, split).await.unwrap();
    let agent = daemon.scoped(&parent).await;
    let own = send(&agent, launch(&parent, Some("probe"))).await;
    assert_eq!(code(own), ErrorCode::ScopeDenied);
    let below = worker(send(&agent, launch(&child, Some("probe"))).await.unwrap());
    assert_eq!(below.entry.unwrap().agent, "probe");
}

/// A launch of a new workspace in the test daemon's repository on `branch`.
fn workspace_launch(daemon: &TestDaemon, branch: &str) -> Launch {
    Launch {
        target: Some(StartTarget::Repository(daemon.repository.id.to_string())),
        branch: Some(branch.into()),
        ..Launch::default()
    }
}

#[tokio::test]
async fn a_launch_opens_a_new_workspace_and_prepares_it() {
    let daemon = TestDaemon::start().await;
    shoal_daemon::test_daemon::configure(&daemon.manager, "[agents.probe]\ncommand = [\"true\"]\n")
        .await;
    let app = daemon.connector();
    let launch = Launch {
        agent: Some("probe".into()),
        ..workspace_launch(&daemon, "new")
    };
    let launched = crate::launches::launch(&app, launch).await.unwrap();
    let Launched::Workspace(started) = launched else {
        panic!("expected a workspace: {launched:?}");
    };
    assert_eq!(started.workspace.branch().map(|b| b.as_str()), Some("new"));
    let worker = fake::take(started.operation.as_deref().unwrap());
    // Its agent waits in the queue for the preparation, which queues none.
    let OperationSpec::Prepare { steps, .. } = worker.order.spec.clone() else {
        panic!("expected a preparation: {:?}", worker.order);
    };
    assert!(steps.agent.is_none(), "{steps:?}");
    let entry = started.entry.expect("the launch queued its agent");
    assert_eq!(entry.agent, "probe");
    assert_eq!(entry.blockers, ["setup of new".to_owned()]);
    worker.exit(1);
}

#[tokio::test]
async fn a_launch_that_starts_now_waits_for_none_of_its_blockers() {
    let daemon = TestDaemon::start().await;
    shoal_daemon::test_daemon::configure(&daemon.manager, "[agents.probe]\ncommand = [\"true\"]\n")
        .await;
    let blocker = daemon.workspace("blocker").await;
    let launch = Launch {
        agent: Some("probe".into()),
        blockers: vec![blocker.id.to_string()],
        now: true,
        ..workspace_launch(&daemon, "now")
    };
    let app = daemon.connector();
    let launched = crate::launches::launch(&app, launch).await.unwrap();
    let Launched::Workspace(started) = launched else {
        panic!("expected a workspace: {launched:?}");
    };
    fake::take(started.operation.as_deref().unwrap()).exit(1);
}

#[tokio::test]
async fn a_launch_refuses_what_it_cannot_start_before_it_creates_anything() {
    let daemon = TestDaemon::start().await;
    let app = daemon.connector();
    let refused = [
        (
            Launch {
                agent: Some("missing".into()),
                ..workspace_launch(&daemon, "unknown-agent")
            },
            "\"missing\"",
        ),
        (
            Launch {
                target: Some("https://other.example/team/repo/issues/7".parse().unwrap()),
                ..Launch::default()
            },
            "shoal repo add https://other.example/team/repo",
        ),
        (
            Launch {
                agent: Some("codex".into()),
                blockers: vec!["missing".into()],
                ..workspace_launch(&daemon, "blocked")
            },
            "neither a space nor a queued agent",
        ),
    ];
    for (launch, refusal) in refused {
        let error = crate::launches::launch(&app, launch).await.unwrap_err();
        assert!(format!("{error:#}").contains(refusal), "{error:#}");
    }
    assert!(daemon.manager.list_workspaces().await.unwrap().is_empty());
}

/// The resolved starts the daemon ran its chat start worker for, each with
/// whether the worker acts with the caller's scope.
fn launched_chats() -> Vec<(ChatStart, bool)> {
    shoal_daemon::test_daemon::STARTED_CHATS
        .lock()
        .unwrap()
        .clone()
}

#[tokio::test]
async fn an_app_starts_a_chat_whose_agent_and_mode_the_daemon_resolves() {
    let daemon = TestDaemon::start().await;
    let app = daemon.connector();
    let start = || Launch {
        target: Some(StartTarget::Repository(
            daemon.repository.path.to_str().unwrap().into(),
        )),
        chat: true,
        task: vec!["Read the build".into()],
        ..Default::default()
    };
    let error = crate::launches::launch(&app, start()).await.unwrap_err();
    assert!(format!("{error:#}").contains("agent.default"), "{error:#}");
    assert!(launched_chats().is_empty());
    shoal_daemon::test_daemon::configure(
        &daemon.manager,
        "[agent]\ndefault = \"probe\"\n[agents.probe]\ncommand = [\"true\"]\n",
    )
    .await;
    (crate::launches::launch(&app, start())).await.unwrap();
    let [(launched, scoped)] = launched_chats().try_into().unwrap();
    assert!(!scoped);
    assert_eq!(launched.agent, Some("probe".into()));
    assert_eq!(launched.mode, Some(shoal_protocol::model::chat_mode()));
    let repository = launched.repository.as_ref().map(|named| named.value());
    assert_eq!(repository, Some(daemon.repository.id.as_str()));
    assert_eq!(launched.parent, None);
    assert_eq!(launched.args, ["Read the build"]);
    // Without a name of its own, a chat in the checkout takes its
    // repository's and its mode's.
    let named = format!("{} chat", shoal_core::repository::name(&daemon.repository));
    assert_eq!(launched.name.as_ref().map(SpaceName::as_str), Some(&*named));
}

#[tokio::test]
async fn a_scoped_app_starts_chats_below_its_own_space() {
    let daemon = TestDaemon::start().await;
    shoal_daemon::test_daemon::configure(&daemon.manager, "[agents.probe]\ncommand = [\"true\"]\n")
        .await;
    let parent = daemon.workspace("parent").await;
    let other = daemon.workspace("other").await;
    let agent = daemon.scoped(&parent).await;
    let start = |parent: Option<SpaceSelector>| Launch {
        target: Some(StartTarget::Repository(daemon.repository.id.to_string())),
        chat: true,
        agent: Some("probe".into()),
        parent,
        ..Default::default()
    };
    let launch = |launch| crate::launches::launch(&agent, launch);
    let outside = start(Some(SpaceSelector::id(&other.id)));
    assert!(launch(outside).await.is_err());
    launch(start(None)).await.unwrap();
    // The worker records it with the caller's scope, below the caller.
    let [(launched, scoped)] = launched_chats().try_into().unwrap();
    assert!(scoped);
    assert_eq!(launched.parent, None);
}
