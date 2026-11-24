use std::sync::{Arc, Mutex, atomic::Ordering};

use serde_json::json;
use shoal_forge::test_support::Server;
use shoal_forge_types::backends::ForgeKind;
use shoal_protocol::alerts::Alert;

use super::*;
use crate::stand_in::{Fixture, runner_program};

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
    (server, changes)
}

/// The repository of `fixture` on `server`'s `kind` of forge, whose runner
/// program the runner finds.
fn on_forge(fixture: &Fixture, server: &Server, kind: ForgeKind, program: &str) {
    let url = format!("http://{}/team/repo", server.host);
    fixture.place_on_forge(&url, kind);
    let bin = runner_program(fixture.root.path(), program);
    *fixture.runner.jobs.path.lock().unwrap() = Some(bin);
}

fn settings(host: &str, enabled: bool) -> String {
    format!(
        "[machine.forges.\"{host}\"]\ntype = \"forgejo\"\nuser_token = \"user\"\n[runner]\nenabled = {enabled}\n"
    )
}

async fn reconcile(fixture: &Fixture) {
    let daemon = fixture.daemon();
    fixture.runner.reconcile_runners(&*daemon).await.unwrap();
}

async fn registration(fixture: &Fixture) -> Option<Registration> {
    let registration = fixture.runner.runner_registration(fixture.repository());
    registration.await.unwrap()
}

async fn runner_notifications(fixture: &Fixture) -> Vec<Alert> {
    let notifications = fixture.runner.notifications.list(false, 50).await.unwrap();
    notifications
        .into_iter()
        .filter(|notification| notification.kind == AlertKind::RunnerFailed)
        .collect()
}

#[tokio::test]
async fn the_runner_follows_the_setting_and_goes_with_its_repository() {
    let fixture = Fixture::new("").await;
    let (server, changes) = forgejo();
    on_forge(&fixture, &server, ForgeKind::Forgejo, "forgejo-runner");

    fixture.configure(&settings(&server.host, true));
    reconcile(&fixture).await;
    let runner = registration(&fixture).await.unwrap().runner.unwrap();
    assert_eq!((runner.uuid.as_str(), runner.token.as_str()), ("u", "t"));
    let registrations = (server.received().into_iter())
        .filter(|request| request.line.starts_with("POST"))
        .collect::<Vec<_>>();
    assert_eq!(registrations.len(), 1);
    assert_eq!(registrations[0].header("Authorization"), Some("token user"));
    // A registered runner stays as it is.
    reconcile(&fixture).await;

    fixture.configure(&settings(&server.host, false));
    reconcile(&fixture).await;
    assert_eq!(registration(&fixture).await, None);
    // Its waiting jobs leave the queue for other runners.
    let forgotten = fixture.daemon.forgotten.lock().unwrap().clone();
    assert_eq!(forgotten.last(), Some(fixture.repository()));

    fixture.configure(&settings(&server.host, true));
    reconcile(&fixture).await;
    let daemon = fixture.daemon();
    let repository = daemon.repository(fixture.repository()).await.unwrap();
    (fixture.runner.delete_runner_of_removed(&repository))
        .await
        .unwrap();
    assert_eq!(
        *changes.lock().unwrap(),
        [
            "POST /api/v1/repos/team/repo/actions/runners",
            "DELETE /api/v1/repos/team/repo/actions/runners/5",
            "POST /api/v1/repos/team/repo/actions/runners",
            "DELETE /api/v1/repos/team/repo/actions/runners/5",
        ]
    );
    assert!(runner_notifications(&fixture).await.is_empty());
}

#[tokio::test]
async fn a_repository_being_removed_registers_no_runner() {
    let fixture = Fixture::new("").await;
    let (server, changes) = forgejo();
    on_forge(&fixture, &server, ForgeKind::Forgejo, "forgejo-runner");
    fixture.configure(&settings(&server.host, true));
    fixture.daemon.removing.store(true, Ordering::Relaxed);

    reconcile(&fixture).await;

    assert_eq!(registration(&fixture).await, None);
    assert!(changes.lock().unwrap().is_empty());
}

#[tokio::test]
async fn registration_fetches_a_token_from_a_command_once() {
    let fixture = Fixture::new("").await;
    let (server, changes) = forgejo();
    on_forge(&fixture, &server, ForgeKind::Forgejo, "forgejo-runner");
    let fetches = fixture.root.path().join("fetches");
    let command = format!("echo >> '{}'; echo user", fetches.display());
    fixture.configure(&format!(
        "[machine.forges.\"{}\"]\ntype = \"forgejo\"\nuser_token = {{ cmd = [\"sh\", \"-c\", {command:?}] }}\n[runner]\nenabled = true\n",
        server.host
    ));

    reconcile(&fixture).await;

    assert!(runner_notifications(&fixture).await.is_empty());
    assert_eq!(changes.lock().unwrap().len(), 1);
    assert_eq!(
        std::fs::read_to_string(&fetches).unwrap().lines().count(),
        1
    );
}

#[tokio::test]
async fn a_missing_runner_program_registers_nothing_and_tells_the_user_once() {
    let fixture = Fixture::new("").await;
    let (server, changes) = forgejo();
    on_forge(&fixture, &server, ForgeKind::Forgejo, "forgejo-runner");
    let empty = fixture.root.path().join("empty").into_os_string();
    *fixture.runner.jobs.path.lock().unwrap() = Some(empty);
    fixture.configure(&settings(&server.host, true));

    reconcile(&fixture).await;
    // A failed repository waits before it is tried again.
    reconcile(&fixture).await;

    let notifications = runner_notifications(&fixture).await;
    assert_eq!(notifications.len(), 1, "{notifications:?}");
    assert!(changes.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_github_repository_keeps_no_runner_of_its_own() {
    let fixture = Fixture::new("").await;
    let server = Server::start(|request| match request.line.as_str() {
        "GET /api/v3/repos/team/repo/actions/runners?per_page=1" => {
            let body = json!({"total_count": 0, "runners": []});
            (200, Vec::new(), body.to_string().into())
        }
        _ => (404, Vec::new(), b"{}".to_vec()),
    });
    on_forge(&fixture, &server, ForgeKind::GitHub, "Runner.Listener");
    let github = |enabled: bool| {
        format!(
            "[machine.forges.\"{}\"]\ntype = \"github\"\nuser_token = \"user\"\n[runner]\nenabled = {enabled}\n",
            server.host
        )
    };

    fixture.configure(&github(true));
    reconcile(&fixture).await;
    let notifications = runner_notifications(&fixture).await;
    assert!(notifications.is_empty(), "{notifications:?}");
    assert_eq!(
        registration(&fixture).await,
        Some(Registration {
            forge_repository: format!("http://{}/team/repo", server.host),
            runner: None,
        })
    );

    fixture.configure(&github(false));
    reconcile(&fixture).await;
    assert_eq!(registration(&fixture).await, None);
    let lines: Vec<_> = (server.received().into_iter())
        .map(|request| request.line)
        .filter(|line| !line.starts_with("GET /api/v3/meta"))
        .collect();
    assert_eq!(
        lines,
        ["GET /api/v3/repos/team/repo/actions/runners?per_page=1"]
    );
}
