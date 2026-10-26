use super::*;
use serde_json::json;

#[test]
fn check_runs_report_failed_and_running_checks() {
    let states = |checks: Vec<CiCheck>| checks.into_iter().map(|c| c.1).collect::<Vec<_>>();
    let github = json!({"total_count": 3, "check_runs": [
        {"name": "rust", "status": "completed", "conclusion": "timed_out", "details_url": "u"},
        {"name": "docs", "status": "queued", "conclusion": null},
        {"name": "lint", "status": "completed", "conclusion": "success"}
    ]});
    assert_eq!(
        states(checks(&github).unwrap()),
        [JobState::Failed, JobState::Running, JobState::Finished]
    );
    let partial = json!({"total_count": 4, "check_runs": github["check_runs"]});
    assert!(checks(&partial).is_err());
}
