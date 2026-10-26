use super::*;

#[test]
fn statuses_report_failed_and_running_checks() {
    let states = |checks: Vec<CiCheck>| checks.into_iter().map(|c| c.1).collect::<Vec<_>>();
    let forgejo = json!({"total_count": 2, "statuses": [
        {"context": "ci / rust", "status": "error", "target_url": "/t/r/actions/runs/1/jobs/0"},
        {"context": "ci / docs", "status": "pending"}
    ]});
    let listed = commit_statuses(&forgejo, Dialect::Forgejo).unwrap();
    assert_eq!(listed[0].2, "/t/r/actions/runs/1/jobs/0");
    assert_eq!(states(listed), [JobState::Failed, JobState::Running]);
    let none = json!({"total_count": 0, "statuses": null});
    assert!(commit_statuses(&none, Dialect::Forgejo).unwrap().is_empty());
    let github = json!({"total_count": 1, "statuses": [
        {"context": "deploy", "state": "failure", "target_url": "https://ci.example/1"}
    ]});
    assert_eq!(
        states(commit_statuses(&github, Dialect::GitHub).unwrap()),
        [JobState::Failed]
    );
    assert!(commit_statuses(&github, Dialect::Forgejo).is_err());
}

#[test]
fn dependencies_keep_open_issues() {
    let issues = [
        json!({"state": "open", "html_url": "https://forge.example/o/r/issues/1"}),
        json!({"state": "closed", "html_url": "https://forge.example/o/r/issues/2"}),
    ];
    assert_eq!(
        open_urls(&issues).unwrap(),
        ["https://forge.example/o/r/issues/1"]
    );
}
