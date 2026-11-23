use shoal_ids::execution_ids::ExecutionId;
use shoal_ids::repository_ids::RepositoryId;
use shoal_ids::space_ids::SpaceId;
use std::{
    collections::HashMap,
    ffi::OsString,
    os::unix::{fs::PermissionsExt as _, process::CommandExt as _},
    sync::{Arc, LazyLock, Mutex},
};

use serde_json::{Value, json};

use crate::{
    daemon::{
        daemon_exit::Ending,
        executions::{ExecutionKind, StartedExecution},
        lifecycle::STOPPED_REMOVAL,
        manager::Manager,
        notifications::AlertKind,
        runner::JobStop,
        test_support::{configure, git, manager, repository},
    },
    forge::backends::ForgeKind,
    model::{QueueEntry, Workspace},
    state::{QueueKind, QueueState, WorkspaceState},
};
use shoal_forge::{RepoRequests, ci::runner::JobCommand, test_support::Server};
use shoal_protocol::resources::ResourceRequest;
use shoal_protocol::space_names::SpaceSelector;
use shoal_runner::daemon::Workspaces as _;

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
        .agents
        .begin_execution(
            manager,
            workspace.id.as_str(),
            None,
            ExecutionKind::Command,
            None,
        )
        .await?;
    let id = started.plan.id.clone();
    LAUNCHED
        .lock()
        .unwrap()
        .insert(workspace.id.clone(), (command, started));
    Ok(id)
}

/// A repository whose origin is a Forgejo that lists `jobs` as waiting:
/// run 1 pushed `main` at its head and runs 2 and 8 wait for PR 12 at that
/// head, until `pull_head` moves the PR on, and the forge refuses to cancel
/// run 8; run 9 waits for PR 13, whose head cannot be read; run 3 runs, and
/// run 6 was cancelled.
pub(super) struct Fixture {
    _root: tempfile::TempDir,
    pub manager: Arc<Manager>,
    pub repository: RepositoryId,
    pub head: String,
    pub server: Server,
    /// The head PR 12 has now.
    pub pull_head: Arc<Mutex<String>>,
}

pub(super) async fn fixture(jobs: Vec<Value>, settings: &str) -> Fixture {
    let (root, manager) = manager().await;
    let path = repository(root.path(), "repo");
    let repo = manager
        .repositories
        .register(path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let head = git(&path, &["rev-parse", "HEAD"]).trim().to_owned();
    let pushed = head.clone();
    let pull_head = Arc::new(Mutex::new(head.clone()));
    let served_head = pull_head.clone();
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
                &pushed,
                json!({"pull_request": {"number": 12}}),
            )
        } else if line == "GET /api/v1/repos/team/repo/actions/runs/3" {
            run(3, "push", &pushed, json!({"ref": "refs/heads/topic"}))
        } else if line == "GET /api/v1/repos/team/repo/actions/runs?run_number=3" {
            let mut running = run(3, "push", &pushed, json!({}));
            running["status"] = json!("running");
            json!({"workflow_runs": [running]})
        } else if line == "GET /api/v1/repos/team/repo/actions/runs/6" {
            run(6, "push", &pushed, json!({"ref": "refs/heads/topic"}))
        } else if line == "GET /api/v1/repos/team/repo/actions/runs?run_number=6" {
            let mut cancelled = run(6, "push", &pushed, json!({}));
            cancelled["status"] = json!("cancelled");
            json!({"workflow_runs": [cancelled]})
        } else if line == "GET /api/v1/repos/team/repo/actions/runs/8" {
            run(8, "pull_request", &pushed, json!({"number": 12}))
        } else if line == "GET /api/v1/repos/team/repo/actions/runs/9" {
            run(
                9,
                "pull_request",
                &pushed,
                json!({"pull_request": {"number": 13}}),
            )
        } else if let Some(number) = ["2", "8"].into_iter().find(|number| {
            line == format!("GET /api/v1/repos/team/repo/actions/runs?run_number={number}")
        }) {
            let mut waiting = run(number.parse().unwrap(), "pull_request", &pushed, json!({}));
            waiting["status"] = json!("waiting");
            json!({"workflow_runs": [waiting]})
        } else if line == "GET /api/v1/repos/team/repo/actions/runs/6/jobs" {
            json!([{"id": 8, "name": "rust", "run_id": 6, "status": "cancelled"}])
        } else if line == "GET /api/v1/repos/team/repo/actions/jobs/8" {
            json!({"id": 8, "name": "rust", "run_id": 6, "task_id": 31})
        } else if line == "POST /api/actions/runner.v1.RunnerService/UpdateTask" {
            json!({"state": {"id": "31", "result": "RESULT_CANCELLED"}})
        } else if line == "POST /api/v1/repos/team/repo/actions/runs/3/cancel"
            || line == "POST /api/v1/repos/team/repo/actions/runs/2/cancel"
            || line == "POST /api/v1/repos/team/repo/actions/jobs/8/rerun"
        {
            return (204, Vec::new(), Vec::new());
        } else if line == "POST /api/v1/repos/team/repo/actions/runs/8/cancel" {
            return (403, Vec::new(), b"{}".to_vec());
        } else if line == "GET /api/v1/repos/team/repo/pulls/12" {
            json!({"number": 12, "title": "Fix", "state": "open", "head": {"sha": *served_head.lock().unwrap(), "ref": "fix"}, "base": {"ref": "main"}})
        } else {
            return (404, Vec::new(), b"{}".to_vec());
        };
        (200, Vec::new(), body.to_string().into())
    });
    crate::forge::hosts::learn(&server.host, ForgeKind::Forgejo);
    *manager.runner.jobs().path.lock().unwrap() =
        Some(runner_program(root.path(), "forgejo-runner"));
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
            "[cleanup]\ngrace_minutes = 0\n[machine]\nmax_jobs = 2\n[machine.forges.\"{}\"]\ntype = \"forgejo\"\nuser_token = \"user\"\n[runner]\nenabled = true\n{settings}",
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
        pull_head,
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

pub(super) fn job(handle: &str, id: u64, run: u64) -> Value {
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
        .repositories
        .register(path.to_str().unwrap().into(), None, None)
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
    *manager.runner.jobs().path.lock().unwrap() = Some(runner_program(&install, "Runner.Listener"));
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
            "[cleanup]\ngrace_minutes = 0\n[machine]\nmax_jobs = 2\n[machine.forges.\"{}\"]\ntype = \"github\"\nuser_token = \"user\"\n[runner]\nenabled = true\n",
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
    refresh_waiting_jobs(manager).await;
    manager.agents.dispatch_queue(manager).await.unwrap();
    let admitted = admitted_jobs(manager).await;
    for entry in &admitted {
        claim_job(manager, entry).await;
    }
    admitted
}

pub(super) async fn job_entries(manager: &Manager) -> Vec<QueueEntry> {
    manager
        .agents
        .queue_entries(manager)
        .await
        .unwrap()
        .into_iter()
        .filter(|entry| entry.kind == QueueKind::Job)
        .collect()
}

