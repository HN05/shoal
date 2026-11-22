use serde_json::json;

use super::*;
use crate::ci::runs::Runs;
use shoal_forge_types::{
    ci::check_state::CheckState,
    items::{link::LinkKind, reference::ItemRef},
    numbers::{JobId, RunId},
};

fn filter(branch: Option<&str>, workflow: Option<&str>) -> RunFilter {
    RunFilter {
        branch: branch.map(Into::into),
        workflow: workflow.map(Into::into),
        commit: None,
        limit: 10,
    }
}

fn path(remote: &str, filter: &RunFilter) -> Result<String> {
    ForgeRepo::parse(remote)?.kind().runs_path(filter)
}

#[test]
fn each_forge_lists_a_branch_or_workflow_its_own_way() -> Result<()> {
    let github = "https://github.com/team/repo";
    assert_eq!(
        path(github, &filter(None, None))?,
        "actions/runs?per_page=10"
    );
    assert_eq!(
        path(github, &filter(Some("fix/a b"), Some("ci.yml")))?,
        "actions/workflows/ci.yml/runs?per_page=10&branch=fix%2Fa%20b"
    );
    let gitlab = "https://gitlab.com/team/repo";
    assert_eq!(
        path(gitlab, &filter(Some("topic"), None))?,
        "pipelines?per_page=10&ref=topic"
    );
    assert!(path(gitlab, &filter(None, Some("ci.yml"))).is_err());
    let forgejo = "https://codeberg.org/team/repo";
    assert_eq!(
        path(forgejo, &filter(Some("topic"), Some("deps.yml")))?,
        "actions/runs?limit=10&page=1&ref=refs%2Fheads%2Ftopic&workflow_id=deps.yml"
    );
    Ok(())
}

#[test]
fn each_forge_lists_the_runs_on_a_commit_its_own_way() -> Result<()> {
    let on_commit = RunFilter {
        commit: Some("abc".into()),
        ..filter(None, None)
    };
    for (remote, expected) in [
        (
            "https://github.com/team/repo",
            "actions/runs?per_page=10&head_sha=abc",
        ),
        (
            "https://gitlab.com/team/repo",
            "pipelines?per_page=10&sha=abc",
        ),
        (
            "https://codeberg.org/team/repo",
            "actions/runs?limit=10&page=1&head_sha=abc",
        ),
    ] {
        assert_eq!(path(remote, &on_commit)?, expected);
    }
    Ok(())
}

#[test]
fn the_newest_finished_run_skips_unfinished_and_held_runs() {
    let run = |id: u64, state: JobState, waiting: bool| Run {
        id: RunId::new(id),
        url: id.to_string(),
        branch: Some("main".into()),
        workflow: Some("ci.yml".into()),
        commit: None,
        pull: None,
        state,
        result: (state == JobState::Finished).then_some(CheckState::Success),
        waiting,
        finish: None,
    };
    let runs = vec![
        run(4, JobState::Running, false),
        run(3, JobState::Failed, true),
        run(2, JobState::Failed, false),
        run(1, JobState::Finished, false),
    ];
    assert_eq!(newest_finished(runs).map(|run| run.url), Some("2".into()));
    assert_eq!(
        newest_finished(vec![run(5, JobState::Running, false)]),
        None
    );
}

#[test]
fn listed_runs_are_read_like_one_run() -> Result<()> {
    let repo = ForgeRepo::parse("https://gitlab.com/team/repo")?;
    let value = json!([{"id": 12, "web_url": "https://gitlab.com/team/repo/-/pipelines/12",
            "ref": "topic", "tag": false, "status": "failed"}]);
    let listed = repo.kind().listed_runs(&value).unwrap_or_default();
    let run = repo.kind().run_item(&repo, &listed[0])?;
    assert_eq!(
        (run.branch.as_deref(), run.state),
        (Some("topic"), JobState::Failed)
    );
    Ok(())
}

