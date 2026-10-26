use super::*;
use crate::backends::github::GitHub;

fn job(name: &str, state: JobState, location: Option<Location>) -> Job {
    Job {
        name: name.into(),
        state,
        location,
    }
}

fn github(run: u64, job: u64) -> Option<Location> {
    Some(Location::GitHub { run, job })
}

fn names(jobs: Vec<Job>) -> Vec<String> {
    jobs.into_iter().map(|job| job.name).collect()
}

#[test]
fn actions_select_named_checks_in_their_state_or_every_ci_job() {
    let gitlab = Some(Location::GitLab { job: 1 });
    let jobs = [
        job("rust", JobState::Failed, gitlab),
        job("docs", JobState::Running, gitlab),
        job("lint", JobState::Finished, gitlab),
        job("rust", JobState::Finished, gitlab),
        job("external", JobState::Failed, None),
    ];
    assert_eq!(
        names(select(&jobs, CiAction::Rerun, &[]).unwrap()),
        ["rust"]
    );
    assert_eq!(
        names(select(&jobs, CiAction::Cancel, &[]).unwrap()),
        ["docs"]
    );
    assert_eq!(
        names(select(&jobs, CiAction::Rerun, &["rust".into()]).unwrap()),
        ["rust"]
    );
    let error = select(&jobs, CiAction::Rerun, &["lint".into()]).unwrap_err();
    assert_eq!(error.to_string(), "check lint is not failed");
    let error = select(&jobs, CiAction::Cancel, &["other".into()]).unwrap_err();
    assert!(error.to_string().contains("was not found"), "{error}");
    let error = select(&jobs, CiAction::Rerun, &["external".into()]).unwrap_err();
    assert!(error.to_string().contains("does not link"), "{error}");
    let error = select(&jobs[2..], CiAction::Rerun, &[]).unwrap_err();
    assert_eq!(error.to_string(), "no CI job on the PR's head is failed");
}

#[test]
fn github_reruns_several_failed_jobs_of_a_run_together() {
    let jobs = [
        job("rust", JobState::Failed, github(1, 11)),
        job("docs", JobState::Failed, github(1, 12)),
        job("lint", JobState::Failed, github(1, 13)),
        job("fmt", JobState::Failed, github(2, 21)),
        job("test", JobState::Running, github(3, 31)),
        job("build", JobState::Failed, github(3, 32)),
    ];
    let post = |endpoint: &str| Operation::Post(endpoint.into());
    let selected = [jobs[0].clone(), jobs[1].clone(), jobs[3].clone()];
    let (planned, checks) = operations(&GitHub, &jobs, &selected, CiAction::Rerun).unwrap();
    assert_eq!(
        planned,
        [
            post("actions/runs/1/rerun-failed-jobs"),
            post("actions/jobs/21/rerun")
        ]
    );
    assert_eq!(checks, ["rust", "docs", "lint", "fmt"]);
    let repeated = select(&jobs, CiAction::Rerun, &["rust".into(), "rust".into()]).unwrap();
    let (planned, checks) = operations(&GitHub, &jobs, &repeated, CiAction::Rerun).unwrap();
    assert_eq!(planned, [post("actions/jobs/11/rerun")]);
    assert_eq!(checks, ["rust"]);
    let (planned, _) = operations(&GitHub, &jobs, &jobs[4..5], CiAction::Cancel).unwrap();
    assert_eq!(planned, [post("actions/runs/3/cancel")]);
    // A run with a running job refuses reruns until it finishes.
    let error = operations(&GitHub, &jobs, &jobs[5..], CiAction::Rerun).unwrap_err();
    assert!(
        error.to_string().contains("test is still running"),
        "{error}"
    );
}

#[test]
fn check_urls_locate_jobs_only_in_the_repository() {
    let github = ForgeRepo::parse("https://github.com/team/repo").unwrap();
    assert_eq!(
        github.locate("https://github.com/team/repo/actions/runs/12/job/34"),
        Some(Location::GitHub { run: 12, job: 34 })
    );
    for target in [
        "https://github.com/other/repo/actions/runs/12/job/34",
        "https://github.com/team/repo/actions/runs/12",
        "https://github.com/team/repo/actions/runs/0/job/34",
        "https://checks.example/team/repo/actions/runs/12/job/34",
        "",
    ] {
        assert_eq!(github.locate(target), None, "{target}");
    }
    let gitlab = ForgeRepo::parse("http://gitlab.com/group/sub/repo").unwrap();
    assert_eq!(
        gitlab.locate("https://gitlab.com/group/sub/repo/-/jobs/56"),
        Some(Location::GitLab { job: 56 })
    );
    for target in [
        "https://gitlab.com/group/repo/-/jobs/56",
        "https://gitlab.com/group/sub/repo/-/pipelines/56",
        "https://gitlab.com/group/sub/repo/-/jobs/56/retry",
    ] {
        assert_eq!(gitlab.locate(target), None, "{target}");
    }
    let forgejo = ForgeRepo::parse("https://forge.example/team/repo").unwrap();
    assert_eq!(
        forgejo.locate("/team/repo/actions/runs/2765/jobs/1"),
        Some(Location::Forgejo { run_number: 2765 })
    );
    assert_eq!(forgejo.locate("https://ci.example/build/1"), None);
}
