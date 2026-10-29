use serde_json::json;

use super::*;

fn filter(branch: Option<&str>, workflow: Option<&str>) -> RunFilter {
    RunFilter {
        branch: branch.map(Into::into),
        workflow: workflow.map(Into::into),
        commit: None,
        limit: 10,
    }
}

fn path(remote: &str, filter: &RunFilter) -> Result<String> {
    ForgeRepo::parse(remote)?.kind.runs_path(filter)
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
    let error = path(gitlab, &filter(None, Some("ci.yml"))).unwrap_err();
    assert!(
        error.to_string().contains("leave out the workflow"),
        "{error}"
    );
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
fn listed_runs_are_read_like_one_run() -> Result<()> {
    let repo = ForgeRepo::parse("https://gitlab.com/team/repo")?;
    let value = json!([{"id": 12, "web_url": "https://gitlab.com/team/repo/-/pipelines/12",
            "ref": "topic", "tag": false, "status": "failed"}]);
    let listed = repo.kind.listed_runs(&value).unwrap_or_default();
    let run = repo.kind.run_item(&repo, &listed[0])?;
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
        id: 12,
        url: "https://github.com/team/repo/actions/runs/12".into(),
        branch: None,
        workflow: None,
        state: JobState::Failed,
    };
    let jobs = github.kind.run_jobs(
        &run,
        0,
        &json!({"jobs": [
            {"id": 34, "name": "rust", "status": "completed", "conclusion": "failure",
                "html_url": "https://github.com/team/repo/actions/runs/12/job/34"},
            {"id": 35, "name": "docs", "status": "in_progress", "conclusion": null}
        ]}),
    )?;
    assert_eq!(
        jobs,
        [
            RunJob {
                id: 34,
                name: "rust".into(),
                state: JobState::Failed,
                url: Some("https://github.com/team/repo/actions/runs/12/job/34".into()),
            },
            RunJob {
                id: 35,
                name: "docs".into(),
                state: JobState::Running,
                url: None,
            },
        ]
    );
    // Forgejo links a job by its index in the run.
    let forgejo = ForgeRepo::parse("https://codeberg.org/team/repo")?;
    let run = Run {
        id: 9331,
        url: "https://codeberg.org/team/repo/actions/runs/2765".into(),
        ..run
    };
    let jobs = forgejo.kind.run_jobs(
        &run,
        100,
        &json!([{"id": 1, "name": "audit", "status": "success", "run_id": 9331},
                {"id": 2, "name": "report", "status": "failure", "run_id": 9331}]),
    )?;
    assert_eq!(
        jobs[1].url.as_deref(),
        Some("https://codeberg.org/team/repo/actions/runs/2765/jobs/101")
    );
    assert_eq!(jobs[1].state, JobState::Failed);
    Ok(())
}

#[test]
fn jobs_are_selected_by_id_or_by_a_name_no_other_job_has() -> Result<()> {
    let run = Run {
        id: 12,
        url: "https://github.com/team/repo/actions/runs/12".into(),
        branch: None,
        workflow: None,
        state: JobState::Failed,
    };
    let job = |id, name: &str| RunJob {
        id,
        name: name.into(),
        state: JobState::Failed,
        url: None,
    };
    let jobs = [job(1, "test"), job(2, "test"), job(3, "lint")];
    assert_eq!(find_job(&run, &jobs, &JobRef::Id(2))?.id, 2);
    assert_eq!(find_job(&run, &jobs, &JobRef::Name("lint".into()))?.id, 3);
    let error = find_job(&run, &jobs, &JobRef::Name("test".into())).unwrap_err();
    assert!(
        error.to_string().contains("select one by its ID"),
        "{error}"
    );
    assert!(find_job(&run, &jobs, &JobRef::Id(4)).is_err());
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
