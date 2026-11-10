//! The runner against a scripted Forgejo, with a `forgejo-runner` that
//! records how it was run and waits for the test to end it.
// These tests never restart the daemon.
#[allow(dead_code)]
#[path = "support/daemon.rs"]
mod daemon_fixture;
use daemon_fixture::DaemonGuard;

#[path = "support/git.rs"]
mod git_fixture;
use git_fixture::{git, init_repo};

mod support;

use std::{fs, path::Path, process::Output};

use serde_json::{Value, json};
use shoal_forge::test_support::Server;
use support::{cli, install_test_script, wait::wait_until};

/// A Forgejo whose `team/repo` holds job `h1` waiting in run 1, a push to
/// `main` at `head`, and registers runner 5; cancelling the run creates
/// `finish`, which ends the recording runner, and the job waits no longer.
fn forgejo(head: String, finish: std::path::PathBuf) -> Server {
    Server::start(move |request| {
        let line = request.line.as_str();
        let run = json!({
            "id": 1, "index_in_repo": 1, "event": "push", "commit_sha": head,
            "workflow_id": "checks.yml", "status": "running",
            "event_payload": json!({"ref": "refs/heads/main"}).to_string(),
        });
        let body = match line {
            "GET /api/v1/repos/team/repo/actions/runs?run_number=1" => {
                json!({"workflow_runs": [run]})
            }
            "POST /api/v1/repos/team/repo/actions/runs/1/cancel" => {
                fs::write(&finish, "").unwrap();
                return (204, Vec::new(), Vec::new());
            }
            "GET /api/v1/version" => json!({"version": "17.0.1+gitea-1.22.0"}),
            "POST /api/v1/repos/team/repo/actions/runners" => {
                json!({"id": 5, "uuid": "runner-uuid", "token": "runner-token"})
            }
            "DELETE /api/v1/repos/team/repo/actions/runners/5" => {
                return (204, Vec::new(), Vec::new());
            }
            "GET /api/v1/repos/team/repo/actions/runs/1" => run,
            line if line.starts_with("GET /api/v1/repos/team/repo/actions/runners/jobs?")
                && finish.exists() =>
            {
                json!([])
            }
            line if line.starts_with("GET /api/v1/repos/team/repo/actions/runners/jobs?") => {
                json!([
                    {"handle": "h1", "id": 4, "run_id": 1, "name": "rust", "runs_on": ["shoal"], "status": "waiting"},
                ])
            }
            _ => return (404, Vec::new(), b"{}".to_vec()),
        };
        (200, Vec::new(), body.to_string().into())
    })
}

fn run(root: &Path, args: &[&str]) -> Output {
    let output = cli(root).args(args).output().unwrap();
    assert!(
        output.status.success(),
        "shoal {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn workspaces(root: &Path) -> Vec<Value> {
    serde_json::from_slice(&run(root, &["--json", "list"]).stdout).unwrap()
}

#[test]
fn the_runner_runs_a_waiting_job_in_its_own_workspace_and_deletes_itself_when_turned_off() {
    let root = tempfile::tempdir_in("/tmp").unwrap();
    let root = &fs::canonicalize(root.path()).unwrap();
    let repo = init_repo(root, "repo", &[("README", "runner\n")]);
    let head = git(&repo, &["rev-parse", "HEAD"]).trim().to_owned();
    let finish = root.join("finish");
    let server = forgejo(head.clone(), finish.clone());
    let origin = format!("http://{}/team/repo", server.host);
    git(&repo, &["remote", "add", "origin", &origin]);
    git(&repo, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
    git(
        &repo,
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/main",
        ],
    );
    let calls = root.join("runner-calls");
    install_test_script(
        root.join("bin/forgejo-runner"),
        format!(
            "#!/bin/sh\nmkdir -p '{calls}'\nprintf '%s\\n' \"$@\" > '{calls}/args'\nprintf '%s\\n' \"$SHOAL_WORKSPACE_ID\" > '{calls}/workspace'\nwhile [ ! -e '{finish}' ]; do sleep 0.1; done\n",
            calls = calls.display(),
            finish = finish.display(),
        ),
    );
    let config = root.join(".config/shoal/config.toml");
    fs::create_dir_all(config.parent().unwrap()).unwrap();
    fs::write(
        &config,
        format!(
            "[cleanup]\ngrace_minutes = 0\n[machine]\nmax_jobs = 1\n[machine.forges.\"{}\"]\ntype = \"forgejo\"\ntoken = \"user\"\n",
            server.host
        ),
    )
    .unwrap();
    let _daemon = DaemonGuard::start(root, &mut cli(root));
    run(root, &["repo", "add", repo.to_str().unwrap()]);
    run(
        root,
        &["config", "set", "runner.enabled", "true", "--repo", "repo"],
    );

    // The job runs the forge's runner in a workspace at its commit.
    // The stub writes its arguments first, and the ID line last.
    let workspace_id = support::wait::try_wait_for(|| {
        let written = fs::read_to_string(calls.join("workspace")).ok()?;
        written.ends_with('\n').then(|| written.trim().to_owned())
    });
    let Some(workspace_id) = workspace_id else {
        let alerts = cli(root).args(["--json", "alerts"]).output().unwrap();
        let queue = cli(root).args(["--json", "queue"]).output().unwrap();
        eprintln!("queue: {}", String::from_utf8_lossy(&queue.stdout));
        eprintln!("list: {:?}", workspaces(root));
        let requests: Vec<String> = server.received().into_iter().map(|r| r.line).collect();
        panic!(
            "the stub runner did not start; the forge saw {requests:?}; alerts: {}",
            String::from_utf8_lossy(&alerts.stdout)
        );
    };
    let args = fs::read_to_string(calls.join("args")).unwrap();
    let args: Vec<&str> = args.lines().collect();
    assert!(
        args.windows(2).any(|pair| pair == ["--handle", "h1"]),
        "{args:?}"
    );
    assert!(
        args.windows(2)
            .any(|pair| pair == ["--uuid", "runner-uuid"]),
        "{args:?}"
    );
    // The move stops what the daemon records running, which the stub's
    // file does not prove.
    let job = support::wait::wait_for("the job's runner to be recorded", || {
        workspaces(root)
            .into_iter()
            .find(|workspace| workspace["id"] == workspace_id.as_str())
            .filter(|workspace| workspace["running"] == true)
    });
    assert_eq!(job["name"], "ci-rust-4");
    assert_eq!(job["at"]["commit"], head.as_str());
    assert_eq!(job["job"]["job"]["handle"], "h1");

    // Moving the root cancels the job's run, and moves once its runner has
    // ended and its workspace is gone.
    let new_root = root.join("moved");
    let moved = run(root, &["move", "root", new_root.to_str().unwrap(), "--yes"]);
    // The job's workspace leaves nothing to resume.
    let warnings = String::from_utf8_lossy(&moved.stderr);
    assert!(!warnings.contains("warning"), "{warnings}");
    assert!(
        server
            .received()
            .iter()
            .any(|request| request.line == "POST /api/v1/repos/team/repo/actions/runs/1/cancel")
    );
    assert!(
        workspaces(root)
            .iter()
            .all(|workspace| workspace["id"] != workspace_id.as_str())
    );

    run(
        root,
        &["config", "set", "runner.enabled", "false", "--repo", "repo"],
    );
    wait_until("the runner's deletion", || {
        server
            .received()
            .iter()
            .any(|request| request.line == "DELETE /api/v1/repos/team/repo/actions/runners/5")
    });
}
