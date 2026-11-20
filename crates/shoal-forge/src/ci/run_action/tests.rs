use serde_json::{Value, json};

use super::*;
use crate::CommitId;
use crate::backends::Runs;
use shoal_forge_types::ci::JobState;
use shoal_forge_types::ci::check_state::CheckState;
use shoal_forge_types::numbers::PullNumber;
use shoal_forge_types::numbers::RunId;
use shoal_forge_types::numbers::RunNumber;

fn run(id: u64) -> Run {
    Run {
        id: RunId::new(id),
        url: String::new(),
        branch: None,
        workflow: None,
        commit: None,
        pull: None,
        state: JobState::Finished,
        result: Some(CheckState::Success),
        waiting: false,
    }
}

#[test]
fn each_forge_acts_on_a_run_its_own_way() -> Result<()> {
    let endpoint = |remote: &str, action, failed| -> Result<String> {
        let operation = ForgeRepo::parse(remote)?
            .kind()
            .run_operation(&run(9), action, failed)?;
        Ok(match operation {
            RunOperation::Post(endpoint) => endpoint,
            RunOperation::EachFailedJob(endpoint) => format!("each of {}", endpoint(JobId::new(7))),
        })
    };
    let github = "https://github.com/team/repo";
    assert_eq!(
        endpoint(github, CiAction::Rerun, false)?,
        "actions/runs/9/rerun"
    );
    assert_eq!(
        endpoint(github, CiAction::Rerun, true)?,
        "actions/runs/9/rerun-failed-jobs"
    );
    assert_eq!(
        endpoint(github, CiAction::Cancel, false)?,
        "actions/runs/9/cancel"
    );
    // GitLab retries a pipeline's failed jobs either way.
    let gitlab = "https://gitlab.com/team/repo";
    assert_eq!(
        endpoint(gitlab, CiAction::Rerun, false)?,
        "pipelines/9/retry"
    );
    assert_eq!(
        endpoint(gitlab, CiAction::Cancel, false)?,
        "pipelines/9/cancel"
    );
    // Forgejo reruns failed jobs one by one.
    let forgejo = "https://codeberg.org/team/repo";
    assert_eq!(
        endpoint(forgejo, CiAction::Rerun, false)?,
        "actions/runs/9/rerun"
    );
    assert_eq!(
        endpoint(forgejo, CiAction::Rerun, true)?,
        "each of actions/jobs/7/rerun"
    );
    assert_eq!(
        endpoint(forgejo, CiAction::Cancel, false)?,
        "actions/runs/9/cancel"
    );
    // A job reruns alone, never with the run's other failed jobs.
    let job = |remote: &str| -> Result<String> {
        ForgeRepo::parse(remote)?
            .kind()
            .rerun_job_endpoint(JobId::new(7))
    };
    assert_eq!(job(github)?, "actions/jobs/7/rerun");
    assert_eq!(job("https://gitlab.com/team/repo")?, "jobs/7/retry");
    assert_eq!(job(forgejo)?, "actions/jobs/7/rerun");
    Ok(())
}

#[test]
fn runs_read_their_branch_workflow_commit_and_state_from_the_forge() -> Result<()> {
    let read = |remote: &str, value| -> Result<Run> {
        let repo = ForgeRepo::parse(remote)?;
        repo.kind().run(&repo, RunNumber::new(12), &value)
    };
    assert_eq!(
        read(
            "https://github.com/team/repo",
            json!({"id": 12, "html_url": "https://github.com/team/repo/actions/runs/12",
                    "head_branch": "topic", "path": ".github/workflows/ci.yml",
                    "head_sha": "abc", "status": "completed", "conclusion": "failure"})
        )?,
        Run {
            id: RunId::new(12),
            url: "https://github.com/team/repo/actions/runs/12".into(),
            branch: Some("topic".into()),
            workflow: Some("ci.yml".into()),
            commit: Some("abc".into()),
            pull: None,
            state: JobState::Failed,
            result: Some(CheckState::Failure),
            waiting: false,
        }
    );
    // A tag pipeline ran on no branch.
    let tag = read(
        "https://gitlab.com/team/repo",
        json!({"id": 12, "web_url": "https://gitlab.com/team/repo/-/pipelines/12",
                "ref": "v1", "tag": true, "sha": "def", "status": "running"}),
    )?;
    assert_eq!(
        (
            tag.branch,
            tag.commit.as_ref().map(CommitId::as_str),
            tag.state
        ),
        (None, Some("def"), JobState::Running)
    );
    let forgejo = read(
        "https://codeberg.org/team/repo",
        json!({"workflow_runs": [{"id": 9331, "index_in_repo": 12, "prettyref": "topic",
                "workflow_id": "deps.yml", "commit_sha": "123", "status": "success"}]}),
    )?;
    assert_eq!(
        forgejo,
        Run {
            id: RunId::new(9331),
            url: "https://codeberg.org/team/repo/actions/runs/12".into(),
            branch: Some("topic".into()),
            workflow: Some("deps.yml".into()),
            commit: Some("123".into()),
            pull: None,
            state: JobState::Finished,
            result: Some(CheckState::Success),
            waiting: false,
        }
    );
    assert!(
        read(
            "https://codeberg.org/team/repo",
            json!({"workflow_runs": []}),
        )
        .is_err()
    );
    Ok(())
}

