use crate::client::execution_ids::ExecutionId;
use crate::client::repository_ids::RepositoryId;
use crate::client::space_ids::SpaceId;
use std::{
    collections::HashMap,
    ffi::OsString,
    os::unix::fs::PermissionsExt as _,
    os::unix::process::CommandExt as _,
    sync::{Arc, LazyLock, Mutex},
};

use serde_json::{Value, json};

use crate::client::space_names::SpaceSelector;
use crate::{
    client::resources::ResourceRequest,
    daemon::{
        notifications::NotificationKind,
        test_support::{configure, git, manager, repository},
        workspace::{Ending, ExecutionKind, Manager, StartedExecution},
    },
    forge::{backends::ForgeKind, ci::runner::JobCommand, test_support::Server},
    model::{QueueEntry, Workspace},
    state::{QueueKind, QueueState},
};

/// A test launch's command and the execution standing in for it.
type Launched = (JobCommand, StartedExecution);

/// Each test launch, by workspace.
static LAUNCHED: LazyLock<Mutex<HashMap<SpaceId, Launched>>> = LazyLock::new(Default::default);

/// Record `command` as run in `workspace` by an execution the test ends.
pub(super) async fn launch(
    manager: &Manager,
    workspace: &Workspace,
    command: JobCommand,
) -> anyhow::Result<ExecutionId> {
    let started = manager
        .begin_execution(workspace.id.as_str(), None, ExecutionKind::Command, None)
        .await?;
    let id = started.plan.id.clone();
    LAUNCHED
        .lock()
        .unwrap()
        .insert(workspace.id.clone(), (command, started));
    Ok(id)
}

/// A repository whose origin is a Forgejo that lists `jobs` as waiting:
/// run 1 pushed `main` at its head and run 2 is for PR 12, whose head is
/// `pull_head`.
struct Fixture {
    _root: tempfile::TempDir,
    manager: Arc<Manager>,
    repository: RepositoryId,
    head: String,
    server: Server,
}