#[test]
fn jobs_are_read_with_their_state_and_page() -> Result<()> {
    let github = ForgeRepo::parse("https://github.com/team/repo")?;
    let run = Run {
        id: RunId::new(12),
        url: "https://github.com/team/repo/actions/runs/12".into(),
        branch: None,
        workflow: None,
        commit: None,
        pull: None,
        state: JobState::Failed,
        result: Some(CheckState::Failure),
        waiting: false,
        finish: None,
    };
    let jobs = github.kind().run_jobs(
        &run,
        0,
        &json!({"jobs": [
            {"id": 34, "name": "rust", "status": "completed", "conclusion": "failure",
                "html_url": "https://github.com/team/repo/actions/runs/12/job/34"},
            {"id": 35, "name": "docs", "status": "in_progress", "conclusion": null},
            {"id": 36, "name": "lint", "status": "completed", "conclusion": "cancelled"}
        ]}),
    )?;
    assert_eq!(
        jobs,
        [
            RunJob {
                id: JobId::new(34),
                name: "rust".into(),
                state: JobState::Failed,
                result: Some(CheckState::Failure),
                url: Some("https://github.com/team/repo/actions/runs/12/job/34".into()),
            },
            RunJob {
                id: JobId::new(35),
                name: "docs".into(),
                state: JobState::Running,
                result: None,
                url: None,
            },
            RunJob {
                id: JobId::new(36),
                name: "lint".into(),
                state: JobState::Cancelled,
                result: Some(CheckState::Cancelled),
                url: None,
            },
        ]
    );
    let gitlab = ForgeRepo::parse("https://gitlab.com/team/repo")?;
    let jobs = gitlab.kind().run_jobs(
        &run,
        0,
        &json!([{"id": 1, "name": "rust", "status": "failed"},
                {"id": 2, "name": "docs", "status": "canceling"},
                {"id": 3, "name": "lint", "status": "canceled"}]),
    )?;
    assert_eq!(
        jobs.iter().map(|job| job.state).collect::<Vec<_>>(),
        [JobState::Failed, JobState::Running, JobState::Cancelled]
    );
    // Forgejo links a job by its index in the run.
    let forgejo = ForgeRepo::parse("https://codeberg.org/team/repo")?;
    let run = Run {
        id: RunId::new(9331),
        url: "https://codeberg.org/team/repo/actions/runs/2765".into(),
        ..run
    };
    let jobs = forgejo.kind().run_jobs(
        &run,
        100,
        &json!([{"id": 1, "name": "audit", "status": "success", "run_id": 9331},
                {"id": 2, "name": "report", "status": "failure", "run_id": 9331},
                {"id": 3, "name": "rust", "status": "waiting", "run_id": 9331, "task_id": 0},
                {"id": 4, "name": "docs", "status": "blocked", "run_id": 9331},
                {"id": 5, "name": "lint", "status": "running", "run_id": 9331},
                {"id": 6, "name": "test", "status": "cancelled", "run_id": 9331, "task_id": 0},
                {"id": 7, "name": "fmt", "status": "cancelled", "run_id": 9331, "task_id": 8}]),
    )?;
    assert_eq!(
        jobs[1].url.as_deref(),
        Some("https://codeberg.org/team/repo/actions/runs/2765/jobs/101")
    );
    assert_eq!(
        jobs.iter().map(|job| job.state).collect::<Vec<_>>(),
        [
            JobState::Finished,
            JobState::Failed,
            JobState::Waiting,
            JobState::Waiting,
            JobState::Running,
            JobState::Cancelled,
            JobState::Cancelled
        ]
    );
    assert_eq!(
        jobs.iter().map(|job| job.result).collect::<Vec<_>>(),
        [
            Some(CheckState::Success),
            Some(CheckState::Failure),
            None,
            None,
            None,
            Some(CheckState::Cancelled),
            Some(CheckState::Cancelled)
        ]
    );
    Ok(())
}

#[test]
fn jobs_are_selected_by_id_or_by_a_name_no_other_job_has() -> Result<()> {
    let run = Run {
        id: RunId::new(12),
        url: "https://github.com/team/repo/actions/runs/12".into(),
        branch: None,
        workflow: None,
        commit: None,
        pull: None,
        state: JobState::Failed,
        result: Some(CheckState::Failure),
        waiting: false,
        finish: None,
    };
    let job = |id, name: &str| RunJob {
        id,
        name: name.into(),
        state: JobState::Failed,
        result: Some(CheckState::Failure),
        url: None,
    };
    let jobs = [
        job(JobId::new(1), "test"),
        job(JobId::new(2), "test"),
        job(JobId::new(3), "lint"),
    ];
    assert_eq!(
        find_job(&run, &jobs, &JobRef::Id(JobId::new(2)))?.id,
        JobId::new(2)
    );
    assert_eq!(
        find_job(&run, &jobs, &JobRef::Name("lint".into()))?.id,
        JobId::new(3)
    );
    assert!(find_job(&run, &jobs, &JobRef::Name("test".into())).is_err());
    assert!(find_job(&run, &jobs, &JobRef::Id(JobId::new(4))).is_err());
    // A name no job has may be the ID listed beside one, and a job's name
    // comes before another's ID.
    assert_eq!(
        find_job(&run, &jobs, &JobRef::Name("2".into()))?.id,
        JobId::new(2)
    );
    let numbered = [job(JobId::new(5), "1"), job(JobId::new(1), "lint")];
    assert_eq!(
        find_job(&run, &numbered, &JobRef::Name("1".into()))?.id,
        JobId::new(5)
    );
    Ok(())
}

