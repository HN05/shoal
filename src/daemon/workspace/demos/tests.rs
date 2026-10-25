use std::collections::BTreeMap;

use super::*;
use crate::{
    daemon::{
        scope::{self, Caller, ScopedExecution},
        workspace::ExecutionKind,
    },
    model::Workspace,
    protocol::Method,
    test_support::{manager, repository},
};

fn launch(name: &str) -> Launch {
    Launch {
        id: "launch".into(),
        name: name.into(),
        preset: None,
        open: None,
        environment: BTreeMap::new(),
        host: "zmx".into(),
        place: format!("session-{name}"),
    }
}

async fn workspaces(manager: &Manager, root: &std::path::Path) -> (Workspace, Workspace) {
    let checkout = repository(root, "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
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
        .begin_execution_running(
            &first.id,
            None,
            ExecutionKind::Command,
            None,
            Some(launch("web")),
        )
        .await
        .unwrap();
    let tui = manager
        .begin_execution_running(
            &second.id,
            None,
            ExecutionKind::Command,
            None,
            Some(launch("tui")),
        )
        .await
        .unwrap();
    let all = manager.demos(None).await.unwrap();
    assert_eq!(
        all.iter()
            .map(|demo| (demo.workspace.as_str(), demo.launch.name.as_str()))
            .collect::<Vec<_>>(),
        [("first", "web"), ("second", "tui")]
    );
    assert_eq!(all[0].execution_id, web.plan.id);
    assert_eq!(all[0].launch, launch("web"));
    let own = manager.demos(Some(&second.name)).await.unwrap();
    assert_eq!(own.len(), 1);
    assert_eq!(own[0].execution_id, tui.plan.id);

    // A name belongs to one demo until its execution ends.
    let error = manager
        .begin_execution_running(
            &first.id,
            None,
            ExecutionKind::Command,
            None,
            Some(launch("web")),
        )
        .await
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("already running"),
        "{error:#}"
    );
    manager
        .finish_execution(web.plan.id.clone(), ExecutionKind::Command, Some(0))
        .await
        .unwrap();
    assert!(manager.demos(Some(&first.id)).await.unwrap().is_empty());
    let replacement = manager
        .begin_execution_running(
            &first.id,
            None,
            ExecutionKind::Command,
            None,
            Some(launch("web")),
        )
        .await
        .unwrap();
    let demos = manager.demos(Some(&first.id)).await.unwrap();
    assert_eq!(demos.len(), 1);
    assert_eq!(demos[0].execution_id, replacement.plan.id);
}

