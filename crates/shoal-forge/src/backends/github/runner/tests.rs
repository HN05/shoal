use std::{os::unix::fs::PermissionsExt as _, process::Command};

use serde_json::json;

use super::*;
use crate::{
    backends::ForgeKind,
    ci::runner::{TookRun, answer_took_run, took_run},
    test_support::{Server, agent_on, repo_on},
};

fn job(id: u64, run: u64, status: &str, labels: &[&str]) -> Value {
    json!({"id": id, "run_id": run, "name": "rust", "status": status, "labels": labels})
}

#[tokio::test]
async fn queued_jobs_of_active_runs_wait_when_the_runner_has_their_labels() {
    let server = Server::start(|request| {
        let body = match request.line.as_str() {
            "GET /api/v3/repos/team/repo/actions/runs?status=queued&per_page=100&page=1" => {
                json!({"workflow_runs": [{"id": 1}]})
            }
            "GET /api/v3/repos/team/repo/actions/runs?status=in_progress&per_page=100&page=1" => {
                json!({"workflow_runs": [{"id": 2}]})
            }
            "GET /api/v3/repos/team/repo/actions/runs/1/jobs?filter=latest&per_page=100&page=1" => {
                // A full page, so the next is read too.
                let gpu = (100..200).map(|id| job(id, 1, "queued", &["shoal", "gpu"]));
                json!({"jobs": gpu.collect::<Vec<_>>()})
            }
            "GET /api/v3/repos/team/repo/actions/runs/1/jobs?filter=latest&per_page=100&page=2" => {
                json!({"jobs": [job(11, 1, "queued", &["Shoal"])]})
            }
            "GET /api/v3/repos/team/repo/actions/runs/2/jobs?filter=latest&per_page=100&page=1" => {
                json!({"jobs": [
                    job(9, 2, "queued", &["shoal-macos-arm64"]),
                    job(10, 2, "in_progress", &["shoal"]),
                ]})
            }
            _ => return (404, Vec::new(), b"{}".to_vec()),
        };
        (200, Vec::new(), body.to_string().into())
    });
    let github = repo_on(&server, ForgeKind::GitHub, "team/repo");
    let account = agent_on(&server, "user");
    github.require_runner(&account).await.unwrap();
    let labels = ["shoal".to_owned(), "shoal-macos-arm64".to_owned()];
    let jobs = github.waiting_jobs(&account, &labels).await.unwrap();
    assert_eq!(
        jobs,
        [
            WaitingJob {
                handle: "9".into(),
                id: JobId::new(9),
                run: RunId::new(2),
                name: "rust".into(),
                labels: vec!["shoal-macos-arm64".into()],
            },
            WaitingJob {
                handle: "11".into(),
                id: JobId::new(11),
                run: RunId::new(1),
                name: "rust".into(),
                labels: vec!["Shoal".into()],
            },
        ]
    );
}

fn run(event: &str, pulls: Value, branch: Value) -> Value {
    json!({
        "id": 30, "event": event, "head_sha": "abc123", "head_branch": branch,
        "pull_requests": pulls, "path": ".github/workflows/ci.yml",
        "html_url": "https://github.com/team/repo/actions/runs/30",
    })
}

#[test]
fn a_run_names_its_pr_or_branch_and_a_forks_pr_only_its_commit() {
    let pull = job_run(&run("pull_request", json!([{"number": 12}]), json!("fix"))).unwrap();
    assert_eq!(pull.url, "https://github.com/team/repo/actions/runs/30");
    assert_eq!(pull.workflow.as_deref(), Some("ci.yml"));
    assert_eq!(pull.commit, "abc123");
    assert_eq!(
        pull.trigger,
        Trigger::Pull {
            number: PullNumber::new(12)
        }
    );
    assert_eq!(pull.reference.as_deref(), Some("refs/pull/12/head"));

    let target = run(
        "pull_request_target",
        json!([{"number": 12}]),
        json!("main"),
    );
    let target = job_run(&target).unwrap();
    assert_eq!(
        (target.trigger, target.reference),
        (Trigger::Other, None),
        "a run at the base branch's commit is no run of the PR's head"
    );
    let fork = job_run(&run("pull_request", json!([]), json!("fix"))).unwrap();
    assert_eq!((fork.trigger, fork.reference), (Trigger::Other, None));
    let push = job_run(&run("push", json!([]), json!("main"))).unwrap();
    assert_eq!(
        (push.trigger, push.reference),
        (Trigger::Branch("main".into()), None)
    );
    let schedule = job_run(&run("schedule", json!([]), json!("main"))).unwrap();
    assert_eq!(schedule.trigger, Trigger::Other);
}