async fn fixture(jobs: Vec<Value>, pull_head: &'static str, settings: &str) -> Fixture {
    let (root, manager) = manager().await;
    let path = repository(root.path(), "repo");
    let repo = manager
        .register_repository(path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let head = git(&path, &["rev-parse", "HEAD"]).trim().to_owned();
    let pushed = head.clone();
    let server = Server::start(move |request| {
        let line = request.line.as_str();
        let body = if line == "GET /api/v1/version" {
            json!({"version": "17.0.1+gitea-1.22.0"})
        } else if line.starts_with("GET /api/v1/repos/team/repo/actions/runners/jobs?") {
            json!(jobs)
        } else if line == "GET /api/v1/repos/team/repo/actions/runs/1" {
            run(1, "push", &pushed, json!({"ref": "refs/heads/main"}))
        } else if line == "GET /api/v1/repos/team/repo/actions/runs/2" {
            run(
                2,
                "pull_request",
                "old",
                json!({"pull_request": {"number": 12}}),
            )
        } else if line == "GET /api/v1/repos/team/repo/actions/runs/3" {
            run(3, "push", &pushed, json!({"ref": "refs/heads/topic"}))
        } else if line == "GET /api/v1/repos/team/repo/actions/runs?run_number=3" {
            let mut running = run(3, "push", &pushed, json!({}));
            running["status"] = json!("running");
            json!({"workflow_runs": [running]})
        } else if line == "POST /api/v1/repos/team/repo/actions/runs/3/cancel" {
            return (204, Vec::new(), Vec::new());
        } else if line == "GET /api/v1/repos/team/repo/pulls/12" {
            json!({"number": 12, "title": "Fix", "state": "open", "head": {"sha": pull_head, "ref": "fix"}, "base": {"ref": "main"}})
        } else {
            return (404, Vec::new(), b"{}".to_vec());
        };
        (200, Vec::new(), body.to_string().into())
    });
    crate::forge::hosts::learn(&server.host, ForgeKind::Forgejo);
    *manager.runners.path.lock().unwrap() = Some(runner_program(root.path(), "forgejo-runner"));
    let origin = format!("http://{}/team/repo", server.host);
    git(&path, &["remote", "add", "origin", &origin]);
    // The cached remote HEAD names the default branch without a fetch.
    git(&path, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
    git(
        &path,
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/main",
        ],
    );
    configure(
        &manager,
        &format!(
            "[cleanup]\ngrace_minutes = 0\n[machine]\nmax_jobs = 2\n[machine.forges.\"{}\"]\ntype = \"forgejo\"\ntoken = \"user\"\n[runner]\nenabled = true\n{settings}",
            server.host
        ),
    )
    .await;
    let id = repo.id.clone();
    manager
        .store
        .run(move |db| {
            db.execute(
                "INSERT INTO runners(repository_id,forge_repository,forge_id,uuid,token)
                 VALUES (?1,?2,5,'runner-uuid','runner-token')",
                rusqlite::params![id, origin],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    Fixture {
        _root: root,
        manager,
        repository: repo.id,
        head,
        server,
    }
}

/// A directory holding an executable `program`.
pub(super) fn runner_program(root: &std::path::Path, program: &str) -> OsString {
    let bin = root.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let path = bin.join(program);
    std::fs::write(&path, "#!/bin/sh\n").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    bin.into_os_string()
}

fn run(id: u64, event: &str, commit: &str, payload: Value) -> Value {
    json!({
        "id": id, "index_in_repo": id, "event": event, "commit_sha": commit,
        "workflow_id": "ci.yml", "event_payload": payload.to_string(),
    })
}

fn job(handle: &str, id: u64, run: u64) -> Value {
    json!({"handle": handle, "id": id, "run_id": run, "name": "rust", "runs_on": ["shoal"], "status": "waiting"})
}

/// What a GitHub fixture's server answers and saw.
#[derive(Default)]
pub(super) struct GitHubState {
    /// The queued jobs of run 1, which `push`ed `main` at the head.
    pub queued: Vec<Value>,
    /// Whether runner 77 runs a job; `None` once it is gone.
    pub busy: Option<bool>,
    /// The job each run lists as taken by runner 77.
    pub taken: HashMap<u64, Value>,
    /// The labels runner 77 takes jobs for.
    pub labels: Vec<&'static str>,
    /// The runners deleted.
    pub deleted: Vec<String>,
    /// The runs cancelled.
    pub cancelled: Vec<String>,
}

/// A repository whose origin is a GitHub that registers runner 77 for each
/// job, with an `actions/runner` installation on the daemon's `PATH`.
pub(super) struct GitHubFixture {
    pub _root: tempfile::TempDir,
    pub _server: Server,
    pub manager: Arc<Manager>,
    pub head: String,
    /// A commit only runs 2 and 5, pushed to `topic`, are at; run 4's
    /// commit is on no remote, and run 5's jobs cannot be listed.
    pub topic: String,
    pub state: Arc<Mutex<GitHubState>>,
}

pub(super) async fn github_fixture() -> GitHubFixture {
    let (root, manager) = manager().await;
    let path = repository(root.path(), "repo");
    let repo = manager
        .register_repository(path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let head = git(&path, &["rev-parse", "HEAD"]).trim().to_owned();
    git(&path, &["commit", "--allow-empty", "-qm", "topic"]);
    let topic = git(&path, &["rev-parse", "HEAD"]).trim().to_owned();
    git(&path, &["reset", "-q", "--hard", &head]);
    let state = Arc::new(Mutex::new(GitHubState {
        busy: Some(false),
        labels: vec!["shoal"],
        ..GitHubState::default()
    }));
    let (seen, pushed, other) = (state.clone(), head.clone(), topic.clone());
    let host = Arc::new(std::sync::OnceLock::<String>::new());
    let named = host.clone();
    let server = Server::start(move |request| {
        let line = request.line.as_str();
        let mut state = seen.lock().unwrap();
        let run = |id: u64, branch: &str, commit: &str| {
            let host = named.get().unwrap();
            json!({
                "id": id, "event": "push", "head_branch": branch, "head_sha": commit,
                "status": "in_progress", "conclusion": null,
                "pull_requests": [], "path": ".github/workflows/ci.yml",
                "html_url": format!("http://{host}/team/repo/actions/runs/{id}"),
            })
        };
        let body = if line
            == "GET /api/v3/repos/team/repo/actions/runs?status=queued&per_page=100&page=1"
        {
            json!({"workflow_runs": [{"id": 1}]})
        } else if line.starts_with("GET /api/v3/repos/team/repo/actions/runs?status=") {
            json!({"workflow_runs": []})
        } else if let Some(listed) = line
            .strip_prefix("GET /api/v3/repos/team/repo/actions/runs/")
            .and_then(|rest| rest.strip_suffix("/jobs?filter=latest&per_page=100&page=1"))
        {
            let id: u64 = listed.parse().unwrap();
            if id == 5 {
                return (403, Vec::new(), b"{}".to_vec());
            }
            let mut jobs = if id == 1 {
                state.queued.clone()
            } else {
                Vec::new()
            };
            jobs.extend(state.taken.get(&id).cloned());
            json!({"jobs": jobs})
        } else if line == "GET /api/v3/repos/team/repo/actions/runs/1" {
            run(1, "main", &pushed)
        } else if line == "GET /api/v3/repos/team/repo/actions/runs/2" {
            run(2, "topic", &other)
        } else if line == "GET /api/v3/repos/team/repo/actions/runs/5" {
            run(5, "topic", &other)
        } else if line == "GET /api/v3/repos/team/repo/actions/runs/4" {
            run(4, "gone", "1111111111111111111111111111111111111111")
        } else if line == "POST /api/v3/repos/team/repo/actions/runners/generate-jitconfig" {
            json!({"runner": {"id": 77}, "encoded_jit_config": "Y29uZmln"})
        } else if line == "GET /api/v3/repos/team/repo/actions/runners/77" {
            match state.busy {
                Some(busy) => {
                    let labels: Vec<_> = state
                        .labels
                        .iter()
                        .map(|name| json!({"name": name}))
                        .collect();
                    json!({"id": 77, "busy": busy, "labels": labels})
                }
                None => return (404, Vec::new(), b"{}".to_vec()),
            }
        } else if let Some(run) = line
            .strip_prefix("POST /api/v3/repos/team/repo/actions/runs/")
            .and_then(|rest| rest.strip_suffix("/cancel"))
        {
            state.cancelled.push(run.to_owned());
            return (202, Vec::new(), b"{}".to_vec());
        } else if line.starts_with("DELETE /api/v3/repos/team/repo/actions/runners/") {
            state
                .deleted
                .push(line.rsplit('/').next().unwrap().to_owned());
            state.busy = None;
            return (204, Vec::new(), Vec::new());
        } else {
            return (404, Vec::new(), b"{}".to_vec());
        };
        (200, Vec::new(), body.to_string().into())
    });
    host.set(server.host.clone()).unwrap();
    crate::forge::hosts::learn(&server.host, ForgeKind::GitHub);
    let install = root.path().join("actions-runner");
    std::fs::create_dir_all(install.join("bin")).unwrap();
    std::fs::write(install.join("run.sh"), "").unwrap();
    *manager.runners.path.lock().unwrap() = Some(runner_program(&install, "Runner.Listener"));
    let origin = format!("http://{}/team/repo", server.host);
    git(&path, &["remote", "add", "origin", &origin]);
    git(&path, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
    git(
        &path,
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/main",
        ],
    );
    configure(
        &manager,
        &format!(
            "[cleanup]\ngrace_minutes = 0\n[machine]\nmax_jobs = 2\n[machine.forges.\"{}\"]\ntype = \"github\"\ntoken = \"user\"\n[runner]\nenabled = true\n",
            server.host
        ),
    )
    .await;
    let id = repo.id.clone();
    manager
        .store
        .run(move |db| {
            db.execute(
                "INSERT INTO runners(repository_id,forge_repository) VALUES (?1,?2)",
                rusqlite::params![id, origin],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    GitHubFixture {
        _root: root,
        _server: server,
        manager,
        head,
        topic,
        state,
    }
}

/// A queued job of GitHub's run `run` for the label `shoal`.
pub(super) fn github_job(id: u64, run: u64) -> Value {
    json!({"id": id, "run_id": run, "name": "rust", "status": "queued", "labels": ["shoal"], "runner_id": null})
}

/// The command launched in `workspace`, and the execution standing in for it.
pub(super) fn launched(workspace: &str) -> Launched {
    LAUNCHED.lock().unwrap().remove(workspace).unwrap()
}

/// Queue the forge's waiting jobs, admit them and claim each admitted one.
pub(super) async fn admit_and_claim(manager: &Manager) -> Vec<QueueEntry> {
    manager.refresh_waiting_jobs().await.unwrap();
    manager.dispatch_queue().await.unwrap();
    let admitted = manager.admitted_jobs().await.unwrap();
    for entry in &admitted {
        manager.claim_job(entry).await;
    }
    admitted
}

async fn job_entries(manager: &Manager) -> Vec<QueueEntry> {
    manager
        .queue_entries()
        .await
        .unwrap()
        .into_iter()
        .filter(|entry| entry.kind == QueueKind::Job)
        .collect()
}

#[tokio::test]
async fn a_claimed_job_runs_the_forges_runner_in_its_own_workspace_until_it_ends() {
    let fixture = fixture(vec![job("h1", 4, 1)], "unused", "").await;
    let manager = &fixture.manager;

    let admitted = admit_and_claim(manager).await;
    assert_eq!(admitted.len(), 1);
    // The forge lists the job until its runner takes it, which queues it no more.
    manager.refresh_waiting_jobs().await.unwrap();
    assert!(job_entries(manager).await.is_empty());
    let claimed = manager.claimed_jobs().await.unwrap();
    assert_eq!(claimed.len(), 1);
    let workspace = manager
        .workspace_by_id(&claimed[0].workspace_id)
        .await
        .unwrap();
    assert_eq!(workspace.name, "ci-rust-4");
    assert_eq!(workspace.repository_id, fixture.repository);
    assert_eq!(workspace.branch, None);
    assert_eq!(workspace.at.as_ref().unwrap().commit, fixture.head);
    assert!(workspace.disposable && workspace.setup_skipped);

    let (command, started) = LAUNCHED.lock().unwrap().remove(&workspace.id).unwrap();
    let argv: Vec<_> = command
        .argv
        .iter()
        .map(|arg| arg.to_str().unwrap())
        .collect();
    let directory = manager.job_directory(&claimed[0].id);
    let token = directory.join("token");
    assert!(argv[0].ends_with("/bin/forgejo-runner"), "{argv:?}");
    assert_eq!(
        argv[1..5],
        [
            "--config",
            directory.join("config.yaml").to_str().unwrap(),
            "one-job",
            "--url",
        ]
    );
    assert!(
        argv.windows(2).any(|pair| pair == ["--handle", "h1"]),
        "{argv:?}"
    );
    assert!(
        argv.windows(2)
            .any(|pair| pair == ["--uuid", "runner-uuid"]),
        "{argv:?}"
    );
    assert!(
        argv.windows(2)
            .any(|pair| pair == ["--label", "shoal:host"]),
        "{argv:?}"
    );
    assert_eq!(std::fs::read_to_string(&token).unwrap(), "runner-token");
    let config = std::fs::read_to_string(directory.join("config.yaml")).unwrap();
    assert!(
        config.contains(&format!("\"SHOAL_WORKSPACE_ID\": \"{}\"", workspace.id)),
        "{config}"
    );
    assert_eq!(
        std::fs::metadata(&token).unwrap().permissions().mode() & 0o777,
        0o600
    );

    // A running job keeps its workspace.
    manager.finish_ended_jobs().await.unwrap();
    assert!(manager.workspace_by_id(&workspace.id).await.is_ok());

    manager
        .finish_execution(started.plan.id.clone(), ExecutionKind::Command, Some(0))
        .await
        .unwrap();
    manager.finish_ended_jobs().await.unwrap();
    assert!(manager.workspace_by_id(&workspace.id).await.is_err());
    assert!(manager.claimed_jobs().await.unwrap().is_empty());
    assert!(!directory.exists());
}

#[tokio::test]
async fn a_job_whose_pr_moved_on_is_passed_over_without_a_workspace() {
    let fixture = fixture(vec![job("old", 4, 2)], "new", "").await;
    let manager = &fixture.manager;

    admit_and_claim(manager).await;
    assert!(job_entries(manager).await.is_empty());
    assert!(manager.claimed_jobs().await.unwrap().is_empty());
    assert!(
        manager
            .list_workspaces()
            .await
            .unwrap()
            .iter()
            .all(|workspace| !workspace.name.starts_with("ci-"))
    );
    // The forge still lists it, but the runner leaves it to others.
    manager.refresh_waiting_jobs().await.unwrap();
    assert!(job_entries(manager).await.is_empty());
    let notifications = manager.notifications(false, 50).await.unwrap();
    assert!(
        notifications
            .iter()
            .all(|notification| notification.kind != NotificationKind::RunnerFailed),
        "{notifications:?}"
    );
}

#[tokio::test]
async fn a_job_waits_while_its_permit_pool_is_full() {
    let fixture = fixture(
        vec![job("h1", 4, 1)],
        "unused",
        "permit = \"builds\"\n[resources.builds]\n",
    )
    .await;
    let manager = &fixture.manager;
    let other = manager
        .create_workspace(&fixture.repository, "other".into(), None, None, None)
        .await
        .unwrap();
    let request = ResourceRequest {
        mode: None,
        kind: None,
        pool: "builds".into(),
        name: "held".into(),
        resource: None,
        reason: None,
        queue: false,
    };
    manager
        .acquire_resource(&SpaceSelector::id(&other.id), request, None)
        .await
        .unwrap();

    admit_and_claim(manager).await;
    let waiting = job_entries(manager).await;
    assert_eq!(waiting.len(), 1);
    assert_eq!(waiting[0].state, QueueState::Queued);
    assert!(manager.claimed_jobs().await.unwrap().is_empty());
    assert!(manager.workspace(&"ci-rust-4".into()).await.is_err());
    // It waits a while before admission tries it again.
    manager.dispatch_queue().await.unwrap();
    assert!(manager.admitted_jobs().await.unwrap().is_empty());

    // Once the pool has a permit, the job holds it while it runs.
    manager.runners.forget_permits_busy();
    manager
        .release_resource(
            &SpaceSelector::id(&other.id),
            "builds".into(),
            "held".into(),
        )
        .await
        .unwrap();
    admit_and_claim(manager).await;
    let claimed = manager.claimed_jobs().await.unwrap();
    assert_eq!(claimed.len(), 1);
    let leases = manager
        .list_resources(Some(&SpaceSelector::id(&claimed[0].workspace_id)))
        .await
        .unwrap();
    assert_eq!(leases.len(), 1);
    LAUNCHED.lock().unwrap().remove(&claimed[0].workspace_id);
}

#[tokio::test]
async fn a_restart_during_a_claim_keeps_a_started_command_and_removes_the_rest() {
    let fixture = fixture(vec![job("h1", 4, 1)], "unused", "").await;
    let manager = &fixture.manager;
    admit_and_claim(manager).await;
    let claimed = manager.claimed_jobs().await.unwrap();
    let (_, started) = LAUNCHED
        .lock()
        .unwrap()
        .remove(&claimed[0].workspace_id)
        .unwrap();
    let forget_execution = || {
        let id = claimed[0].id.clone();
        manager.store.run(move |db| {
            db.execute("UPDATE ci_jobs SET execution_id=NULL WHERE id=?1", [id])?;
            Ok(())
        })
    };
    // As if the daemon stopped after the command started but before its
    // execution was recorded: the running command is kept.
    forget_execution().await.unwrap();
    manager.finish_ended_jobs().await.unwrap();
    let kept = manager.claimed_jobs().await.unwrap();
    assert_eq!(
        kept[0].execution_id.as_ref().map(ExecutionId::as_str),
        Some(started.plan.id.as_str())
    );

    // As if it stopped before the command started.
    manager
        .finish_execution(started.plan.id.clone(), ExecutionKind::Command, Some(0))
        .await
        .unwrap();
    forget_execution().await.unwrap();
    manager.finish_ended_jobs().await.unwrap();
    assert!(manager.claimed_jobs().await.unwrap().is_empty());
    assert!(
        manager
            .workspace_by_id(&claimed[0].workspace_id)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn a_jobs_directory_goes_however_its_workspace_went() {
    let fixture = fixture(vec![job("h1", 4, 1)], "unused", "").await;
    let manager = &fixture.manager;
    admit_and_claim(manager).await;
    let claimed = manager.claimed_jobs().await.unwrap();
    LAUNCHED.lock().unwrap().remove(&claimed[0].workspace_id);
    let directory = manager.job_directory(&claimed[0].id);
    assert!(directory.join("token").exists());
    manager
        .remove_job_workspace(&claimed[0].workspace_id)
        .await
        .unwrap();
    manager.finish_ended_jobs().await.unwrap();
    assert!(!directory.exists());
}

#[tokio::test]
async fn turning_the_runner_off_withdraws_its_jobs_without_claiming_them() {
    let fixture = fixture(vec![job("h1", 4, 1), job("h2", 5, 1)], "unused", "").await;
    let manager = &fixture.manager;
    manager.refresh_waiting_jobs().await.unwrap();
    manager.dispatch_queue().await.unwrap();
    // Low free disk space admits one of them, more room both.
    let admitted = manager.admitted_jobs().await.unwrap();
    assert!(!admitted.is_empty());
    let id = fixture.repository.clone();
    manager
        .write_local_config(&id, Some("[runner]\nenabled = false\n".into()))
        .await
        .unwrap();

    // An admitted job leaves the queue unclaimed, and every other with it.
    manager.claim_job(&admitted[0]).await;
    assert!(job_entries(manager).await.is_empty());
    assert!(manager.claimed_jobs().await.unwrap().is_empty());
    let notifications = manager.notifications(false, 50).await.unwrap();
    assert!(
        notifications
            .iter()
            .all(|notification| notification.kind != NotificationKind::RunnerFailed),
        "{notifications:?}"
    );
    // So do waiting ones once the runners are checked.
    manager.refresh_waiting_jobs().await.unwrap();
    manager
        .write_local_config(&id, Some("[runner]\nenabled = true\n".into()))
        .await
        .unwrap();
    manager.refresh_waiting_jobs().await.unwrap();
    assert_eq!(job_entries(manager).await.len(), 2);
    manager
        .write_local_config(&id, Some("[runner]\nenabled = false\n".into()))
        .await
        .unwrap();
    manager.reconcile_runners().await.unwrap();
    assert!(job_entries(manager).await.is_empty());
}

#[tokio::test]
async fn a_jobs_workspace_names_its_job_and_lists_with_its_branchs_workspace() {
    let fixture = fixture(vec![job("h3", 7, 3)], "unused", "").await;
    let manager = &fixture.manager;
    let topic = manager
        .create_workspace(&fixture.repository, "topic".into(), None, None, None)
        .await
        .unwrap();
    admit_and_claim(manager).await;
    let claimed = manager.claimed_jobs().await.unwrap();
    assert_eq!(claimed.len(), 1);
    LAUNCHED.lock().unwrap().remove(&claimed[0].workspace_id);

    let listed = manager.visible_workspaces(None).await.unwrap();
    let job = listed
        .iter()
        .find(|workspace| workspace.id == claimed[0].workspace_id)
        .unwrap();
    let named = job.job.as_ref().unwrap();
    assert_eq!(named.job.name, "rust");
    assert_eq!(named.workspace_id.as_ref(), Some(&topic.id));
    let linked = manager.linked_job_workspaces(&topic.id).await.unwrap();
    assert_eq!(linked.len(), 1);
    assert_eq!(linked[0].id, claimed[0].workspace_id);
}

#[tokio::test]
async fn stopping_a_jobs_workspace_cancels_its_run_and_leaves_the_end_to_the_forge() {
    let fixture = fixture(vec![job("h3", 7, 3)], "unused", "").await;
    let manager = &fixture.manager;
    admit_and_claim(manager).await;
    let claimed = manager.claimed_jobs().await.unwrap();
    let (_, started) = LAUNCHED
        .lock()
        .unwrap()
        .remove(&claimed[0].workspace_id)
        .unwrap();
    // A running job stays cancellable once its runner is turned off.
    manager
        .write_local_config(
            &fixture.repository,
            Some("[runner]\nenabled = false\n".into()),
        )
        .await
        .unwrap();
    manager.reconcile_runners().await.unwrap();
    assert!(
        manager
            .runner_registration(&fixture.repository)
            .await
            .unwrap()
            .is_none()
    );

    let stopped = manager
        .stop_workspace_manually(
            &SpaceSelector::id(&claimed[0].workspace_id),
            false,
            None,
            crate::protocol::QueuedLaunches::Cancel,
        )
        .await
        .unwrap();
    assert!(
        stopped
            .cancelled_run
            .is_some_and(|run| run.ends_with("/team/repo/actions/runs/3"))
    );
    assert!(
        fixture
            .server
            .received()
            .iter()
            .any(|request| request.line == "POST /api/v1/repos/team/repo/actions/runs/3/cancel")
    );
    // The job runs until the forge's runner stops it.
    manager.finish_ended_jobs().await.unwrap();
    assert_eq!(manager.claimed_jobs().await.unwrap().len(), 1);
    manager
        .finish_execution(started.plan.id.clone(), ExecutionKind::Command, Some(1))
        .await
        .unwrap();
    manager.finish_ended_jobs().await.unwrap();
    assert!(manager.claimed_jobs().await.unwrap().is_empty());
}

/// The next message on `stream`, failing the test when none comes.
async fn read<T: serde::de::DeserializeOwned>(
    stream: &mut tokio::io::BufReader<tokio::net::UnixStream>,
) -> T {
    tokio::time::timeout(
        std::time::Duration::from_secs(60),
        crate::protocol::read_buffered(stream),
    )
    .await
    .expect("the daemon sent nothing")
    .unwrap()
}

/// How the daemon can stop work as it exits; saving is how a signal or a
/// system shutdown stops it.
const ENDINGS: [Ending; 2] = [Ending::Stop, Ending::Save];

#[tokio::test]
async fn the_daemon_exiting_cancels_a_jobs_run_and_stops_its_runner() {
    for ending in ENDINGS {
        let fixture = daemon_exit_with_runner(job("h3", 7, 3), ending).await;
        assert!(
            fixture.server.received().iter().any(|request| request.line
                == "POST /api/v1/repos/team/repo/actions/runs/3/cancel"),
            "{ending:?}"
        );
    }
}

#[tokio::test]
async fn the_daemon_exiting_stops_a_jobs_runner_when_the_forge_refuses_to_cancel_its_run() {
    for ending in ENDINGS {
        // The forge finds no run 1 to cancel.
        let fixture = daemon_exit_with_runner(job("h1", 4, 1), ending).await;
        let failures: Vec<_> = fixture
            .manager
            .notifications(true, 10)
            .await
            .unwrap()
            .into_iter()
            .filter(|notification| notification.kind == NotificationKind::RunnerFailed)
            .map(|notification| notification.message)
            .collect();
        assert_eq!(failures.len(), 1, "{ending:?}: {failures:?}");
    }
}

/// End the daemon's work with `ending` while `job` runs, connected as its
/// wrapper runs it, checking the daemon stops the runner rather than
/// waiting for it to see its run cancelled.
async fn daemon_exit_with_runner(job: Value, ending: Ending) -> Fixture {
    use tokio::{io::BufReader, net::UnixStream};

    use crate::{
        daemon::execution_connection::{ExecutionContext, execute},
        protocol::{self, Body, Control, ExecutionEvent, Response},
    };

    let fixture = fixture(vec![job], "unused", "").await;
    let manager = &fixture.manager;
    admit_and_claim(manager).await;
    let claimed = manager.claimed_jobs().await.unwrap();
    let workspace = claimed[0].workspace_id.clone();
    let (_, started) = LAUNCHED.lock().unwrap().remove(&workspace).unwrap();
    // The runner as its wrapper runs it: connected to the daemon.
    manager
        .finish_execution(started.plan.id.clone(), ExecutionKind::Command, Some(0))
        .await
        .unwrap();
    let wrapper = crate::process::identity::capture(std::process::id())
        .unwrap()
        .unwrap();
    let (client, server) = UnixStream::pair().unwrap();
    let executing = tokio::spawn(execute(
        server,
        1,
        manager.clone(),
        ExecutionContext {
            workspace: SpaceSelector::parse(workspace.as_str()),
            wrapper: wrapper.clone(),
            kind: ExecutionKind::Command,
            agent: None,
            recover: false,
            reattach: false,
            queue_entry: None,
            demo: None,
            origin: None,
            merge: None,
            caller: None,
        },
        manager.gates.owned_operation().await,
    ));
    let mut connection = BufReader::new(client);
    let response: Response = read(&mut connection).await;
    let Body::Execution(plan) = response.body else {
        panic!("execution was not registered: {:?}", response.body);
    };
    let claimed = workspace.clone();
    manager
        .store
        .run(move |db| {
            db.execute(
                "UPDATE ci_jobs SET execution_id=?1 WHERE workspace_id=?2",
                rusqlite::params![plan.id, claimed],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let event = ExecutionEvent::Started {
        child: None,
        group_id: wrapper.pid,
        restored: false,
    };
    protocol::write(connection.get_mut(), &event).await.unwrap();
    let control: Control = read(&mut connection).await;
    assert!(matches!(control, Control::Started));

    let ending = tokio::spawn({
        let manager = manager.clone();
        async move { manager.end_work(ending).await }
    });
    let control: Control = read(&mut connection).await;
    assert!(matches!(&control, Control::Pause { .. }), "{control:?}");
    let event = ExecutionEvent::Finished { exit_code: 143 };
    protocol::write(connection.get_mut(), &event).await.unwrap();
    let control: Control = read(&mut connection).await;
    assert!(matches!(control, Control::Finished { .. }), "{control:?}");
    executing.await.unwrap().unwrap();
    ending.await.unwrap();
    fixture
}

#[tokio::test]
async fn a_job_admitted_as_the_system_shuts_down_waits_in_the_queue() {
    let fixture = fixture(vec![job("h1", 4, 1)], "unused", "").await;
    let manager = &fixture.manager;
    manager.refresh_waiting_jobs().await.unwrap();
    manager.dispatch_queue().await.unwrap();
    let admitted = manager.admitted_jobs().await.unwrap();
    assert_eq!(admitted.len(), 1);
    manager.agent_queue.suspend(true).await;
    manager.claim_job(&admitted[0]).await;
    let waiting = job_entries(manager).await;
    assert_eq!(waiting.len(), 1);
    assert_eq!(waiting[0].state, QueueState::Queued);
    assert!(manager.claimed_jobs().await.unwrap().is_empty());
    assert!(manager.workspace(&"ci-rust-4".into()).await.is_err());
    manager.agent_queue.suspend(false).await;
}

#[tokio::test]
async fn a_claim_a_restart_cut_short_gives_its_permit_back_before_the_job_is_claimed_again() {
    let fixture = fixture(
        vec![job("h1", 4, 1)],
        "unused",
        "permit = \"builds\"\n[resources.builds]\n",
    )
    .await;
    let manager = &fixture.manager;
    manager.refresh_waiting_jobs().await.unwrap();
    manager.dispatch_queue().await.unwrap();
    let admitted = manager.admitted_jobs().await.unwrap();
    assert_eq!(admitted.len(), 1);
    let entry = &admitted[0];
    let job = entry.job.as_deref().unwrap();
    // As if the daemon stopped once the claim's workspace held the permit.
    let workspace = manager
        .create_job_workspace(
            &fixture.repository,
            &entry.name,
            &job.run.commit,
            job.run.reference.as_deref(),
        )
        .await
        .unwrap();
    let forge = crate::forge::ForgeRepo::resolve(
        &format!("http://{}/team/repo", fixture.server.host),
        &manager.forge_hosts(),
    )
    .await
    .unwrap();
    manager
        .record_claim(entry, &workspace.id, &forge, job)
        .await
        .unwrap();
    let request = ResourceRequest {
        mode: None,
        kind: None,
        pool: "builds".into(),
        name: "ci-job".into(),
        resource: None,
        reason: None,
        queue: false,
    };
    manager
        .acquire_resource(&SpaceSelector::id(&workspace.id), request, None)
        .await
        .unwrap();

    // The cut-short claim's workspace goes with its permit, and the job,
    // still admitted, is claimed again.
    manager.finish_ended_jobs().await.unwrap();
    assert!(manager.workspace_by_id(&workspace.id).await.is_err());
    manager.claim_job(entry).await;
    let claimed = manager.claimed_jobs().await.unwrap();
    assert_eq!(claimed.len(), 1);
    assert_ne!(claimed[0].workspace_id, workspace.id);
    let leases = manager
        .list_resources(Some(&SpaceSelector::id(&claimed[0].workspace_id)))
        .await
        .unwrap();
    assert_eq!(leases.len(), 1);
    LAUNCHED.lock().unwrap().remove(&claimed[0].workspace_id);
}

#[tokio::test]
async fn removing_a_running_jobs_workspace_cancels_its_run_first() {
    let fixture = fixture(vec![job("h3", 7, 3)], "unused", "").await;
    let manager = &fixture.manager;
    admit_and_claim(manager).await;
    let claimed = manager.claimed_jobs().await.unwrap();
    LAUNCHED.lock().unwrap().remove(&claimed[0].workspace_id);
    manager
        .remove_job_workspace(&claimed[0].workspace_id)
        .await
        .unwrap();
    assert!(
        fixture
            .server
            .received()
            .iter()
            .any(|request| request.line == "POST /api/v1/repos/team/repo/actions/runs/3/cancel")
    );
    assert!(manager.claimed_jobs().await.unwrap().is_empty());
}

#[tokio::test]
async fn removing_a_job_whose_execution_is_not_recorded_yet_cancels_its_run() {
    let fixture = fixture(vec![job("h3", 7, 3)], "unused", "").await;
    let manager = &fixture.manager;
    admit_and_claim(manager).await;
    let claimed = manager.claimed_jobs().await.unwrap();
    LAUNCHED.lock().unwrap().remove(&claimed[0].workspace_id);
    let id = claimed[0].id.clone();
    // As if removal came between the runner's start and its record.
    manager
        .store
        .run(move |db| {
            db.execute("UPDATE ci_jobs SET execution_id=NULL WHERE id=?1", [id])?;
            Ok(())
        })
        .await
        .unwrap();
    manager
        .remove_job_workspace(&claimed[0].workspace_id)
        .await
        .unwrap();
    assert!(
        fixture
            .server
            .received()
            .iter()
            .any(|request| request.line == "POST /api/v1/repos/team/repo/actions/runs/3/cancel")
    );
}

#[tokio::test]
async fn a_job_keeps_warming_until_its_runner_starts() {
    use crate::daemon::workspace::capacity::{Activity, WARM_UP};
    let fixture = fixture(vec![job("h1", 4, 1)], "unused", "").await;
    let manager = &fixture.manager;
    let ages = || async {
        manager
            .job_activities()
            .await
            .unwrap()
            .into_iter()
            .map(|activity| match activity {
                Activity::Job { age, .. } => age,
                other => panic!("{other:?}"),
            })
            .collect::<Vec<_>>()
    };
    manager.refresh_waiting_jobs().await.unwrap();
    manager.dispatch_queue().await.unwrap();
    assert_eq!(ages().await, [std::time::Duration::ZERO]);
    admit_and_claim(manager).await;
    let claimed = manager.claimed_jobs().await.unwrap();
    LAUNCHED.lock().unwrap().remove(&claimed[0].workspace_id);
    let id = claimed[0].id.clone();
    let set = |sql: &'static str| {
        let id = id.clone();
        manager.store.run(move |db| {
            db.execute(sql, [id])?;
            Ok(())
        })
    };
    // Its age counts from its runner's start, however long ago it was admitted.
    set("UPDATE ci_jobs SET started_at=0,launched_at=strftime('%s','now')-600 WHERE id=?1")
        .await
        .unwrap();
    assert!(ages().await[0] >= WARM_UP);
    set("UPDATE ci_jobs SET launched_at=NULL WHERE id=?1")
        .await
        .unwrap();
    assert_eq!(ages().await, [std::time::Duration::ZERO]);
}

#[tokio::test]
async fn the_sweep_keeps_the_directory_of_a_recorded_claim() {
    let fixture = fixture(vec![job("h1", 4, 1)], "unused", "").await;
    let manager = &fixture.manager;
    admit_and_claim(manager).await;
    let claimed = manager.claimed_jobs().await.unwrap();
    LAUNCHED.lock().unwrap().remove(&claimed[0].workspace_id);
    let stray = manager.job_directory("stray");
    std::fs::create_dir_all(&stray).unwrap();
    manager.finish_ended_jobs().await.unwrap();
    assert!(manager.job_directory(&claimed[0].id).join("token").exists());
    assert!(!stray.exists());
}

#[tokio::test]
async fn a_runner_gone_while_the_daemon_was_away_gives_its_job_up_once_nothing_of_it_runs() {
    use crate::process::identity::{Identity, capture};
    let fixture = fixture(vec![job("h1", 4, 1)], "unused", "").await;
    let manager = &fixture.manager;
    admit_and_claim(manager).await;
    let claimed = manager.claimed_jobs().await.unwrap();
    LAUNCHED.lock().unwrap().remove(&claimed[0].workspace_id);
    let execution = claimed[0].execution_id.clone().unwrap();
    let mark = |wrapper: &Identity, child: &Identity| {
        let group = i64::from(child.pid);
        let (execution, wrapper, child) = (
            execution.clone(),
            serde_json::to_string(wrapper).unwrap(),
            serde_json::to_string(child).unwrap(),
        );
        manager.store.run(move |db| {
            db.execute(
                "UPDATE executions SET state='unknown',wrapper=?2,child=?3,group_id=?4 WHERE id=?1",
                rusqlite::params![execution, wrapper, child, group],
            )?;
            Ok(())
        })
    };
    // A process that ended, standing for the lost wrapper and runner.
    let mut ended = std::process::Command::new("sleep")
        .arg("30")
        .process_group(0)
        .spawn()
        .unwrap();
    let gone = capture(ended.id()).unwrap().unwrap();
    ended.kill().unwrap();
    ended.wait().unwrap();

    // A wrapper still alive may reattach, so the job keeps running.
    let alive = capture(std::process::id()).unwrap().unwrap();
    mark(&alive, &gone).await.unwrap();
    manager.finish_ended_jobs().await.unwrap();
    assert_eq!(manager.claimed_jobs().await.unwrap().len(), 1);

    // A descendant carrying the execution's marker keeps it too, and is
    // stopped.
    // It runs this test binary, whose environment the scan can read where a
    // system binary's may be hidden.
    let mut marked = crate::daemon::test_support::marked_child();
    marked
        .env_clear()
        .env("SHOAL_TEST_PROCESS", "1")
        .env(crate::env::EXECUTION_ID, &execution)
        .process_group(0)
        .kill_on_drop(true);
    let mut survivor = crate::daemon::test_support::spawn_marked_child(&mut marked);
    mark(&gone, &gone).await.unwrap();
    manager.finish_ended_jobs().await.unwrap();
    assert_eq!(manager.claimed_jobs().await.unwrap().len(), 1);
    survivor.wait().await.unwrap();

    // Once nothing of it runs, the job ends here.
    manager.finish_ended_jobs().await.unwrap();
    assert!(manager.claimed_jobs().await.unwrap().is_empty());
    assert!(
        manager
            .workspace_by_id(&claimed[0].workspace_id)
            .await
            .is_err()
    );
}