#[test]
fn filters_need_a_bounded_limit_and_nonempty_names() {
    assert!(validate_filter(&filter(Some("topic"), Some("ci.yml"))).is_ok());
    assert!(validate_filter(&filter(Some(" "), None)).is_err());
    let mut unbounded = filter(None, None);
    unbounded.limit = MAX_RUNS + 1;
    assert!(validate_filter(&unbounded).is_err());
}

#[tokio::test]
async fn a_run_whose_jobs_were_cancelled_reports_it_and_keeps_their_logs() -> Result<()> {
    use crate::test_support::{Server, agent_on, repo_on};
    use shoal_forge_types::backends::ForgeKind;
    let server = Server::start(|request| {
        let body = match request.line.as_str() {
            "GET /api/v1/repos/team/repo/actions/runs?run_number=7" => json!({"workflow_runs": [
                {"id": 30, "index_in_repo": 7, "status": "cancelled", "prettyref": "main",
                    "workflow_id": "ci.yml", "commit_sha": "abc"}
            ]}),
            // `rust` was cancelled while it ran, `musl` before a runner took it.
            "GET /api/v1/repos/team/repo/actions/runs/30/jobs" => json!([
                {"id": 1, "name": "gate", "status": "success", "run_id": 30, "task_id": 5},
                {"id": 2, "name": "rust", "status": "cancelled", "run_id": 30, "task_id": 6},
                {"id": 3, "name": "musl", "status": "cancelled", "run_id": 30, "task_id": 0}
            ]),
            "GET /api/v1/repos/team/repo/actions/jobs/2/logs" => {
                return (
                    200,
                    Vec::new(),
                    "Compiling shoal\ncontext canceled\n".into(),
                );
            }
            _ => return (404, Vec::new(), Vec::new()),
        };
        (200, Vec::new(), body.to_string().into())
    });
    let forgejo = repo_on(&server, ForgeKind::Forgejo, "team/repo");
    let account = agent_on(&server, "agent");
    let run = ItemRef::numbered(&forgejo, LinkKind::Ci, 7)?;
    let activity = forgejo.run_activity(&account, &run, None).await?;
    assert_eq!(activity.state(), Some(SnapshotState::Cancelled));
    assert!(activity.failed_jobs.is_empty());
    assert!(
        !server
            .received()
            .iter()
            .any(|request| request.line.contains("/logs")),
        "a cancelled job is no failure to read the log of"
    );
    let log = forgejo
        .run_job_log(&account, &run, &JobRef::Name("rust".into()))
        .await?;
    assert!(log.text.contains("context canceled"));
    Ok(())
}

#[tokio::test]
async fn a_rerun_that_finishes_between_two_reads_reports_its_finish() -> Result<()> {
    use crate::test_support::{Server, agent_on, repo_on};
    use shoal_forge_types::{backends::ForgeKind, updates::UpdateKind};
    let read = |stopped: &'static str| async move {
        let server = Server::start(move |request| {
            if request.line != "GET /api/v1/repos/team/repo/actions/runs?run_number=7" {
                return (404, Vec::new(), Vec::new());
            }
            let body = json!({"workflow_runs": [
                {"id": 30, "index_in_repo": 7, "status": "success", "prettyref": "main",
                    "workflow_id": "ci.yml", "commit_sha": "abc", "stopped": stopped}
            ]});
            (200, Vec::new(), body.to_string().into())
        });
        let forgejo = repo_on(&server, ForgeKind::Forgejo, "team/repo");
        let run = ItemRef::numbered(&forgejo, LinkKind::Ci, 7)?;
        forgejo
            .run_activity(&agent_on(&server, "agent"), &run, None)
            .await
    };
    let first = read("2026-11-21T23:08:00+01:00").await?;
    let url = "https://forge.example/team/repo/actions/runs/7";
    assert!(first.changes(&first, url).is_empty());
    let rerun = read("2026-11-21T23:28:04+01:00").await?;
    let updates = first.changes(&rerun, url);
    assert_eq!(updates.len(), 1, "{updates:?}");
    assert_eq!(updates[0].kind, UpdateKind::CiCompleted);
    Ok(())
}