/// An `actions/runner` installation under `root` that a job has run from
/// before, with what the runner writes into it.
fn installation(root: &Path) -> std::path::PathBuf {
    let install = root.join("actions-runner");
    for directory in ["bin", "externals", "_work", "_diag", "bin.2.300.0"] {
        std::fs::create_dir_all(install.join(directory)).unwrap();
    }
    for file in [
        "run.sh",
        "config.sh",
        ".runner",
        ".credentials",
        "bin/Runner.Listener",
    ] {
        std::fs::write(install.join(file), "").unwrap();
    }
    install.join("bin/Runner.Listener")
}

#[tokio::test]
async fn a_job_runs_a_just_in_time_runner_from_a_copy_of_the_installation() {
    let server = Server::start(|request| {
        if request.line == "POST /api/v3/repos/team/repo/actions/runners/generate-jitconfig" {
            let body = json!({"runner": {"id": 77}, "encoded_jit_config": "Y29uZmln"});
            return (201, Vec::new(), body.to_string().into());
        }
        (404, Vec::new(), b"{}".to_vec())
    });
    let root = tempfile::tempdir().unwrap();
    let program = installation(root.path());
    let work = root.path().join("job");
    let labels = ["shoal".to_owned(), "shoal-macos-arm64".to_owned()];
    let job = WaitingJob {
        handle: "11".into(),
        id: JobId::new(11),
        run: RunId::new(1),
        name: "rust".into(),
        labels: vec!["shoal".into()],
    };
    let launch = JobLaunch {
        runner: None,
        program: &program,
        name: "shoal-host-11",
        work_dir: &work,
        workspace: Path::new("/work/ci-rust-11"),
        labels: &labels,
        job: &job,
        env: &[("SHOAL_WORKSPACE_ID".into(), "id".into())],
    };
    let github = repo_on(&server, ForgeKind::GitHub, "team/repo");
    let account = agent_on(&server, "user");
    let command = github.job_command(&account, &launch).await.unwrap();

    let body: Value = serde_json::from_str(&server.received()[0].body).unwrap();
    assert_eq!(
        body,
        json!({
            "name": "shoal-host-11", "runner_group_id": 1,
            "labels": ["shoal", "shoal-macos-arm64"], "work_folder": "_work",
        })
    );
    let copy = work.join("runner");
    assert_eq!(command.argv, [copy.join("run.sh").into_os_string()]);
    assert_eq!(command.runner, Some(RunnerId::new(77)));
    let installed = command.installation.unwrap();
    assert_eq!(
        installed.source,
        std::fs::canonicalize(root.path().join("actions-runner")).unwrap()
    );
    assert_eq!(installed.destination, copy);
    assert_eq!(
        installed.entries,
        ["bin", "config.sh", "externals", "run.sh"]
    );
    assert_eq!(command.workspace_links, [copy.join("_work/repo/repo")]);
    let hook = work.join("job-started.sh");
    assert_eq!(
        command.env,
        [
            (
                "ACTIONS_RUNNER_INPUT_JITCONFIG".to_owned(),
                "Y29uZmln".to_owned()
            ),
            (
                "ACTIONS_RUNNER_HOOK_JOB_STARTED".to_owned(),
                hook.to_str().unwrap().to_owned()
            ),
        ]
    );
    assert_eq!(command.files[0].0, hook);
}