#[test]
fn runs_name_the_pr_of_their_repository_they_ran_for() -> Result<()> {
    let pull = |remote: &str, value| -> Result<Option<u64>> {
        let repo = ForgeRepo::parse(remote)?;
        let run = repo.kind().run(&repo, RunNumber::new(12), &value)?;
        Ok(run.pull.map(PullNumber::get))
    };
    // Forgejo names a PR run's branch as the PR, and its PR only in the
    // event's payload.
    let forgejo = |event: &str, payload: Value| {
        json!({"workflow_runs": [{"id": 9331, "index_in_repo": 12, "prettyref": "#8",
            "event": event, "event_payload": payload.to_string(), "status": "running"}]})
    };
    let codeberg = "https://codeberg.org/team/repo";
    let opened = json!({"number": 8, "pull_request": {"number": 8}});
    assert_eq!(pull(codeberg, forgejo("pull_request", opened))?, Some(8));
    let issue = json!({"number": 3, "issue": {"number": 3}});
    assert_eq!(pull(codeberg, forgejo("issues", issue))?, None);
    // GitHub lists the PRs whose head the run ran on, also another
    // repository's, and also for a push's run.
    let github = |event: &str| {
        json!({"id": 12, "html_url": "https://github.com/team/repo/actions/runs/12",
        "head_branch": "topic", "event": event, "status": "in_progress", "pull_requests": [
            {"number": 4, "url": "https://api.github.com/repos/upstream/repo/pulls/4"},
            {"number": 9, "url": "https://api.github.com/repos/Team/Repo/pulls/9"}
        ]})
    };
    let repo = "https://github.com/team/repo";
    assert_eq!(pull(repo, github("pull_request"))?, Some(9));
    assert_eq!(pull(repo, github("push"))?, None);
    Ok(())
}

#[tokio::test]
async fn forgejo_reruns_each_failed_job_once_skipping_those_a_rerun_restarted() -> Result<()> {
    use std::sync::{Arc, Mutex};

    use crate::test_support::{Server, agent_on, repo_on};
    use shoal_forge_types::backends::ForgeKind;
    // `deploy` needs `test`, so rerunning `test` blocks it again; `lint`
    // was cancelled.
    let statuses = Arc::new(Mutex::new(vec![
        ("build", "success"),
        ("test", "failure"),
        ("deploy", "failure"),
        ("lint", "cancelled"),
    ]));
    let jobs = statuses.clone();
    let server = Server::start(move |request| {
        let mut jobs = jobs.lock().unwrap();
        if let Some(id) = request
            .line
            .strip_prefix("POST /api/v1/repos/team/repo/actions/jobs/")
            .and_then(|rest| rest.strip_suffix("/rerun"))
        {
            let index: usize = id.parse::<usize>().unwrap() - 1;
            jobs[index].1 = "waiting";
            if jobs[index].0 == "test" {
                jobs[2].1 = "blocked";
            }
            return (204, Vec::new(), Vec::new());
        }
        let listed: Vec<_> = jobs
            .iter()
            .enumerate()
            .map(|(index, (name, status))| {
                json!({"id": index + 1, "name": name, "run_id": 9, "status": status})
            })
            .collect();
        (200, Vec::new(), json!(listed).to_string().into())
    });
    let forgejo = repo_on(&server, ForgeKind::Forgejo, "team/repo");
    let account = agent_on(&server, "agent");
    forgejo
        .act_on_run(&account, &run(9), CiAction::Rerun, true)
        .await?;
    let posts: Vec<_> = server
        .received()
        .into_iter()
        .map(|request| request.line)
        .filter(|line| line.starts_with("POST"))
        .collect();
    assert_eq!(
        posts,
        [
            "POST /api/v1/repos/team/repo/actions/jobs/2/rerun",
            "POST /api/v1/repos/team/repo/actions/jobs/4/rerun",
        ]
    );

    // A run without failed jobs has nothing to rerun.
    let rerun = forgejo
        .act_on_run(&account, &run(9), CiAction::Rerun, true)
        .await;
    assert!(rerun.is_err());
    Ok(())
}