#[tokio::test]
async fn a_claimed_job_runs_the_forges_runner_in_its_own_workspace_until_it_ends() {
    let fixture = fixture(vec![job("h1", 4, 1)], "").await;
    let manager = &fixture.manager;

    let admitted = admit_and_claim(manager).await;
    assert_eq!(admitted.len(), 1);
    // The forge lists the job until its runner takes it, which queues it no more.
    refresh_waiting_jobs(manager).await;
    assert!(job_entries(manager).await.is_empty());
    let claimed = manager.runner.claimed_jobs().await.unwrap();
    assert_eq!(claimed.len(), 1);
    let workspace = manager
        .spaces
        .workspace_by_id(&claimed[0].workspace_id)
        .await
        .unwrap();
    assert_eq!(workspace.name, "rust");
    assert_eq!(workspace.directory_name(), "ci-rust-4");
    assert_eq!(workspace.repository(), &fixture.repository);
    assert_eq!(workspace.branch(), None);
    assert_eq!(workspace.at().unwrap().commit, fixture.head);
    assert!(workspace.disposable && workspace.setup_skipped);

    let (command, started) = LAUNCHED.lock().unwrap().remove(&workspace.id).unwrap();
    let argv: Vec<_> = command
        .argv
        .iter()
        .map(|arg| arg.to_str().unwrap())
        .collect();
    let directory = manager.runner.job_directory(&claimed[0].id);
    let token = directory.join("token");
    // The runner and every process it starts run at low priority.
    assert_eq!(argv[..3], ["nice", "-n", "10"]);
    assert!(argv[3].ends_with("/bin/forgejo-runner"), "{argv:?}");
    assert_eq!(
        argv[4..8],
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
        config.contains(&format!("\"SHOAL_SPACE\": \"{}\"", workspace.id)),
        "{config}"
    );
    assert_eq!(
        std::fs::metadata(&token).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert!(job_variable(&command, "CARGO_INCREMENTAL").is_none());

    // A running job keeps its workspace.
    finish_ended_jobs(manager).await;
    assert!(manager.spaces.workspace_by_id(&workspace.id).await.is_ok());

    manager.runner.jobs().take_wake();
    manager
        .agents
        .finish_execution(
            &**manager,
            started.plan.id.clone(),
            ExecutionKind::Command,
            Some(0),
        )
        .await
        .unwrap();
    // Its end wakes the runner, which removes the workspace without waiting
    // for its next poll.
    assert!(manager.runner.jobs().take_wake());
    finish_ended_jobs(manager).await;
    assert!(manager.spaces.workspace_by_id(&workspace.id).await.is_err());
    assert!(manager.runner.claimed_jobs().await.unwrap().is_empty());
    assert!(!directory.exists());
}

/// The value `command` gives its runner for `name`.
fn job_variable<'a>(command: &'a JobCommand, name: &str) -> Option<&'a str> {
    command
        .env
        .iter()
        .find(|(variable, _)| variable == name)
        .map(|(_, value)| value.as_str())
}

#[tokio::test]
async fn the_cargo_presets_jobs_build_incrementally_as_its_seed_did() {
    let fixture = fixture(vec![job("h1", 4, 1)], "").await;
    let manager = &fixture.manager;
    let presets = Some("setup.presets = ['cargo']\n".into());
    (manager.write_local_config(&fixture.repository, presets))
        .await
        .unwrap();
    admit_and_claim(manager).await;
    let claimed = manager.runner.claimed_jobs().await.unwrap();
    let (command, _) = launched(claimed[0].workspace_id.as_str());
    assert_eq!(job_variable(&command, "CARGO_INCREMENTAL"), Some("1"));
}

#[tokio::test]
async fn a_job_whose_checkout_fails_is_passed_over_and_its_workspace_removed() {
    let fixture = fixture(vec![job("h1", 4, 1)], "").await;
    let manager = &fixture.manager;
    // The commit is there, but its tree cannot be read.
    let checkout = manager
        .repositories
        .repository(&fixture.repository)
        .await
        .unwrap()
        .path;
    let tree = git(&checkout, &["rev-parse", "HEAD^{tree}"]);
    let objects = git(&checkout, &["rev-parse", "--git-path", "objects"]);
    let (fan_out, rest) = tree.trim().split_at(2);
    std::fs::remove_file(checkout.join(objects.trim()).join(fan_out).join(rest)).unwrap();

    admit_and_claim(manager).await;
    let notifications = manager.notifications.list(false, 50).await.unwrap();
    let checkout_failed = format!("check out {}", fixture.head);
    assert!(
        notifications.iter().any(|notification| {
            notification.kind == AlertKind::RunnerFailed
                && notification.message.contains(&checkout_failed)
        }),
        "{notifications:?}"
    );
    assert!(manager.runner.claimed_jobs().await.unwrap().is_empty());
    let workspaces = manager.spaces.list_workspaces().await.unwrap();
    assert!(
        workspaces.iter().all(|workspace| !workspace.disposable),
        "{workspaces:?}"
    );
    // The forge still lists it, but the runner leaves it to others.
    refresh_waiting_jobs(manager).await;
    assert!(job_entries(manager).await.is_empty());
}

/// A workspace holding the one permit of the fixture's `builds` pool.
pub(super) async fn hold_the_builds_permit(fixture: &Fixture) -> crate::model::Workspace {
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
        .leases
        .acquire_resource(&**manager, &SpaceSelector::id(&other.id), request, None)
        .await
        .unwrap();
    other
}

#[tokio::test]
async fn a_job_waits_while_its_permit_pool_is_full() {
    let fixture = fixture(
        vec![job("h1", 4, 1)],
        "permit = \"builds\"\n[resources.builds]\n",
    )
    .await;
    let manager = &fixture.manager;
    let other = hold_the_builds_permit(&fixture).await;

    admit_and_claim(manager).await;
    let waiting = job_entries(manager).await;
    assert_eq!(waiting.len(), 1);
    assert_eq!(waiting[0].state, QueueState::Queued);
    assert!(manager.runner.claimed_jobs().await.unwrap().is_empty());
    assert!(
        manager
            .spaces
            .workspace(&manager.named("ci-rust-4").await)
            .await
            .is_err()
    );
    // It waits a while before admission tries it again, held by the pool.
    manager.agents.dispatch_queue(&**manager).await.unwrap();
    assert!(admitted_jobs(manager).await.is_empty());
    assert_eq!(
        job_entries(manager).await[0].held_by,
        Some(crate::model::HeldBy::Permits)
    );

    // Once the pool has a permit, the job holds it while it runs.
    manager.runner.jobs().forget_permits_busy();
    manager
        .leases
        .release_resource(
            &**manager,
            &SpaceSelector::id(&other.id),
            "builds".into(),
            "held".into(),
        )
        .await
        .unwrap();
    admit_and_claim(manager).await;
    let claimed = manager.runner.claimed_jobs().await.unwrap();
    assert_eq!(claimed.len(), 1);
    let leases = manager
        .leases
        .list_resources(
            &**manager,
            Some(&SpaceSelector::id(&claimed[0].workspace_id)),
        )
        .await
        .unwrap();
    assert_eq!(leases.len(), 1);
    LAUNCHED.lock().unwrap().remove(&claimed[0].workspace_id);
}