#[tokio::test]
async fn stopping_demos_asks_only_their_wrappers_to_stop() {
    let (temp, manager) = manager().await;
    let (first, _) = workspaces(&manager, temp.path()).await;
    let mut web = manager
        .begin_execution_running(
            &first.id,
            None,
            ExecutionKind::Command,
            None,
            Some(launch("web")),
        )
        .await
        .unwrap();
    let mut tui = manager
        .begin_execution_running(
            &first.id,
            None,
            ExecutionKind::Command,
            None,
            Some(launch("tui")),
        )
        .await
        .unwrap();
    let mut command = manager
        .begin_execution(&first.id, None, ExecutionKind::Command, None)
        .await
        .unwrap();

    manager
        .stop_demos(&first.name, Some("web".into()))
        .await
        .unwrap();
    assert!(*web.stop.borrow_and_update());
    assert!(!*tui.stop.borrow_and_update());
    let request = manager.stop_request(&web.plan.id).await.unwrap();
    assert_eq!(request.save, Save::Nothing);

    manager.stop_demos(&first.name, None).await.unwrap();
    assert!(*tui.stop.borrow_and_update());
    assert!(!*command.stop.borrow_and_update());
    assert!(
        manager
            .stop_demos(&first.name, Some("Bad".into()))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn workspace_processes_list_and_stop_only_their_own_demos() {
    let (temp, manager) = manager().await;
    let (first, second) = workspaces(&manager, temp.path()).await;
    manager
        .issue_scope(
            "scope".into(),
            Caller {
                workspace_id: first.id.clone(),
                execution: Some(ScopedExecution {
                    id: "execution".into(),
                    kind: ExecutionKind::Command,
                }),
            },
        )
        .await;
    let mut list = Method::ListDemos { workspace: None };
    scope::authorize(&manager, Some("scope"), &mut list)
        .await
        .unwrap();
    assert!(matches!(list, Method::ListDemos { workspace: Some(own) } if own == first.id));
    let mut stop = Method::StopDemos {
        workspace: first.name.clone(),
        name: None,
    };
    scope::authorize(&manager, Some("scope"), &mut stop)
        .await
        .unwrap();
    for mut method in [
        Method::ListDemos {
            workspace: Some(second.name.clone()),
        },
        Method::StopDemos {
            workspace: second.name.clone(),
            name: None,
        },
    ] {
        assert!(
            scope::authorize(&manager, Some("scope"), &mut method)
                .await
                .is_err()
        );
    }
}

#[tokio::test]
async fn a_demo_registers_its_next_run_once_the_exited_one_is_released() {
    let (temp, manager) = manager().await;
    let (first, _) = workspaces(&manager, temp.path()).await;
    let wrapper = crate::process::identity::capture(std::process::id()).unwrap();
    let started = manager
        .begin_execution_running(
            &first.id,
            wrapper,
            ExecutionKind::Command,
            None,
            Some(launch("tui")),
        )
        .await
        .unwrap();
    let id = started.plan.id.clone();
    let run = || {
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let identity = crate::process::identity::capture(child.id())
            .unwrap()
            .unwrap();
        child.kill().unwrap();
        child.wait().unwrap();
        identity
    };
    let exited = run();
    manager
        .record_execution_child(id.clone(), Some(exited.clone()), exited.pid)
        .await
        .unwrap();
    let next = run();
    assert!(
        manager
            .record_execution_child(id.clone(), Some(next.clone()), next.pid)
            .await
            .is_err(),
        "a run registers only after the previous one is released"
    );
    // The next run starts only in the worktree the workspace recorded.
    let moved = first.path.with_extension("moved");
    std::fs::rename(&first.path, &moved).unwrap();
    assert!(manager.release_exited_child(&id, &first).await.is_err());
    std::fs::rename(&moved, &first.path).unwrap();
    manager.release_exited_child(&id, &first).await.unwrap();
    manager
        .record_execution_child(id, Some(next.clone()), next.pid)
        .await
        .unwrap();
}

#[tokio::test]
async fn stopping_a_demo_whose_wrapper_is_away_stops_its_processes() {
    let (temp, manager) = manager().await;
    let (first, _) = workspaces(&manager, temp.path()).await;
    let ended = || {
        let mut process = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let identity = crate::process::identity::capture(process.id())
            .unwrap()
            .unwrap();
        process.kill().unwrap();
        process.wait().unwrap();
        identity
    };
    // The wrapper still runs, away from the daemon.
    let mut away = std::process::Command::new("sleep")
        .arg("600")
        .spawn()
        .unwrap();
    let wrapper = crate::process::identity::capture(away.id()).unwrap();
    let child = ended();
    let started = manager
        .begin_execution_running(
            &first.id,
            wrapper,
            ExecutionKind::Command,
            None,
            Some(launch("web")),
        )
        .await
        .unwrap();
    let id = started.plan.id.clone();
    manager
        .record_execution_child(id.clone(), Some(child.clone()), child.pid)
        .await
        .unwrap();
    // As after a daemon restart, before the wrapper reconnects.
    drop(started);
    manager.detach_execution(&id).await;
    manager.stop_demos(&first.name, None).await.unwrap();
    assert!(manager.demos(Some(&first.id)).await.unwrap().is_empty());
    assert!(away.wait().is_ok_and(|status| !status.success()));
}