#[test]
fn only_a_run_the_forge_reports_successful_passes() -> Result<()> {
    let read = |remote: &str, value| -> Result<Run> {
        let repo = ForgeRepo::parse(remote)?;
        repo.kind().run(&repo, RunNumber::new(12), &value)
    };
    let gitlab = |status: &str| {
        json!({"id": 12, "web_url": "https://gitlab.com/team/repo/-/pipelines/12",
                "ref": "main", "tag": false, "status": status})
    };
    assert!(read("https://gitlab.com/team/repo", gitlab("success"))?.passed());
    for status in ["skipped", "manual"] {
        assert!(
            !read("https://gitlab.com/team/repo", gitlab(status))?.passed(),
            "{status}"
        );
    }
    let forgejo = |status: &str| {
        json!({"workflow_runs": [{"id": 9331, "index_in_repo": 12, "prettyref": "main",
                "workflow_id": "ci.yml", "status": status}]})
    };
    assert!(read("https://codeberg.org/team/repo", forgejo("success"))?.passed());
    assert!(!read("https://codeberg.org/team/repo", forgejo("skipped"))?.passed());
    Ok(())
}

/// A Forgejo server whose run 12 is in `run`, with the jobs `jobs` of
/// names and statuses, numbered from 1, that records what it is asked.
fn forgejo_run(
    run: &'static str,
    jobs: Vec<(&'static str, &'static str)>,
) -> crate::test_support::Server {
    crate::test_support::Server::start(move |request| {
        let body = if request.line.contains("/version") {
            json!({"version": "17.0.1+gitea-1.22.0"})
        } else if request.line.contains("/jobs?") || request.line.contains("/runs/9331/jobs") {
            let listed: Vec<_> = (jobs.iter().enumerate())
                .map(|(index, (name, status))| {
                    json!({"id": index + 1, "name": name, "run_id": 9331, "status": status})
                })
                .collect();
            json!(listed)
        } else if request.line.starts_with("POST") {
            return (204, Vec::new(), Vec::new());
        } else {
            json!({"workflow_runs": [{"id": 9331, "index_in_repo": 12, "prettyref": "topic",
                    "workflow_id": "ci.yml", "commit_sha": "123", "status": run}]})
        };
        (200, Vec::new(), body.to_string().into())
    })
}

/// The POST requests `server` received.
fn posts(server: &crate::test_support::Server) -> Vec<String> {
    (server.received().into_iter())
        .map(|request| request.line)
        .filter(|line| line.starts_with("POST"))
        .collect()
}

#[tokio::test]
async fn a_stopped_job_reruns_alone_once_its_run_finished() -> Result<()> {
    use crate::test_support::{agent_on, repo_on};
    use shoal_forge_types::backends::ForgeKind;
    let rerun = |server: &crate::test_support::Server, id: u64| {
        let forgejo = repo_on(server, ForgeKind::Forgejo, "team/repo");
        let url = ItemRef::numbered(&forgejo, LinkKind::Ci, 12).unwrap();
        let account = agent_on(server, "agent");
        async move { forgejo.rerun_job(&account, &url, JobId::new(id)).await }
    };
    let jobs = vec![
        ("build", "failure"),
        ("test", "failure"),
        ("lint", "success"),
    ];

    let running = forgejo_run("running", jobs.clone());
    assert_eq!(rerun(&running, 2).await?, JobRerun::Unfinished);
    assert!(posts(&running).is_empty());

    let finished = forgejo_run("failure", jobs);
    assert_eq!(rerun(&finished, 2).await?, JobRerun::Rerun);
    assert_eq!(
        posts(&finished),
        ["POST /api/v1/repos/team/repo/actions/jobs/2/rerun"]
    );
    assert_eq!(rerun(&finished, 3).await?, JobRerun::Settled);
    // A later attempt of the job has an ID of its own.
    assert_eq!(rerun(&finished, 9).await?, JobRerun::Settled);
    assert_eq!(posts(&finished).len(), 1);
    Ok(())
}
