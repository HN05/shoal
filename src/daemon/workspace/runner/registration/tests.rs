use std::{
    path::Path,
    sync::{Arc, Mutex},
};

use serde_json::json;

use super::*;
use crate::{
    daemon::notifications::Notification,
    forge::{backends::ForgeKind, test_support::Server},
    test_support::{configure, git, manager, repository},
};

/// A Forgejo that registers runner 5 as `u` and deletes it, and the
/// requests it saw that changed something.
fn forgejo() -> (Server, Arc<Mutex<Vec<String>>>) {
    let changes = Arc::new(Mutex::new(Vec::new()));
    let seen = changes.clone();
    let server = Server::start(move |request| {
        let body = match request.line.as_str() {
            "GET /api/v1/version" => json!({"version": "17.0.1+gitea-1.22.0"}),
            "POST /api/v1/repos/team/repo/actions/runners" => {
                seen.lock().unwrap().push(request.line.clone());
                json!({"id": 5, "uuid": "u", "token": "t"})
            }
            line if line.starts_with("DELETE ") => {
                seen.lock().unwrap().push(request.line.clone());
                return (204, Vec::new(), Vec::new());
            }
            _ => return (404, Vec::new(), b"{}".to_vec()),
        };
        (200, Vec::new(), body.to_string().into())
    });
    // Pins are this process's, which parallel tests replace; an answer stays.
    crate::forge::hosts::learn(&server.host, ForgeKind::Forgejo);
    (server, changes)
}

fn runner_program(root: &Path) -> std::ffi::OsString {
    crate::daemon::workspace::runner::tests::runner_program(root, "forgejo-runner")
}

fn settings(host: &str, enabled: bool) -> String {
    format!(
        "[cleanup]\ngrace_minutes = 0\n[machine.forges.\"{host}\"]\ntype = \"forgejo\"\ntoken = \"user\"\n[runner]\nenabled = {enabled}\n"
    )
}

async fn runner_notifications(manager: &Manager) -> Vec<Notification> {
    let notifications = manager.notifications(false, 50).await.unwrap();
    notifications
        .into_iter()
        .filter(|notification| notification.kind == NotificationKind::RunnerFailed)
        .collect()
}

