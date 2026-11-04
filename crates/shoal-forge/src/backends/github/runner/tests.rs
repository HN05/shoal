use serde_json::json;

use super::*;
use crate::{
    backends::ForgeKind,
    test_support::{Server, agent_on, repo_on},
};

fn job(id: u64, run: u64, status: &str, labels: &[&str]) -> Value {
    json!({"id": id, "run_id": run, "name": "rust", "status": status, "labels": labels})
}

#[tokio::test]
async fn queued_jobs_of_active_runs_wait_when_the_runner_has_their_labels() {
    let server = Server::start(|request| {
        let body = match request.line.as_str() {
            "GET /api/v3/repos/team/repo/actions/runs?status=queued&per_page=100" => {
                json!({"workflow_runs": [{"id": 1}]})
            }
            "GET /api/v3/repos/team/repo/actions/runs?status=in_progress&per_page=100" => {
                json!({"workflow_runs": [{"id": 2}]})
            }
            "GET /api/v3/repos/team/repo/actions/runs/1/jobs?filter=latest&per_page=100" => {
                json!({"jobs": [
                    job(11, 1, "queued", &["Shoal"]),
                    job(12, 1, "queued", &["shoal", "gpu"]),
                ]})
            }
            "GET /api/v3/repos/team/repo/actions/runs/2/jobs?filter=latest&per_page=100" => {
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
    let labels = ["shoal".to_owned(), "shoal-macos-arm64".to_owned()];
    let jobs = github.waiting_jobs(&account, &labels).await.unwrap();
    assert_eq!(
        jobs,
        [
            WaitingJob {
                handle: "9".into(),
                id: 9,
                run: 2,
                name: "rust".into(),
                labels: vec!["shoal-macos-arm64".into()],
            },
            WaitingJob {
                handle: "11".into(),
                id: 11,
                run: 1,
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
    assert_eq!(pull.trigger, Trigger::Pull { number: 12 });
    assert_eq!(pull.reference.as_deref(), Some("refs/pull/12/head"));

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

#[tokio::test]
async fn a_runner_is_busy_until_it_goes_and_names_the_job_it_took() {
    let server = Server::start(|request| {
        let body = match request.line.as_str() {
            "GET /api/v3/repos/team/repo/actions/runners/77" => json!({"id": 77, "busy": true}),
            "GET /api/v3/repos/team/repo/actions/runs/2/jobs?filter=latest&per_page=100" => {
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
    assert_eq!(github.runner_busy(&account, 77).await.unwrap(), Some(true));
    assert_eq!(github.runner_busy(&account, 78).await.unwrap(), None);
    let job = github.runner_job(&account, 2, 77).await.unwrap().unwrap();
    assert_eq!((job.id, job.run, job.name.as_str()), (10, 2, "rust"));
    assert_eq!(github.runner_job(&account, 2, 78).await.unwrap(), None);
}
