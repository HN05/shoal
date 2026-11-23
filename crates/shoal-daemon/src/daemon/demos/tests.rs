use std::collections::BTreeMap;

use crate::{
    daemon::{
        events::{EventItem, EventKind},
        executions::{ExecutionKind, Save},
        manager::Manager,
        scope::{self, Caller, ScopedExecution},
        test_support::{manager, repository},
        workspace_files::Attacher,
    },
    demos::Launch,
    model::Workspace,
    protocol::DemoMethod,
};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::assignments::Done;
use shoal_protocol::space_names::SpaceSelector;

fn launch(name: &str) -> Launch {
    Launch {
        id: "launch".into(),
        name: name.into(),
        preset: None,
        open: None,
        scheme: None,
        viewer_command: None,
        environment: BTreeMap::new(),
        origin: crate::sessions::Origin::created(
            crate::sessions::HostName::Zmx,
            format!("session-{name}"),
        ),
    }
}

async fn workspaces(manager: &Manager, root: &std::path::Path) -> (Workspace, Workspace) {
    let checkout = repository(root, "repo");
    let repo = manager
        .repositories
        .register(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let first = manager
        .create_workspace(&repo.id, "first".into(), None, None, None)
        .await
        .unwrap();
    let second = manager
        .create_workspace(&repo.id, "second".into(), None, None, None)
        .await
        .unwrap();
    (first, second)
}

#[tokio::test]
async fn demos_are_recorded_with_their_executions_and_end_with_them() {
    let (temp, manager) = manager().await;
    let (first, second) = workspaces(&manager, temp.path()).await;
    let web = manager
        .agents
        .begin_execution_running(
            &*manager,
            first.id.as_str(),
            None,
            ExecutionKind::Command,
            None,
            Some(launch("web")),
        )
        .await
        .unwrap();
    let tui = manager
        .agents
        .begin_execution_running(
            &*manager,
            second.id.as_str(),
            None,
            ExecutionKind::Command,
            None,
            Some(launch("tui")),
        )
        .await
        .unwrap();
    let all = manager.demos.list(&*manager, None).await.unwrap();
    assert_eq!(
        all.iter()
            .map(|demo| (demo.workspace.as_str(), demo.launch.name.as_str()))
            .collect::<Vec<_>>(),
        [("first", "web"), ("second", "tui")]
    );
    assert_eq!(all[0].execution_id, web.plan.id);
    assert_eq!(all[0].launch, launch("web"));
    let own = manager
        .demos
        .list(&*manager, Some(&SpaceSelector::id(&second.id)))
        .await
        .unwrap();
    assert_eq!(own.len(), 1);
    assert_eq!(own[0].execution_id, tui.plan.id);

    // A name belongs to one demo until its execution ends.
    manager
        .agents
        .begin_execution_running(
            &*manager,
            first.id.as_str(),
            None,
            ExecutionKind::Command,
            None,
            Some(launch("web")),
        )
        .await
        .unwrap_err();
    manager
        .agents
        .finish_execution(
            &*manager,
            web.plan.id.clone(),
            ExecutionKind::Command,
            Some(0),
        )
        .await
        .unwrap();
    assert!(
        manager
            .demos
            .list(&*manager, Some(&SpaceSelector::id(&first.id)))
            .await
            .unwrap()
            .is_empty()
    );
    let replacement = manager
        .agents
        .begin_execution_running(
            &*manager,
            first.id.as_str(),
            None,
            ExecutionKind::Command,
            None,
            Some(launch("web")),
        )
        .await
        .unwrap();
    let demos = manager
        .demos
        .list(&*manager, Some(&SpaceSelector::id(&first.id)))
        .await
        .unwrap();
    assert_eq!(demos.len(), 1);
    assert_eq!(demos[0].execution_id, replacement.plan.id);
}

#[tokio::test]
async fn stopping_demos_asks_only_their_wrappers_to_stop() {
    let (temp, manager) = manager().await;
    let (first, _) = workspaces(&manager, temp.path()).await;
    let mut web = manager
        .agents
        .begin_execution_running(
            &*manager,
            first.id.as_str(),
            None,
            ExecutionKind::Command,
            None,
            Some(launch("web")),
        )
        .await
        .unwrap();
    let mut tui = manager
        .agents
        .begin_execution_running(
            &*manager,
            first.id.as_str(),
            None,
            ExecutionKind::Command,
            None,
            Some(launch("tui")),
        )
        .await
        .unwrap();
    let mut command = manager
        .agents
        .begin_execution(
            &*manager,
            first.id.as_str(),
            None,
            ExecutionKind::Command,
            None,
        )
        .await
        .unwrap();

    manager
        .demos
        .stop(&*manager, &SpaceSelector::id(&first.id), Some("web".into()))
        .await
        .unwrap();
    assert!(*web.stop.borrow_and_update());
    assert!(!*tui.stop.borrow_and_update());
    let request = manager
        .agents
        .executions()
        .stop_request(&web.plan.id)
        .await
        .unwrap();
    assert_eq!(request.save, Save::Nothing);

    manager
        .demos
        .stop(&*manager, &SpaceSelector::id(&first.id), None)
        .await
        .unwrap();
    assert!(*tui.stop.borrow_and_update());
    assert!(!*command.stop.borrow_and_update());
    assert!(
        manager
            .demos
            .stop(&*manager, &SpaceSelector::id(&first.id), Some("Bad".into()))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn completion_stops_the_workspaces_demos_unless_it_keeps_them() {
    let (temp, manager) = manager().await;
    let (first, _) = workspaces(&manager, temp.path()).await;
    let mut web = manager
        .agents
        .begin_execution_running(
            &*manager,
            first.id.as_str(),
            None,
            ExecutionKind::Command,
            None,
            Some(launch("web")),
        )
        .await
        .unwrap();
    let selector = SpaceSelector::id(&first.id);
    let kept = Done {
        keep_demos: true,
        ..Done::default()
    };
    manager
        .spaces
        .mark_space_done(&*manager, &selector, kept, &Attacher::User)
        .await
        .unwrap();
    assert!(!*web.stop.borrow_and_update());

    manager
        .spaces
        .undo_space_done(&*manager, &selector)
        .await
        .unwrap();
    (manager
        .spaces
        .mark_space_done(&*manager, &selector, Done::default(), &Attacher::User))
    .await
    .unwrap();
    assert!(*web.stop.borrow_and_update());
}

#[tokio::test]
async fn workspace_processes_list_and_stop_only_their_own_demos() {
    let (temp, manager) = manager().await;
    let (first, second) = workspaces(&manager, temp.path()).await;
    manager
        .agents
        .executions()
        .issue_scope(
            "scope".into(),
            Caller {
                space_id: first.id.clone(),
                execution: Some(ScopedExecution {
                    id: "execution".into(),
                    kind: ExecutionKind::Command,
                    agent_execution: None,
                }),
                granted: None,
                read_only: false,
                vouched: None,
            },
        )
        .await;
    let mut list = DemoMethod::ListDemos { workspace: None };
    scope::authorize(&manager, Some(&"scope".into()), &mut list)
        .await
        .unwrap();
    assert!(
        matches!(list, DemoMethod::ListDemos { workspace: Some(own) } if own == SpaceSelector::id(&first.id))
    );
    let mut stop = DemoMethod::StopDemos {
        workspace: SpaceSelector::id(&first.id),
        name: None,
    };
    scope::authorize(&manager, Some(&"scope".into()), &mut stop)
        .await
        .unwrap();
    for mut method in [
        DemoMethod::ListDemos {
            workspace: Some(SpaceSelector::id(&second.id)),
        },
        DemoMethod::StopDemos {
            workspace: SpaceSelector::id(&second.id),
            name: None,
        },
    ] {
        assert!(
            scope::authorize(&manager, Some(&"scope".into()), &mut method)
                .await
                .is_err()
        );
    }
}

#[tokio::test]
async fn a_chat_lists_no_demos_of_its_own() {
    let (temp, manager) = manager().await;
    let (first, _) = workspaces(&manager, temp.path()).await;
    let chat = manager
        .start_chat(
            first.repository().clone().into(),
            "planning".into(),
            "claude".into(),
            None,
        )
        .await
        .unwrap();
    manager
        .agents
        .begin_execution_running(
            &*manager,
            first.id.as_str(),
            None,
            ExecutionKind::Command,
            None,
            Some(launch("web")),
        )
        .await
        .unwrap();
    assert!(
        manager
            .demos
            .list(&*manager, Some(&SpaceSelector::id(&chat.id)))
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(manager.demos.list(&*manager, None).await.unwrap().len(), 1);
}

#[tokio::test]
async fn a_demo_registers_its_next_run_once_the_exited_one_is_released() {
    let (temp, manager) = manager().await;
    let (first, _) = workspaces(&manager, temp.path()).await;
    let wrapper = crate::process::identity::capture(std::process::id()).unwrap();
    let started = manager
        .agents
        .begin_execution_running(
            &*manager,
            first.id.as_str(),
            wrapper,
            ExecutionKind::Command,
            None,
            Some(launch("tui")),
        )
        .await
        .unwrap();
    let id = started.plan.id.clone();
    let run = || {
        let child = crate::daemon::test_support::ProcessGroup::spawn(
            std::process::Command::new("sleep").arg("30"),
        );
        let identity = crate::process::identity::capture(child.id())
            .unwrap()
            .unwrap();
        child.kill();
        identity
    };
    let exited = run();
    manager
        .agents
        .record_execution_child(id.clone(), Some(exited.clone()), exited.pid)
        .await
        .unwrap();
    let next = run();
    assert!(
        manager
            .agents
            .record_execution_child(id.clone(), Some(next.clone()), next.pid)
            .await
            .is_err(),
        "a run registers only after the previous one is released"
    );
    // The next run starts only in the worktree the workspace recorded.
    let moved = first.path.with_extension("moved");
    std::fs::rename(&first.path, &moved).unwrap();
    assert!(
        manager
            .agents
            .release_exited_child(&*manager, &id, &first.id)
            .await
            .is_err()
    );
    std::fs::rename(&moved, &first.path).unwrap();
    manager
        .agents
        .release_exited_child(&*manager, &id, &first.id)
        .await
        .unwrap();
    manager
        .agents
        .record_execution_child(id, Some(next.clone()), next.pid)
        .await
        .unwrap();
}

#[tokio::test]
async fn stopping_a_demo_whose_wrapper_is_away_stops_its_processes() {
    let (temp, manager) = manager().await;
    let (first, _) = workspaces(&manager, temp.path()).await;
    let ended = || {
        let process = crate::daemon::test_support::ProcessGroup::spawn(
            std::process::Command::new("sleep").arg("30"),
        );
        let identity = crate::process::identity::capture(process.id())
            .unwrap()
            .unwrap();
        process.kill();
        identity
    };
    // The wrapper still runs, away from the daemon.
    let mut away = crate::daemon::test_support::ProcessGroup::spawn(
        std::process::Command::new("sleep").arg("600"),
    );
    let wrapper = crate::process::identity::capture(away.id()).unwrap();
    let child = ended();
    let started = manager
        .agents
        .begin_execution_running(
            &*manager,
            first.id.as_str(),
            wrapper,
            ExecutionKind::Command,
            None,
            Some(launch("web")),
        )
        .await
        .unwrap();
    let id = started.plan.id.clone();
    manager
        .agents
        .record_execution_child(id.clone(), Some(child.clone()), child.pid)
        .await
        .unwrap();
    // As after a daemon restart, before the wrapper reconnects.
    drop(started);
    manager.agents.detach_execution(&id).await;
    manager
        .demos
        .stop(&*manager, &SpaceSelector::id(&first.id), None)
        .await
        .unwrap();
    assert!(
        manager
            .demos
            .list(&*manager, Some(&SpaceSelector::id(&first.id)))
            .await
            .unwrap()
            .is_empty()
    );
    assert!(away.wait().is_ok_and(|status| !status.success()));
}

#[tokio::test]
async fn stopping_an_away_demo_between_runs_ends_its_record() {
    let (temp, manager) = manager().await;
    let (first, _) = workspaces(&manager, temp.path()).await;
    let mut away = crate::daemon::test_support::ProcessGroup::spawn(
        std::process::Command::new("sleep").arg("600"),
    );
    let wrapper = crate::process::identity::capture(away.id()).unwrap();
    let started = manager
        .agents
        .begin_execution_running(
            &*manager,
            first.id.as_str(),
            wrapper,
            ExecutionKind::Command,
            None,
            Some(launch("web")),
        )
        .await
        .unwrap();
    let id = started.plan.id.clone();
    // The last run was released, so the record names no child.
    drop(started);
    manager.agents.detach_execution(&id).await;
    manager
        .demos
        .stop(&*manager, &SpaceSelector::id(&first.id), None)
        .await
        .unwrap();
    assert!(
        manager
            .demos
            .list(&*manager, Some(&SpaceSelector::id(&first.id)))
            .await
            .unwrap()
            .is_empty()
    );
    assert!(away.wait().is_ok_and(|status| !status.success()));
}

/// The kinds and demos of the workspace's demo events, oldest first.
async fn demo_events(
    manager: &Manager,
    workspace: &SpaceId,
) -> Vec<(EventKind, String, Option<String>)> {
    manager
        .space_events(None, 1000)
        .await
        .unwrap()
        .into_iter()
        .filter_map(|item| match item {
            EventItem::Event(event) if event.details.space_id == *workspace => {
                let demo = event.details.demo?;
                Some((event.details.kind, demo.name, demo.udid))
            }
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn demos_journal_their_first_run_and_their_end() {
    let (temp, manager) = manager().await;
    let (first, _) = workspaces(&manager, temp.path()).await;
    let mut app = launch("app");
    app.preset = Some(crate::demos::Preset::Simulator);
    app.environment = BTreeMap::from([(crate::demos::SIM_UDID.into(), "UDID-1".into())]);
    let started = manager
        .agents
        .begin_execution_running(
            &*manager,
            first.id.as_str(),
            None,
            ExecutionKind::Command,
            None,
            Some(app),
        )
        .await
        .unwrap();
    let mut unrun = launch("unrun");
    unrun.id = "unrun".into();
    let never = manager
        .agents
        .begin_execution_running(
            &*manager,
            first.id.as_str(),
            None,
            ExecutionKind::Command,
            None,
            Some(unrun),
        )
        .await
        .unwrap();
    assert!(demo_events(&manager, &first.id).await.is_empty());

    let id = started.plan.id.clone();
    manager
        .agents
        .record_execution_child(id.clone(), None, 4_000_000)
        .await
        .unwrap();
    for execution in [id, never.plan.id] {
        manager
            .agents
            .forget_execution(&*manager, &execution)
            .await
            .unwrap();
    }
    let started = (
        EventKind::DemoStarted,
        "app".to_owned(),
        Some("UDID-1".to_owned()),
    );
    let stopped = (
        EventKind::DemoStopped,
        "app".to_owned(),
        Some("UDID-1".to_owned()),
    );
    assert_eq!(demo_events(&manager, &first.id).await, [started, stopped]);
}

mod viewers;