#[tokio::test]
async fn the_runner_follows_the_setting_and_goes_with_its_repository() {
    let (root, manager) = manager().await;
    let path = repository(root.path(), "repo");
    let repo = manager
        .register_repository(path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let (server, changes) = forgejo();
    git(
        &path,
        &[
            "remote",
            "add",
            "origin",
            &format!("http://{}/team/repo", server.host),
        ],
    );
    *manager.runners.path.lock().unwrap() = Some(runner_program(root.path()));

    configure(&manager, &settings(&server.host, true)).await;
    manager.reconcile_runners().await.unwrap();
    let registration = manager
        .runner_registration(&repo.id)
        .await
        .unwrap()
        .unwrap();
    let runner = registration.runner.unwrap();
    assert_eq!((runner.uuid.as_str(), runner.token.as_str()), ("u", "t"));
    let registrations = server
        .received()
        .into_iter()
        .filter(|request| request.line.starts_with("POST"))
        .collect::<Vec<_>>();
    assert_eq!(registrations.len(), 1);
    assert_eq!(registrations[0].header("Authorization"), Some("token user"));
    // A registered runner stays as it is.
    manager.reconcile_runners().await.unwrap();

    configure(&manager, &settings(&server.host, false)).await;
    manager.reconcile_runners().await.unwrap();
    assert_eq!(manager.runner_registration(&repo.id).await.unwrap(), None);

    configure(&manager, &settings(&server.host, true)).await;
    manager.reconcile_runners().await.unwrap();
    manager.remove_repository(repo.id.as_str()).await.unwrap();
    assert_eq!(
        *changes.lock().unwrap(),
        [
            "POST /api/v1/repos/team/repo/actions/runners",
            "DELETE /api/v1/repos/team/repo/actions/runners/5",
            "POST /api/v1/repos/team/repo/actions/runners",
            "DELETE /api/v1/repos/team/repo/actions/runners/5",
        ]
    );
    assert!(runner_notifications(&manager).await.is_empty());
}

#[tokio::test]
async fn registration_fetches_a_token_from_a_command_once() {
    let (root, manager) = manager().await;
    let path = repository(root.path(), "repo");
    manager
        .register_repository(path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let (server, changes) = forgejo();
    git(
        &path,
        &[
            "remote",
            "add",
            "origin",
            &format!("http://{}/team/repo", server.host),
        ],
    );
    *manager.runners.path.lock().unwrap() = Some(runner_program(root.path()));
    let fetches = root.path().join("fetches");
    let command = format!("echo >> '{}'; echo user", fetches.display());
    configure(
        &manager,
        &format!(
            "[machine.forges.\"{}\"]\ntype = \"forgejo\"\ntoken = {{ cmd = [\"sh\", \"-c\", {command:?}] }}\n[runner]\nenabled = true\n",
            server.host
        ),
    )
    .await;
    manager.reconcile_runners().await.unwrap();
    assert!(runner_notifications(&manager).await.is_empty());
    assert_eq!(changes.lock().unwrap().len(), 1);
    assert_eq!(
        std::fs::read_to_string(&fetches).unwrap().lines().count(),
        1
    );
}

#[tokio::test]
async fn a_missing_runner_program_registers_nothing_and_tells_the_user_once() {
    let (root, manager) = manager().await;
    let path = repository(root.path(), "repo");
    manager
        .register_repository(path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let (server, changes) = forgejo();
    git(
        &path,
        &[
            "remote",
            "add",
            "origin",
            &format!("http://{}/team/repo", server.host),
        ],
    );
    *manager.runners.path.lock().unwrap() = Some(root.path().join("empty").into_os_string());
    configure(&manager, &settings(&server.host, true)).await;

    manager.reconcile_runners().await.unwrap();
    // A failed repository waits before it is tried again.
    manager.reconcile_runners().await.unwrap();
    let notifications = runner_notifications(&manager).await;
    assert_eq!(notifications.len(), 1, "{notifications:?}");
    assert!(
        notifications[0]
            .message
            .contains("forgejo-runner is not on the daemon's PATH"),
        "{}",
        notifications[0].message
    );
    assert!(changes.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_github_repository_keeps_no_runner_of_its_own() {
    let (root, manager) = manager().await;
    let path = repository(root.path(), "repo");
    let repo = manager
        .register_repository(path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let server = Server::start(|request| match request.line.as_str() {
        "GET /api/v3/repos/team/repo/actions/runners?per_page=1" => {
            let body = json!({"total_count": 0, "runners": []});
            (200, Vec::new(), body.to_string().into())
        }
        _ => (404, Vec::new(), b"{}".to_vec()),
    });
    crate::forge::hosts::learn(&server.host, ForgeKind::GitHub);
    let origin = format!("http://{}/team/repo", server.host);
    git(&path, &["remote", "add", "origin", &origin]);
    let bin = crate::daemon::workspace::runner::tests::runner_program(
        &root.path().join("actions-runner"),
        "Runner.Listener",
    );
    *manager.runners.path.lock().unwrap() = Some(bin);
    let github = |enabled: bool| {
        format!(
            "[machine.forges.\"{}\"]\ntype = \"github\"\ntoken = \"user\"\n[runner]\nenabled = {enabled}\n",
            server.host
        )
    };

    configure(&manager, &github(true)).await;
    manager.reconcile_runners().await.unwrap();
    assert!(runner_notifications(&manager).await.is_empty());
    let registration = manager.runner_registration(&repo.id).await.unwrap();
    assert_eq!(
        registration,
        Some(Registration {
            forge_repository: origin,
            runner: None,
        })
    );

    configure(&manager, &github(false)).await;
    manager.reconcile_runners().await.unwrap();
    assert_eq!(manager.runner_registration(&repo.id).await.unwrap(), None);
    let lines: Vec<_> = server
        .received()
        .into_iter()
        .map(|request| request.line)
        .filter(|line| !line.starts_with("GET /api/v3/meta"))
        .collect();
    assert_eq!(
        lines,
        ["GET /api/v3/repos/team/repo/actions/runners?per_page=1"]
    );
}
