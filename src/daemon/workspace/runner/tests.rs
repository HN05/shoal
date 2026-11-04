use std::{
    collections::HashMap,
    ffi::OsString,
    os::unix::fs::PermissionsExt as _,
    sync::{Arc, LazyLock, Mutex},
};

use serde_json::{Value, json};

use crate::{
    client::resources::ResourceRequest,
    daemon::{
        notifications::NotificationKind,
        workspace::{ExecutionKind, Manager, StartedExecution},
    },
    forge::{backends::ForgeKind, test_support::Server},
    model::{QueueEntry, Workspace},
    state::{QueueKind, QueueState},
    test_support::{configure, git, manager, repository},
};

/// A test launch's command and the execution standing in for it.
type Launched = (Vec<OsString>, StartedExecution);

/// Each test launch, by workspace.
static LAUNCHED: LazyLock<Mutex<HashMap<String, Launched>>> = LazyLock::new(Default::default);

/// Record `argv` as run in `workspace` by an execution the test ends.
pub(super) async fn launch(
    manager: &Manager,
    workspace: &Workspace,
    argv: Vec<OsString>,
) -> anyhow::Result<String> {
    let started = manager
        .begin_execution(&workspace.id, None, ExecutionKind::Command, None)
        .await?;
    let id = started.plan.id.clone();
    LAUNCHED
        .lock()
        .unwrap()
        .insert(workspace.id.clone(), (argv, started));
    Ok(id)
}

/// A repository whose origin is a Forgejo that lists `jobs` as waiting:
/// run 1 pushed `main` at its head and run 2 is for PR 12, whose head is
/// `pull_head`.
struct Fixture {
    _root: tempfile::TempDir,
    manager: Arc<Manager>,
    repository: String,
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

fn run(id: u64, event: &str, commit: &str, payload: Value) -> Value {
    json!({
        "id": id, "index_in_repo": id, "event": event, "commit_sha": commit,
        "workflow_id": "ci.yml", "event_payload": payload.to_string(),
    })
}

fn job(handle: &str, id: u64, run: u64) -> Value {
    json!({"handle": handle, "id": id, "run_id": run, "name": "rust", "runs_on": ["shoal"], "status": "waiting"})
}

/// Queue the forge's waiting jobs, admit them and claim each admitted one.
async fn admit_and_claim(manager: &Manager) -> Vec<QueueEntry> {
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
    let workspace = manager.workspace(&claimed[0].workspace_id).await.unwrap();
    assert_eq!(workspace.name, "ci-rust-4");
    assert_eq!(workspace.repository_id, fixture.repository);
    assert_eq!(workspace.branch, None);
    assert_eq!(workspace.at.as_ref().unwrap().commit, fixture.head);
    assert!(workspace.disposable && workspace.setup_skipped);

    let (argv, started) = LAUNCHED.lock().unwrap().remove(&workspace.id).unwrap();
    let argv: Vec<_> = argv.iter().map(|arg| arg.to_str().unwrap()).collect();
    let directory = manager.job_directory(&claimed[0].id);
    let token = directory.join("token");
    assert_eq!(
        argv[..5],
        [
            "forgejo-runner",
            "--config",
            directory.join("config.yaml").to_str().unwrap(),
            "one-job",
            "--url"
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
    assert_eq!(
        std::fs::metadata(&token).unwrap().permissions().mode() & 0o777,
        0o600
    );

    // A running job keeps its workspace.
    manager.finish_ended_jobs().await.unwrap();
    assert!(manager.workspace(&workspace.id).await.is_ok());

    manager
        .finish_execution(started.plan.id.clone(), ExecutionKind::Command, Some(0))
        .await
        .unwrap();
    manager.finish_ended_jobs().await.unwrap();
    assert!(manager.workspace(&workspace.id).await.is_err());
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
        .acquire_resource(&other.id, request, None)
        .await
        .unwrap();

    admit_and_claim(manager).await;
    let waiting = job_entries(manager).await;
    assert_eq!(waiting.len(), 1);
    assert_eq!(waiting[0].state, QueueState::Queued);
    assert!(manager.claimed_jobs().await.unwrap().is_empty());
    assert!(manager.workspace("ci-rust-4").await.is_err());
    // It waits a while before admission tries it again.
    manager.dispatch_queue().await.unwrap();
    assert!(manager.admitted_jobs().await.unwrap().is_empty());

    // Once the pool has a permit, the job holds it while it runs.
    manager.runners.forget_permits_busy();
    manager
        .release_resource(&other.id, "builds".into(), "held".into())
        .await
        .unwrap();
    admit_and_claim(manager).await;
    let claimed = manager.claimed_jobs().await.unwrap();
    assert_eq!(claimed.len(), 1);
    let leases = manager
        .list_resources(Some(&claimed[0].workspace_id))
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
        kept[0].execution_id.as_deref(),
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
    assert!(manager.workspace(&claimed[0].workspace_id).await.is_err());
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
    assert_eq!(named.workspace_id.as_deref(), Some(topic.id.as_str()));
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
            &claimed[0].workspace_id,
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
    assert!(manager.workspace("ci-rust-4").await.is_err());
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
        .acquire_resource(&workspace.id, request, None)
        .await
        .unwrap();

    // The cut-short claim's workspace goes with its permit, and the job,
    // still admitted, is claimed again.
    manager.finish_ended_jobs().await.unwrap();
    assert!(manager.workspace(&workspace.id).await.is_err());
    manager.claim_job(entry).await;
    let claimed = manager.claimed_jobs().await.unwrap();
    assert_eq!(claimed.len(), 1);
    assert_ne!(claimed[0].workspace_id, workspace.id);
    let leases = manager
        .list_resources(Some(&claimed[0].workspace_id))
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