#[tokio::test]
async fn a_restart_during_a_claim_keeps_a_started_command_and_removes_the_rest() {
    let fixture = fixture(vec![job("h1", 4, 1)], "").await;
    let manager = &fixture.manager;
    admit_and_claim(manager).await;
    let claimed = manager.runner.claimed_jobs().await.unwrap();
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
    finish_ended_jobs(manager).await;
    let kept = manager.runner.claimed_jobs().await.unwrap();
    assert_eq!(
        kept[0].execution_id.as_ref().map(ExecutionId::as_str),
        Some(started.plan.id.as_str())
    );

    // As if it stopped before the command started.
    manager
        .agents
        .finish_execution(
            &**manager,
            started.plan.id.clone(),
            ExecutionKind::Command,
            Some(0),
        )
        .await
        .unwrap();
    forget_execution().await.unwrap();
    finish_ended_jobs(manager).await;
    assert!(manager.runner.claimed_jobs().await.unwrap().is_empty());
    assert!(
        manager
            .spaces
            .workspace_by_id(&claimed[0].workspace_id)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn a_jobs_directory_goes_however_its_workspace_went() {
    let fixture = fixture(vec![job("h1", 4, 1)], "").await;
    let manager = &fixture.manager;
    admit_and_claim(manager).await;
    let claimed = manager.runner.claimed_jobs().await.unwrap();
    LAUNCHED.lock().unwrap().remove(&claimed[0].workspace_id);
    let directory = manager.runner.job_directory(&claimed[0].id);
    assert!(directory.join("token").exists());
    manager
        .remove_job_workspace(&claimed[0].workspace_id)
        .await
        .unwrap();
    finish_ended_jobs(manager).await;
    assert!(!directory.exists());
}

#[tokio::test]
async fn turning_the_runner_off_withdraws_its_jobs_without_claiming_them() {
    let fixture = fixture(vec![job("h1", 4, 1), job("h2", 5, 1)], "").await;
    let manager = &fixture.manager;
    refresh_waiting_jobs(manager).await;
    manager.agents.dispatch_queue(&**manager).await.unwrap();
    // Low free disk space admits one of them, more room both.
    let admitted = admitted_jobs(manager).await;
    assert!(!admitted.is_empty());
    let id = fixture.repository.clone();
    manager
        .write_local_config(&id, Some("[runner]\nenabled = false\n".into()))
        .await
        .unwrap();

    // An admitted job leaves the queue unclaimed, and every other with it.
    claim_job(manager, &admitted[0]).await;
    assert!(job_entries(manager).await.is_empty());
    assert!(manager.runner.claimed_jobs().await.unwrap().is_empty());
    let notifications = manager.notifications.list(false, 50).await.unwrap();
    assert!(
        notifications
            .iter()
            .all(|notification| notification.kind != AlertKind::RunnerFailed),
        "{notifications:?}"
    );
    // So do waiting ones once the runners are checked.
    refresh_waiting_jobs(manager).await;
    manager
        .write_local_config(&id, Some("[runner]\nenabled = true\n".into()))
        .await
        .unwrap();
    refresh_waiting_jobs(manager).await;
    assert_eq!(job_entries(manager).await.len(), 2);
    manager
        .write_local_config(&id, Some("[runner]\nenabled = false\n".into()))
        .await
        .unwrap();
    manager.runner.reconcile_runners(&**manager).await.unwrap();
    assert!(job_entries(manager).await.is_empty());
}

#[tokio::test]
async fn a_jobs_workspace_names_its_job_and_lists_with_its_branchs_workspace() {
    let fixture = fixture(vec![job("h3", 7, 3)], "").await;
    let manager = &fixture.manager;
    let topic = manager
        .create_workspace(&fixture.repository, "topic".into(), None, None, None)
        .await
        .unwrap();
    admit_and_claim(manager).await;
    let claimed = manager.runner.claimed_jobs().await.unwrap();
    assert_eq!(claimed.len(), 1);
    LAUNCHED.lock().unwrap().remove(&claimed[0].workspace_id);

    let listed = manager
        .spaces
        .listed_spaces(&**manager, None)
        .await
        .unwrap();
    let job = (listed.into_iter())
        .find_map(|space| (space.into_workspace().ok()).filter(|w| w.id == claimed[0].workspace_id))
        .unwrap();
    let named = job.job.as_ref().unwrap();
    assert_eq!(named.job.name, "rust");
    // The workspace takes the job's name, and its directory the entry's.
    assert_eq!(job.name, "rust");
    assert!(
        job.directory_name().starts_with("ci-rust-"),
        "{}",
        job.path.display()
    );
    assert_eq!(named.space_ids, std::slice::from_ref(&topic.id));
    let linked = manager
        .runner
        .linked_job_workspaces(&**manager, &topic.id)
        .await
        .unwrap();
    assert_eq!(linked.len(), 1);
    assert_eq!(linked[0].id, claimed[0].workspace_id);
}

#[tokio::test]
async fn a_waiting_job_lists_with_its_branchs_workspace_and_what_holds_it() {
    let fixture = fixture(
        vec![job("h3", 7, 3)],
        "permit = \"builds\"\n[resources.builds]\n",
    )
    .await;
    let manager = &fixture.manager;
    let topic = manager
        .create_workspace(&fixture.repository, "topic".into(), None, None, None)
        .await
        .unwrap();
    hold_the_builds_permit(&fixture).await;
    admit_and_claim(manager).await;
    manager.agents.dispatch_queue(&**manager).await.unwrap();

    let waiting = manager
        .runner
        .linked_waiting_jobs(&**manager, &topic.id)
        .await
        .unwrap();
    assert_eq!(waiting.len(), 1);
    assert_eq!(waiting[0].job.as_ref().unwrap().job.name, "rust");
    assert_eq!(waiting[0].held_by, Some(crate::model::HeldBy::Permits));
}

/// Record `space` as watching the PR `url`, or as watching none.
async fn watch_pull(manager: &Manager, space: &SpaceId, url: Option<String>) {
    let space = space.clone();
    manager
        .store
        .run(move |db| {
            match url {
                Some(url) => db.execute(
                    "INSERT INTO pr_cleanup(space_id,record) VALUES (?1,?2)",
                    rusqlite::params![space, json!({"urls": [url]}).to_string()],
                )?,
                None => db.execute("DELETE FROM pr_cleanup WHERE space_id=?1", [space])?,
            };
            Ok(())
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn every_space_that_links_a_jobs_pr_lists_the_job_waiting_and_running() {
    let fixture = fixture(
        vec![job("h2", 4, 2)],
        "permit = \"builds\"\n[resources.builds]\n",
    )
    .await;
    let manager = &fixture.manager;
    let repository = manager
        .repositories
        .list()
        .await
        .unwrap()
        .into_iter()
        .find(|repository| repository.id == fixture.repository)
        .unwrap();
    let pull = manager
        .create_workspace(&fixture.repository, "pull".into(), None, None, None)
        .await
        .unwrap();
    let second = manager
        .create_workspace(&fixture.repository, "second".into(), None, None, None)
        .await
        .unwrap();
    let chat =
        crate::daemon::test_support::chat(manager, "talk", "chat", Some(&repository), None).await;
    let url = format!("http://{}/team/repo/pulls/12", fixture.server.host);
    watch_pull(manager, &pull.id, Some(url.clone())).await;
    watch_pull(manager, &second.id, Some(url.clone())).await;
    let other = hold_the_builds_permit(&fixture).await;
    admit_and_claim(manager).await;
    manager.agents.dispatch_queue(&**manager).await.unwrap();
    let chat_status = || async {
        manager
            .spaces
            .space_status(&**manager, &SpaceSelector::id(&chat.id))
            .await
            .unwrap()
    };
    assert!(chat_status().await.waiting_jobs.is_empty());

    // Linked after the job queued, the chat lists it waiting with what
    // holds it, after each workspace that links the PR too.
    watch_pull(manager, &chat.id, Some(url)).await;
    let status = chat_status().await;
    assert_eq!(status.waiting_jobs.len(), 1);
    let waiting = status.waiting_jobs[0].job.as_ref().unwrap();
    assert_eq!(
        waiting.space_ids,
        [pull.id.clone(), second.id.clone(), chat.id.clone()]
    );
    assert_eq!(
        status.waiting_jobs[0].held_by,
        Some(crate::model::HeldBy::Permits)
    );
    assert!(status.jobs.is_empty());

    // Running, it lists there with its workspace, which stays outside the
    // chat's scope.
    manager.runner.jobs().forget_permits_busy();
    manager
        .leases
        .release_resource(
            &**manager,
            &SpaceSelector::id(&other.id),
            "builds".into(),
            "held".into(),
        )
        .await
        .unwrap();
    admit_and_claim(manager).await;
    let claimed = manager.runner.claimed_jobs().await.unwrap();
    assert_eq!(claimed.len(), 1);
    LAUNCHED.lock().unwrap().remove(&claimed[0].workspace_id);
    let status = chat_status().await;
    assert!(status.waiting_jobs.is_empty());
    assert_eq!(status.jobs.len(), 1);
    assert_eq!(status.jobs[0].id, claimed[0].workspace_id);
    assert!(
        manager
            .spaces
            .ancestors(&claimed[0].workspace_id)
            .await
            .unwrap()
            .is_empty()
    );

    // Unlinked, the chat lists it no more, and the workspace still does.
    watch_pull(manager, &chat.id, None).await;
    assert!(chat_status().await.jobs.is_empty());
    let linked = manager
        .runner
        .linked_job_workspaces(&**manager, &pull.id)
        .await
        .unwrap();
    assert_eq!(linked.len(), 1);
}

#[tokio::test]
async fn stopping_a_jobs_workspace_cancels_its_run_and_leaves_the_end_to_the_forge() {
    let fixture = fixture(vec![job("h3", 7, 3)], "").await;
    let manager = &fixture.manager;
    admit_and_claim(manager).await;
    let claimed = manager.runner.claimed_jobs().await.unwrap();
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
    manager.runner.reconcile_runners(&**manager).await.unwrap();
    assert!(
        manager
            .runner
            .runner_registration(&fixture.repository)
            .await
            .unwrap()
            .is_none()
    );

    let stopped = manager
        .agents
        .stop_space_manually(
            &**manager,
            &SpaceSelector::id(&claimed[0].workspace_id),
            false,
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
    assert_eq!(logged_stops(manager), [JobStop::Stop]);
    // The job runs until the forge's runner stops it.
    finish_ended_jobs(manager).await;
    assert_eq!(manager.runner.claimed_jobs().await.unwrap().len(), 1);
    manager
        .agents
        .finish_execution(
            &**manager,
            started.plan.id.clone(),
            ExecutionKind::Command,
            Some(1),
        )
        .await
        .unwrap();
    finish_ended_jobs(manager).await;
    assert!(manager.runner.claimed_jobs().await.unwrap().is_empty());
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

#[tokio::test]
async fn daemon_stop_cancels_a_jobs_run_and_stops_its_runner() {
    let fixture = daemon_exit_with_runner(job("h3", 7, 3), Cancelling::DaemonStop).await;
    assert!(cancelled(&fixture, 3));
    assert_eq!(logged_stops(&fixture.manager), [JobStop::DaemonStop]);
}

/// The stop before a system shutdown cancels a running job alone on its
/// forge and stops its runner, then reruns the job at once, since its run
/// has finished, telling the user.
#[tokio::test]
async fn a_system_shutdown_ends_a_job_alone_and_reruns_it() {
    let fixture = daemon_exit_with_runner(job("h6", 8, 6), Cancelling::SystemShutdown).await;
    assert!(cancelled_alone(&fixture));
    assert!(!cancelled(&fixture, 6));
    assert_eq!(logged_stops(&fixture.manager), [JobStop::SystemShutdown]);
    assert_eq!(reruns(&fixture, 8), 1);
    let told = alerts(&fixture.manager, AlertKind::JobsRerun).await;
    assert_eq!(told.len(), 1, "{told:?}");
    assert!(told[0].contains("/team/repo/actions/runs/6"), "{told:?}");
    assert!(told[0].contains("the system shutting down"), "{told:?}");
}

/// The machine going to sleep cancels a running job alone on its forge and
/// stops its runner, and the job reruns, as before a shutdown.
#[tokio::test]
async fn the_machine_going_to_sleep_ends_a_job_alone_and_reruns_it() {
    use crate::protocol::{self, Control, ExecutionEvent};

    let fixture = fixture(vec![job("h6", 8, 6)], "").await;
    let manager = &fixture.manager;
    admit_and_claim(manager).await;
    let claimed = manager.runner.claimed_jobs().await.unwrap();
    let ConnectedRunner {
        mut connection,
        executing,
    } = connect_runner(manager, &claimed[0].workspace_id, false).await;

    let stopping = tokio::spawn({
        let manager = manager.clone();
        async move {
            (manager.runner)
                .stop_jobs_to_rerun(&*manager, JobStop::Sleep)
                .await
        }
    });
    let control: Control = read(&mut connection).await;
    assert!(matches!(&control, Control::Stop { .. }), "{control:?}");
    let event = ExecutionEvent::Finished {
        exit_code: 143,
        memory: None,
    };
    protocol::write(connection.get_mut(), &event).await.unwrap();
    let control: Control = read(&mut connection).await;
    assert!(matches!(control, Control::Finished { .. }), "{control:?}");
    executing.await.unwrap().unwrap();
    stopping.await.unwrap();
    assert!(cancelled_alone(&fixture));
    assert_eq!(logged_stops(manager), [JobStop::Sleep]);
    assert_eq!(reruns(&fixture, 8), 1);
    let told = alerts(manager, AlertKind::JobsRerun).await;
    assert!(told[0].contains("the machine going to sleep"), "{told:?}");
}

#[tokio::test]
async fn the_daemon_exiting_stops_a_jobs_runner_when_the_forge_refuses_to_cancel_its_run() {
    // The forge finds no run 1 to cancel.
    let fixture = daemon_exit_with_runner(job("h1", 4, 1), Cancelling::DaemonStop).await;
    let failures = alerts(&fixture.manager, AlertKind::RunnerFailed).await;
    assert_eq!(failures.len(), 1, "{failures:?}");
}

#[tokio::test]
async fn a_signal_to_the_daemon_leaves_a_jobs_runner_running_for_the_next_daemon() {
    use crate::protocol::Control;

    let fixture = fixture(vec![job("h3", 7, 3)], "").await;
    let manager = &fixture.manager;
    admit_and_claim(manager).await;
    let claimed = manager.runner.claimed_jobs().await.unwrap();
    let ConnectedRunner {
        mut connection,
        executing,
    } = connect_runner(manager, &claimed[0].workspace_id, true).await;

    manager.end_work(Ending::Save).await;
    // The exit's last step detaches what the signal's stop left running,
    // the first control the runner hears.
    manager.agents.stop_for_shutdown().await;
    let control: Control = read(&mut connection).await;
    assert!(matches!(control, Control::Detach), "{control:?}");
    executing.await.unwrap().unwrap();
    assert!(!cancelled(&fixture, 3));
    assert!(logged_stops(manager).is_empty());
    assert_eq!(manager.runner.claimed_jobs().await.unwrap().len(), 1);
}

/// A signal ending a job's wrapper's session, as a shutdown sends, has the
/// daemon cancel the job alone before the runner stops, and the job
/// reruns once its run has finished.
#[tokio::test]
async fn a_signal_to_a_jobs_wrapper_cancels_its_job_alone_to_rerun() {
    use crate::{
        daemon::scope::{Caller, ScopedExecution},
        protocol::SessionSignal,
    };

    let fixture = fixture(vec![job("h6", 8, 6)], "").await;
    let manager = &fixture.manager;
    admit_and_claim(manager).await;
    let claimed = manager.runner.claimed_jobs().await.unwrap();
    let (_, started) = (LAUNCHED.lock().unwrap())
        .remove(&claimed[0].workspace_id)
        .unwrap();
    // The job's wrapper tells the daemon of a signal before it stops.
    assert!(started.plan.reports_signals);
    let wrapper = Caller {
        space_id: claimed[0].workspace_id.clone(),
        execution: Some(ScopedExecution {
            id: started.plan.id.clone(),
            kind: ExecutionKind::Command,
            agent_execution: None,
        }),
        granted: None,
        read_only: false,
        vouched: None,
    };
    // A step's command, which inherits the job's scope, neither reports
    // signals nor cancels the run with one.
    let step = manager
        .agents
        .begin_execution(
            &**manager,
            claimed[0].workspace_id.as_str(),
            None,
            ExecutionKind::Command,
            Some(&wrapper),
        )
        .await
        .unwrap();
    assert!(!step.plan.reports_signals);
    let step = Caller {
        execution: Some(ScopedExecution {
            id: step.plan.id.clone(),
            kind: ExecutionKind::Command,
            agent_execution: None,
        }),
        ..wrapper.clone()
    };
    (manager.runner)
        .cancel_job_for_signal(&**manager, Some(&step), SessionSignal::Terminate)
        .await
        .unwrap();
    assert!(!cancelled_alone(&fixture));

    (manager.runner)
        .cancel_job_for_signal(&**manager, Some(&wrapper), SessionSignal::Terminate)
        .await
        .unwrap();
    assert!(cancelled_alone(&fixture));
    assert!(!cancelled(&fixture, 6));
    assert_eq!(
        logged_stops(manager),
        [JobStop::Signal(SessionSignal::Terminate)]
    );
    manager.runner.rerun_stopped_jobs(&**manager).await.unwrap();
    assert_eq!(reruns(&fixture, 8), 1);
    let told = alerts(manager, AlertKind::JobsRerun).await;
    assert!(told[0].contains("SIGTERM"), "{told:?}");
}

/// What stopped each job whose stop Shoal's log recorded.
fn logged_stops(manager: &Manager) -> Vec<JobStop> {
    let stops = manager.runner.jobs().stops.lock().unwrap();
    stops.iter().map(|(_, cause)| *cause).collect()
}

/// Whether the forge was asked, through its runner protocol, to cancel a
/// job alone.
fn cancelled_alone(fixture: &Fixture) -> bool {
    (fixture.server.received().iter())
        .any(|request| request.line.ends_with("RunnerService/UpdateTask"))
}

/// How often the forge was asked to rerun the job `id`.
fn reruns(fixture: &Fixture, id: u64) -> usize {
    let rerun = format!("POST /api/v1/repos/team/repo/actions/jobs/{id}/rerun");
    (fixture.server.received().iter())
        .filter(|request| request.line == rerun)
        .count()
}

/// The messages of the user's alerts of `kind`.
async fn alerts(manager: &Manager, kind: AlertKind) -> Vec<String> {
    (manager
        .notifications
        .list(true, 10)
        .await
        .unwrap()
        .into_iter())
    .filter(|notification| notification.kind == kind)
    .map(|notification| notification.message)
    .collect()
}

/// Whether the forge was asked to cancel `run`.
fn cancelled(fixture: &Fixture, run: u64) -> bool {
    let cancel = format!("POST /api/v1/repos/team/repo/actions/runs/{run}/cancel");
    fixture
        .server
        .received()
        .iter()
        .any(|request| request.line == cancel)
}

/// How the daemon's exit ends a running job and stops its runner.
#[derive(Clone, Copy)]
enum Cancelling {
    /// `shoal daemon stop`.
    DaemonStop,
    /// The stop before a system shutdown.
    SystemShutdown,
}

/// The runner of the job claimed in `workspace`, connected to the daemon
/// as its wrapper runs it, and the daemon's side of that connection.
struct ConnectedRunner {
    connection: tokio::io::BufReader<tokio::net::UnixStream>,
    executing: tokio::task::JoinHandle<anyhow::Result<()>>,
}

/// `reattach` as a job's wrapper does, which outlives the daemon.
async fn connect_runner(
    manager: &Arc<Manager>,
    workspace: &SpaceId,
    reattach: bool,
) -> ConnectedRunner {
    use tokio::{io::BufReader, net::UnixStream};

    use crate::{
        daemon::execution_connection::{ExecutionContext, execute},
        protocol::{self, Body, Control, ExecutionEvent, Response},
    };

    let workspace = workspace.clone();
    let (_, started) = LAUNCHED.lock().unwrap().remove(&workspace).unwrap();
    // The runner as its wrapper runs it: connected to the daemon.
    manager
        .agents
        .finish_execution(
            &**manager,
            started.plan.id.clone(),
            ExecutionKind::Command,
            Some(0),
        )
        .await
        .unwrap();
    let wrapper = crate::process::identity::capture(std::process::id())
        .unwrap()
        .unwrap();
    let (client, server) = UnixStream::pair().unwrap();
    let executing = tokio::spawn(execute(
        server,
        1,
        manager.agents.clone(),
        manager.clone(),
        ExecutionContext {
            space: SpaceSelector::from(workspace.as_str()),
            wrapper: wrapper.clone(),
            kind: ExecutionKind::Command,
            agent: None,
            recover: false,
            reattach,
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
        restored_from: Vec::new(),
        reports_status: false,
    };
    protocol::write(connection.get_mut(), &event).await.unwrap();
    let control: Control = read(&mut connection).await;
    assert!(matches!(control, Control::Started));
    ConnectedRunner {
        connection,
        executing,
    }
}

/// End the daemon's work as `cancelling` does while `job` runs, connected
/// as its wrapper runs it, checking the daemon stops the runner rather than
/// waiting for it to see its run cancelled.
async fn daemon_exit_with_runner(job: Value, cancelling: Cancelling) -> Fixture {
    use crate::protocol::{self, Control, ExecutionEvent};

    let fixture = fixture(vec![job], "").await;
    let manager = &fixture.manager;
    admit_and_claim(manager).await;
    let claimed = manager.runner.claimed_jobs().await.unwrap();
    let ConnectedRunner {
        mut connection,
        executing,
    } = connect_runner(manager, &claimed[0].workspace_id, false).await;

    let ending = tokio::spawn({
        let manager = manager.clone();
        async move {
            match cancelling {
                Cancelling::DaemonStop => {
                    manager.end_work(Ending::Stop).await;
                }
                Cancelling::SystemShutdown => {
                    let window = std::time::Duration::from_secs(60);
                    (manager.agents)
                        .stop_for_system_shutdown(&*manager, window)
                        .await
                }
            }
        }
    });
    // A daemon stop saves what it stops; a job ended to rerun keeps nothing.
    let control: Control = read(&mut connection).await;
    match cancelling {
        Cancelling::DaemonStop => assert!(matches!(&control, Control::Pause { .. }), "{control:?}"),
        Cancelling::SystemShutdown => {
            assert!(matches!(&control, Control::Stop { .. }), "{control:?}")
        }
    }
    let event = ExecutionEvent::Finished {
        exit_code: 143,
        memory: None,
    };
    protocol::write(connection.get_mut(), &event).await.unwrap();
    let control: Control = read(&mut connection).await;
    assert!(matches!(control, Control::Finished { .. }), "{control:?}");
    executing.await.unwrap().unwrap();
    ending.await.unwrap();
    fixture
}

/// A process a test started that no process of the test waits for, killed
/// when the test ends however it ends.
struct Leftover(crate::process::identity::Identity);

impl Drop for Leftover {
    fn drop(&mut self) {
        let _ = crate::process::identity::signal(&self.0, libc::SIGKILL);
    }
}

#[tokio::test]
async fn a_runner_that_outlives_its_run_is_stopped_after_what_its_steps_left() {
    use crate::{
        daemon::events::{EventItem, EventKind},
        process::identity,
        protocol::{self, Control, ExecutionEvent},
    };

    let fixture = fixture(vec![job("h6", 8, 6)], "").await;
    let manager = &fixture.manager;
    admit_and_claim(manager).await;
    let claimed = manager.runner.claimed_jobs().await.unwrap();
    let workspace = manager
        .spaces
        .workspace_by_id(&claimed[0].workspace_id)
        .await
        .unwrap();
    let ConnectedRunner {
        mut connection,
        executing,
    } = connect_runner(manager, &workspace.id, false).await;
    // What a cancelled step left behind, holding the runner's output open:
    // its parent gone, in a process group of its own, as a test's script
    // runs once the test is killed.
    let started = crate::daemon::test_support::unmarked("sh")
        .args(["-c", "sleep 600 >/dev/null 2>&1 & echo $!"])
        .current_dir(&workspace.path)
        .process_group(0)
        .output()
        .unwrap();
    let pid: u32 = String::from_utf8_lossy(&started.stdout)
        .trim()
        .parse()
        .unwrap();
    let left = Leftover(identity::capture(pid).unwrap().unwrap());

    // The forge cancelled run 6, but the runner gets its grace period.
    finish_ended_jobs(manager).await;
    assert_eq!(manager.runner.claimed_jobs().await.unwrap().len(), 1);
    assert!(identity::alive(&left.0).unwrap());

    manager
        .runner
        .jobs()
        .backdate_finished_runs(shoal_runner::jobs::GRACE);
    let finishing = tokio::spawn({
        let manager = manager.clone();
        async move { manager.runner.finish_ended_jobs(&*manager).await }
    });
    let control: Control = read(&mut connection).await;
    assert!(matches!(&control, Control::Stop { .. }), "{control:?}");
    // The runner is stopped only once the processes its steps left are.
    assert!(!identity::alive(&left.0).unwrap());
    let event = ExecutionEvent::Finished {
        exit_code: 143,
        memory: None,
    };
    protocol::write(connection.get_mut(), &event).await.unwrap();
    let control: Control = read(&mut connection).await;
    assert!(matches!(control, Control::Finished { .. }), "{control:?}");
    executing.await.unwrap().unwrap();
    finishing.await.unwrap().unwrap();

    assert!(manager.runner.claimed_jobs().await.unwrap().is_empty());
    assert!(manager.spaces.workspace_by_id(&workspace.id).await.is_err());
    let ended: Vec<_> = manager
        .space_events(None, 1000)
        .await
        .unwrap()
        .into_iter()
        .filter_map(|item| match item {
            EventItem::Event(event) if event.details.kind == EventKind::JobEnded => {
                event.details.job
            }
            _ => None,
        })
        .collect();
    assert_eq!(ended.len(), 1);
    assert!(ended[0].run.ends_with("/team/repo/actions/runs/6"));
    let stopped: Vec<_> = (ended[0].stopped.iter())
        .map(|process| (process.pid, process.command.as_str()))
        .collect();
    assert_eq!(stopped, [(pid, "sleep 600")]);
    assert_eq!(logged_stops(manager), [JobStop::Overrun]);
}

/// A heavy agent running in a new workspace of `repository`.
async fn heavy_agent(manager: &Manager, repository: &RepositoryId) -> StartedExecution {
    let workspace = manager
        .create_workspace(repository, "agent".into(), None, None, None)
        .await
        .unwrap();
    let execution = manager
        .agents
        .begin_execution(
            manager,
            workspace.id.as_str(),
            None,
            ExecutionKind::Command,
            None,
        )
        .await
        .unwrap();
    let agent = shoal_protocol::executions::TrackedAgent::named("claude");
    (manager.agents.track_agent(
        manager,
        &execution.plan.id,
        &agent,
        &workspace.id,
        &workspace.name,
        true,
        std::time::Duration::ZERO,
    ))
    .await;
    execution
}

/// Load protection cancels the running job on the forge as its runner, then
/// ends it before any agent, records why, and reruns it on the forge once
/// its run has finished, while readings have yet to recover.
#[tokio::test]
async fn load_protection_ends_a_job_before_agents_and_reruns_it_once_its_run_finished() {
    use crate::daemon::events::EventItem;
    use crate::daemon::overload::Pressure;
    use crate::protocol::{self, Control, ExecutionEvent};
    let fixture = fixture(vec![job("h6", 8, 6)], "").await;
    let manager = &fixture.manager;
    admit_and_claim(manager).await;
    let claimed = manager.runner.claimed_jobs().await.unwrap();
    let ConnectedRunner {
        mut connection,
        executing,
    } = connect_runner(manager, &claimed[0].workspace_id, false).await;
    let agent = heavy_agent(manager, &fixture.repository).await;

    let stopping = tokio::spawn({
        let manager = manager.clone();
        async move {
            manager
                .agents
                .stop_for_overload(&*manager, Pressure::Memory)
                .await
        }
    });
    let control: Control = read(&mut connection).await;
    assert!(matches!(&control, Control::Stop { .. }), "{control:?}");
    let received = fixture.server.received();
    let cancel = (received.iter())
        .find(|request| request.line.ends_with("RunnerService/UpdateTask"))
        .expect("the job is cancelled before its runner stops");
    assert_eq!(cancel.header("x-runner-uuid"), Some("runner-uuid"));
    let event = ExecutionEvent::Finished {
        exit_code: 143,
        memory: None,
    };
    protocol::write(connection.get_mut(), &event).await.unwrap();
    let control: Control = read(&mut connection).await;
    assert!(matches!(control, Control::Finished { .. }), "{control:?}");
    executing.await.unwrap().unwrap();
    assert!(stopping.await.unwrap());
    assert!(!*agent.stop.borrow(), "agents stop only once no job runs");
    assert_eq!(
        logged_stops(manager),
        [JobStop::Overload(Pressure::Memory.reason())]
    );
    let reasons: Vec<_> = manager
        .space_events(None, 1000)
        .await
        .unwrap()
        .into_iter()
        .filter_map(|item| match item {
            EventItem::Event(event) => event.details.job.map(|job| job.reason),
            _ => None,
        })
        .collect();
    assert_eq!(reasons, ["stopped for critical memory pressure"]);

    assert!(manager.agents.overload_recovery().published().is_none());
    manager.runner.rerun_stopped_jobs(&**manager).await.unwrap();
    manager.runner.rerun_stopped_jobs(&**manager).await.unwrap();
    assert_eq!(reruns(&fixture, 8), 1);
    // Load protection's reruns tell nobody.
    assert!(alerts(manager, AlertKind::JobsRerun).await.is_empty());
}

/// A stop another lifecycle operation of the job's workspace refuses
/// interrupts nothing, so it leaves the job no rerun of protection's.
#[tokio::test]
async fn a_refused_load_stop_leaves_a_job_no_rerun() {
    let fixture = fixture(vec![job("h6", 8, 6)], "").await;
    let manager = &fixture.manager;
    admit_and_claim(manager).await;
    let claimed = manager.runner.claimed_jobs().await.unwrap();
    let workspace = claimed[0].workspace_id.clone();
    let _runner = connect_runner(manager, &workspace, false).await;
    manager
        .spaces
        .reserve_lifecycle(&workspace, WorkspaceState::Removing)
        .await
        .unwrap();

    let memory = crate::daemon::overload::Pressure::Memory.reason();
    (manager
        .runner
        .stop_job_to_rerun(&**manager, &workspace, JobStop::Overload(memory)))
    .await
    .unwrap_err();
    let reruns: i64 = manager
        .store
        .run(|db| Ok(db.query_row("SELECT COUNT(*) FROM ci_job_reruns", [], |row| row.get(0))?))
        .await
        .unwrap();
    assert_eq!(reruns, 0);
}

/// A job whose runner's execution ended, while its workspace's removal
/// waits, holds nothing, so load stops an agent instead.
#[tokio::test]
async fn load_protection_passes_over_a_job_whose_runner_ended() {
    let fixture = fixture(vec![job("h6", 8, 6)], "").await;
    let manager = &fixture.manager;
    admit_and_claim(manager).await;
    let claimed = manager.runner.claimed_jobs().await.unwrap();
    let execution = claimed[0]
        .execution_id
        .clone()
        .expect("the claim runs its runner");
    manager
        .agents
        .forget_execution(&**manager, &execution)
        .await
        .unwrap();
    let agent = heavy_agent(manager, &fixture.repository).await;

    let memory = crate::daemon::overload::Pressure::Memory;
    assert!(manager.agents.stop_for_overload(&**manager, memory).await);
    assert!(*agent.stop.borrow());
    assert!(logged_stops(manager).is_empty());
}

#[tokio::test]
async fn a_job_admitted_as_the_system_shuts_down_waits_in_the_queue() {
    let fixture = fixture(vec![job("h1", 4, 1)], "").await;
    let manager = &fixture.manager;
    refresh_waiting_jobs(manager).await;
    manager.agents.dispatch_queue(&**manager).await.unwrap();
    let admitted = admitted_jobs(manager).await;
    assert_eq!(admitted.len(), 1);
    manager.agents.agent_queue().suspend(true).await;
    claim_job(manager, &admitted[0]).await;
    let waiting = job_entries(manager).await;
    assert_eq!(waiting.len(), 1);
    assert_eq!(waiting[0].state, QueueState::Queued);
    assert!(manager.runner.claimed_jobs().await.unwrap().is_empty());
    assert!(
        manager
            .spaces
            .workspace(&manager.named("ci-rust-4").await)
            .await
            .is_err()
    );
    manager.agents.agent_queue().suspend(false).await;
}

#[tokio::test]
async fn a_claim_a_restart_cut_short_gives_its_permit_back_before_the_job_is_claimed_again() {
    let fixture = fixture(
        vec![job("h1", 4, 1)],
        "permit = \"builds\"\n[resources.builds]\n",
    )
    .await;
    let manager = &fixture.manager;
    refresh_waiting_jobs(manager).await;
    manager.agents.dispatch_queue(&**manager).await.unwrap();
    let admitted = admitted_jobs(manager).await;
    assert_eq!(admitted.len(), 1);
    let entry = &admitted[0];
    let job = entry.job.as_deref().unwrap();
    // As if the daemon stopped once the claim's workspace held the permit.
    let forge = crate::forge::ForgeRepo::resolve(
        &format!("http://{}/team/repo", fixture.server.host),
        &manager.forge_hosts(),
    )
    .await
    .unwrap();
    let workspace = (manager.runner)
        .create_claimed_workspace(&**manager, entry, &forge, job)
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
        .leases
        .acquire_resource(&**manager, &SpaceSelector::id(&workspace.id), request, None)
        .await
        .unwrap();

    // The cut-short claim's workspace goes with its permit, and the job,
    // still admitted, is claimed again.
    finish_ended_jobs(manager).await;
    assert!(manager.spaces.workspace_by_id(&workspace.id).await.is_err());
    claim_job(manager, entry).await;
    let claimed = manager.runner.claimed_jobs().await.unwrap();
    assert_eq!(claimed.len(), 1);
    assert_ne!(claimed[0].workspace_id, workspace.id);
    let leases = manager
        .leases
        .list_resources(
            &**manager,
            Some(&SpaceSelector::id(&claimed[0].workspace_id)),
        )
        .await
        .unwrap();
    assert_eq!(leases.len(), 1);
    LAUNCHED.lock().unwrap().remove(&claimed[0].workspace_id);
}

/// The job workspace of the fixture's one job as a restart leaves it when
/// it comes before the claim checked the workspace out, recorded and claimed
/// but still preparing.
async fn workspace_a_restart_caught_preparing(fixture: &Fixture) -> Workspace {
    let manager = &fixture.manager;
    refresh_waiting_jobs(manager).await;
    manager.agents.dispatch_queue(&**manager).await.unwrap();
    let admitted = admitted_jobs(manager).await;
    let (entry, job) = (&admitted[0], admitted[0].job.as_deref().unwrap());
    let forge = crate::forge::ForgeRepo::resolve(
        &format!("http://{}/team/repo", fixture.server.host),
        &manager.forge_hosts(),
    )
    .await
    .unwrap();
    let recorded = (manager.runner)
        .record_claimed_workspace(&**manager, entry, &forge, job)
        .await
        .unwrap();
    let id = recorded.id().clone();
    drop(recorded);
    let workspace = manager.spaces.workspace_by_id(&id).await.unwrap();
    assert_eq!(workspace.state, crate::state::WorkspaceState::Preparing);
    workspace
}

/// Whether the cut-short claim of `workspace` went with it.
async fn removed_with_its_claim(manager: &Manager, workspace: &Workspace) -> bool {
    finish_ended_jobs(manager).await;
    manager.spaces.workspace_by_id(&workspace.id).await.is_err()
        && manager.runner.claimed_jobs().await.unwrap().is_empty()
}

#[tokio::test]
async fn a_jobs_workspace_a_restart_caught_before_its_checkout_is_removed() {
    let fixture = fixture(vec![job("h1", 4, 1)], "").await;
    let workspace = workspace_a_restart_caught_preparing(&fixture).await;
    assert!(removed_with_its_claim(&fixture.manager, &workspace).await);
}

/// Add the worktree of `workspace` as Git does before Shoal records it.
async fn add_worktree(fixture: &Fixture, workspace: &Workspace) {
    let manager = &fixture.manager;
    let checkout = manager
        .repositories
        .repository(&fixture.repository)
        .await
        .unwrap()
        .path;
    let path = workspace.path.to_str().unwrap();
    git(
        &checkout,
        &["worktree", "add", "--detach", path, &fixture.head],
    );
}

#[tokio::test]
async fn a_jobs_workspace_a_restart_caught_adding_its_worktree_is_removed() {
    let fixture = fixture(vec![job("h1", 4, 1)], "").await;
    let workspace = workspace_a_restart_caught_preparing(&fixture).await;
    add_worktree(&fixture, &workspace).await;
    assert!(removed_with_its_claim(&fixture.manager, &workspace).await);
    assert!(!workspace.path.exists());
}

#[tokio::test]
async fn a_jobs_workspace_a_restart_caught_seeding_is_removed() {
    let fixture = fixture(vec![job("h1", 4, 1)], "").await;
    let workspace = workspace_a_restart_caught_preparing(&fixture).await;
    // As if the daemon stopped during the seed clone, once Shoal had
    // recorded the worktree Git added.
    add_worktree(&fixture, &workspace).await;
    let manager = &fixture.manager;
    manager
        .spaces
        .record_worktree_identity(&workspace)
        .await
        .unwrap();
    assert!(removed_with_its_claim(manager, &workspace).await);
    assert!(!workspace.path.exists());
}

/// The workspace of the fixture's one job as a removal leaves it that
/// stopped once Git deleted the worktree's `.git` link, after a `worktree
/// prune` deleted the admin directory no link named: what Git did not
/// delete, failed with that removal's error.
async fn workspace_whose_removal_stopped_after_its_admin_directory_went(
    fixture: &Fixture,
) -> Workspace {
    let workspace = workspace_a_restart_caught_preparing(fixture).await;
    add_worktree(fixture, &workspace).await;
    let manager = &fixture.manager;
    manager
        .spaces
        .record_worktree_identity(&workspace)
        .await
        .unwrap();
    manager
        .spaces
        .record_removal_directory(&workspace)
        .await
        .unwrap();
    std::fs::create_dir(workspace.path.join("target")).unwrap();
    std::fs::write(workspace.path.join("target/built"), "").unwrap();
    std::fs::remove_file(workspace.path.join(".git")).unwrap();
    let checkout = manager
        .repositories
        .repository(&fixture.repository)
        .await
        .unwrap()
        .path;
    git(&checkout, &["worktree", "prune"]);
    let error = format!("{STOPPED_REMOVAL} when its work ended early");
    (manager
        .spaces
        .set_state(&workspace.id, WorkspaceState::Failed, Some(error)))
    .await
    .unwrap();
    workspace
}

#[tokio::test]
async fn a_jobs_workspace_whose_removal_stopped_after_its_admin_directory_went_is_removed() {
    let fixture = fixture(vec![job("h1", 4, 1)], "").await;
    let workspace = workspace_whose_removal_stopped_after_its_admin_directory_went(&fixture).await;
    assert!(removed_with_its_claim(&fixture.manager, &workspace).await);
    assert!(!workspace.path.exists());
}

/// A directory that replaced such a leftover is not the one the removal
/// began deleting, so it stays with what it holds.
#[tokio::test]
async fn a_directory_that_replaced_a_stopped_removals_leftover_stays() {
    let fixture = fixture(vec![job("h1", 4, 1)], "").await;
    let workspace = workspace_whose_removal_stopped_after_its_admin_directory_went(&fixture).await;
    std::fs::remove_dir_all(&workspace.path).unwrap();
    std::fs::create_dir(&workspace.path).unwrap();
    std::fs::write(workspace.path.join("kept"), "").unwrap();
    assert!(!removed_with_its_claim(&fixture.manager, &workspace).await);
    assert!(workspace.path.join("kept").exists());
}

/// Nor is a symbolic link that replaced it, whose target stays whole.
#[tokio::test]
async fn a_symbolic_link_that_replaced_a_stopped_removals_leftover_stays() {
    let fixture = fixture(vec![job("h1", 4, 1)], "").await;
    let workspace = workspace_whose_removal_stopped_after_its_admin_directory_went(&fixture).await;
    let target = workspace.path.with_file_name("elsewhere");
    std::fs::create_dir(&target).unwrap();
    std::fs::write(target.join("kept"), "").unwrap();
    std::fs::remove_dir_all(&workspace.path).unwrap();
    std::os::unix::fs::symlink(&target, &workspace.path).unwrap();
    assert!(!removed_with_its_claim(&fixture.manager, &workspace).await);
    assert!(target.join("kept").exists());
}

#[tokio::test]
async fn an_unrecorded_worktree_another_workspace_marked_stays_with_its_job() {
    let fixture = fixture(vec![job("h1", 4, 1)], "").await;
    let workspace = workspace_a_restart_caught_preparing(&fixture).await;
    add_worktree(&fixture, &workspace).await;
    let admin_dir = git(&workspace.path, &["rev-parse", "--absolute-git-dir"]);
    shoal_daemon_base::ownership::mark_owner(admin_dir.trim_end().as_ref(), "another").unwrap();
    assert!(!removed_with_its_claim(&fixture.manager, &workspace).await);
    assert!(workspace.path.join(".git").exists());
}

#[tokio::test]
async fn removing_a_running_jobs_workspace_cancels_its_run_first() {
    let fixture = fixture(vec![job("h3", 7, 3)], "").await;
    let manager = &fixture.manager;
    admit_and_claim(manager).await;
    let claimed = manager.runner.claimed_jobs().await.unwrap();
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
    assert_eq!(logged_stops(manager), [JobStop::Removal]);
    assert!(manager.runner.claimed_jobs().await.unwrap().is_empty());
}

#[tokio::test]
async fn removing_a_job_whose_execution_is_not_recorded_yet_cancels_its_run() {
    let fixture = fixture(vec![job("h3", 7, 3)], "").await;
    let manager = &fixture.manager;
    admit_and_claim(manager).await;
    let claimed = manager.runner.claimed_jobs().await.unwrap();
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
async fn a_runner_gone_while_the_daemon_was_away_gives_its_job_up_once_nothing_of_it_runs() {
    use crate::process::identity::{Identity, capture};
    let fixture = fixture(vec![job("h1", 4, 1)], "").await;
    let manager = &fixture.manager;
    admit_and_claim(manager).await;
    let claimed = manager.runner.claimed_jobs().await.unwrap();
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
    finish_ended_jobs(manager).await;
    assert_eq!(manager.runner.claimed_jobs().await.unwrap().len(), 1);

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
    finish_ended_jobs(manager).await;
    assert_eq!(manager.runner.claimed_jobs().await.unwrap().len(), 1);
    survivor.wait().await.unwrap();

    // Once nothing of it runs, the job ends here.
    finish_ended_jobs(manager).await;
    assert!(manager.runner.claimed_jobs().await.unwrap().is_empty());
    assert!(
        manager
            .spaces
            .workspace_by_id(&claimed[0].workspace_id)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn removing_a_repository_deletes_its_runner_on_the_forge() {
    let (root, manager) = manager().await;
    let path = repository(root.path(), "repo");
    let repo = (manager
        .repositories
        .register(path.to_str().unwrap().into(), None, None))
    .await
    .unwrap();
    let deleted = Arc::new(Mutex::new(Vec::new()));
    let seen = deleted.clone();
    let server = Server::start(move |request| {
        let body = match request.line.as_str() {
            "GET /api/v1/version" => json!({"version": "17.0.1+gitea-1.22.0"}),
            "POST /api/v1/repos/team/repo/actions/runners" => {
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
    crate::forge::hosts::learn(&server.host, ForgeKind::Forgejo);
    let origin = format!("http://{}/team/repo", server.host);
    git(&path, &["remote", "add", "origin", &origin]);
    *manager.runner.jobs().path.lock().unwrap() =
        Some(runner_program(root.path(), "forgejo-runner"));
    configure(
        &manager,
        &format!(
            "[machine.forges.\"{}\"]\ntype = \"forgejo\"\nuser_token = \"user\"\n[runner]\nenabled = true\n",
            server.host
        ),
    )
    .await;
    manager.runner.reconcile_runners(&*manager).await.unwrap();

    manager
        .repositories
        .remove(&*manager, &repo.id)
        .await
        .unwrap();

    assert_eq!(
        *deleted.lock().unwrap(),
        ["DELETE /api/v1/repos/team/repo/actions/runners/5"]
    );
}

/// Bring the job entries in line with the jobs the forge lists as waiting.
pub(super) async fn refresh_waiting_jobs(manager: &Manager) {
    manager.runner.refresh_waiting_jobs(manager).await.unwrap();
}

/// The job entries admission admitted.
pub(super) async fn admitted_jobs(manager: &Manager) -> Vec<QueueEntry> {
    manager.runner.admitted_jobs(manager).await.unwrap()
}

/// Claim the admitted job `entry`.
pub(super) async fn claim_job(manager: &Manager, entry: &QueueEntry) {
    manager.runner.claim_job(manager, entry).await;
}

/// End the claimed jobs whose runner ended.
pub(super) async fn finish_ended_jobs(manager: &Manager) {
    manager.runner.finish_ended_jobs(manager).await.unwrap();
}

mod single_use;
mod superseded;
mod taken;
mod triggers;
mod waiting;