#[tokio::test]
async fn a_failed_run_read_again_at_the_same_finish_keeps_its_known_failure() -> Result<()> {
    use crate::test_support::{Server, agent_on, repo_on};
    use shoal_forge_types::backends::ForgeKind;
    let stopped = std::sync::Arc::new(std::sync::Mutex::new("23:08"));
    let server = Server::start({
        let stopped = stopped.clone();
        move |request| {
            let body = match request.line.as_str() {
                "GET /api/v1/repos/team/repo/actions/runs?run_number=7" => {
                    json!({"workflow_runs": [
                        {"id": 30, "index_in_repo": 7, "status": "failure", "prettyref": "main",
                            "workflow_id": "ci.yml", "commit_sha": "abc",
                            "stopped": *stopped.lock().unwrap()}
                    ]})
                }
                "GET /api/v1/repos/team/repo/actions/runs/30/jobs" => json!([
                    {"id": 2, "name": "rust", "status": "failure", "run_id": 30, "task_id": 6}
                ]),
                _ => return (404, Vec::new(), Vec::new()),
            };
            (200, Vec::new(), body.to_string().into())
        }
    });
    let forgejo = repo_on(&server, ForgeKind::Forgejo, "team/repo");
    let account = agent_on(&server, "agent");
    let run = ItemRef::numbered(&forgejo, LinkKind::Ci, 7)?;
    let job_reads = || {
        (server.received().iter())
            .filter(|request| request.line.contains("/jobs"))
            .count()
    };
    let first = forgejo.run_activity(&account, &run, None).await?;
    let read = job_reads();
    assert!(read > 0);
    let again = forgejo.run_activity(&account, &run, Some(&first)).await?;
    assert_eq!(job_reads(), read);
    assert_eq!(again.state(), Some(SnapshotState::Failed));
    assert_eq!(again.failed_jobs, first.failed_jobs);
    // A rerun's finish reads its own jobs.
    *stopped.lock().unwrap() = "23:28";
    forgejo.run_activity(&account, &run, Some(&first)).await?;
    assert!(job_reads() > read);
    Ok(())
}

#[tokio::test]
async fn a_listed_failed_run_takes_the_state_its_jobs_show() -> Result<()> {
    use crate::test_support::{Server, agent_on, repo_on};
    use shoal_forge_types::backends::ForgeKind;
    let run = |id: u64, status: &str| {
        json!({"id": id, "index_in_repo": id, "status": status, "prettyref": "main",
            "workflow_id": "ci.yml", "commit_sha": "abc"})
    };
    let server = Server::start(move |request| {
        let body = match request.line.as_str() {
            "GET /api/v1/repos/team/repo/actions/runs?limit=10&page=1&ref=refs%2Fheads%2Fmain" => {
                json!({"workflow_runs": [
                    run(4, "failure"), run(3, "cancelled"), run(2, "failure"), run(1, "success")
                ]})
            }
            "GET /api/v1/repos/team/repo/actions/runs/4/jobs" => json!([
                {"id": 1, "name": "gate", "status": "success"},
                {"id": 2, "name": "rust", "status": "cancelled"}
            ]),
            "GET /api/v1/repos/team/repo/actions/runs/3/jobs" => json!([
                {"id": 3, "name": "rust", "status": "failure"},
                {"id": 4, "name": "musl", "status": "cancelled"}
            ]),
            _ => return (404, Vec::new(), Vec::new()),
        };
        (200, Vec::new(), body.to_string().into())
    });
    let forgejo = repo_on(&server, ForgeKind::Forgejo, "team/repo");
    let account = agent_on(&server, "agent");
    let runs = forgejo
        .list_runs(&account, &filter(Some("main"), None))
        .await?;
    let states = runs.iter().map(|run| run.state).collect::<Vec<_>>();
    // Run 2's jobs cannot be read, so it stays as the forge lists it.
    assert_eq!(
        states,
        [
            JobState::Cancelled,
            JobState::Failed,
            JobState::Failed,
            JobState::Finished
        ]
    );
    assert!(
        !server
            .received()
            .iter()
            .any(|request| request.line.contains("runs/1/jobs")),
        "a run that passed has no jobs to read"
    );
    Ok(())
}