/// Run the job-started hook `script` in `work` as the runner would for a
/// job of `run`, returning whether it let the job go on and what it added
/// to the job's variables.
fn run_hook(work: &Path, script: &str, run: &str) -> (bool, String) {
    let path = work.join("job-started.sh");
    std::fs::write(&path, script).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    let env_file = work.join("github-env");
    std::fs::write(&env_file, "").unwrap();
    let status = Command::new("sh")
        .arg(&path)
        .env("GITHUB_RUN_ID", run)
        .env("GITHUB_ENV", &env_file)
        .output()
        .unwrap()
        .status;
    (status.success(), std::fs::read_to_string(env_file).unwrap())
}

#[test]
fn the_hook_goes_on_with_the_claimed_run_and_waits_for_the_daemon_with_another() {
    let root = tempfile::tempdir().unwrap();
    let work = root.path().join("it's work");
    std::fs::create_dir(&work).unwrap();
    let env = [("SHOAL_WORKSPACE_PATH".to_owned(), "/w/it's".to_owned())];
    let script = took_run_hook(&work, RunId::new(1), &job_env_lines(&env)).unwrap();

    let (went_on, variables) = run_hook(&work, &script, "1");
    assert!(went_on);
    assert_eq!(variables, "SHOAL_WORKSPACE_PATH=/w/it's\n");
    let went_on = TookRun {
        run: RunId::new(1),
        awaits: false,
    };
    assert_eq!(took_run(&work).unwrap(), Some(went_on));

    answer_took_run(&work, Err("the commit is gone".into())).unwrap();
    assert_eq!(took_run(&work).unwrap(), None, "an answered report is done");
    let (went_on, _) = run_hook(&work, &script, "2");
    assert!(!went_on, "a refusal fails the job");
    assert!(work.join("awaits").exists(), "a job of another run waits");

    std::fs::remove_file(work.join("refused")).unwrap();
    answer_took_run(&work, Ok(())).unwrap();
    let (went_on, _) = run_hook(&work, &script, "2");
    assert!(went_on);
}

#[tokio::test]
async fn a_runner_is_busy_until_it_goes_and_names_the_job_it_took() {
    let server = Server::start(|request| {
        let body = match request.line.as_str() {
            "GET /api/v3/repos/team/repo/actions/runners/77" => {
                json!({"id": 77, "busy": true, "labels": [{"name": "self-hosted"}, {"name": "shoal"}]})
            }
            "GET /api/v3/repos/team/repo/actions/runs/2/jobs?filter=latest&per_page=100&page=1" => {
                json!({"jobs": [
                    {"id": 9, "run_id": 2, "name": "docs", "runner_id": 76, "labels": ["shoal"]},
                    {"id": 10, "run_id": 2, "name": "rust", "runner_id": 77, "labels": ["shoal"]},
                ]})
            }
            _ => return (404, Vec::new(), b"{}".to_vec()),
        };
        (200, Vec::new(), body.to_string().into())
    });
    let github = repo_on(&server, ForgeKind::GitHub, "team/repo");
    let account = agent_on(&server, "user");
    let state = github
        .runner_state(&account, RunnerId::new(77))
        .await
        .unwrap()
        .unwrap();
    assert!(state.busy);
    assert_eq!(state.labels, ["self-hosted", "shoal"]);
    assert_eq!(
        github
            .runner_state(&account, RunnerId::new(78))
            .await
            .unwrap(),
        None
    );
    let job = github
        .runner_job(&account, RunId::new(2), RunnerId::new(77))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (job.id, job.run, job.name.as_str()),
        (JobId::new(10), RunId::new(2), "rust")
    );
    assert_eq!(
        github
            .runner_job(&account, RunId::new(2), RunnerId::new(78))
            .await
            .unwrap(),
        None
    );
}
