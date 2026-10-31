use super::*;
use serde_json::json;

#[test]
fn web_job_locations_are_repository_bound_and_use_indices() {
    let forge = ForgeRepo::parse("https://forge.example/team/repo").unwrap();
    let http = ForgeRepo::parse("http://forge.example/team/repo").unwrap();
    for target in [
        "/team/repo/actions/runs/2765/jobs/1",
        "https://forge.example/team/repo/actions/runs/2765/jobs/1",
        "http://forge.example/team/repo/actions/runs/2765/jobs/1",
    ] {
        assert_eq!(job_location(&forge, target).unwrap(), (2765, 1));
        assert_eq!(job_location(&http, target).unwrap(), (2765, 1));
    }
    for target in [
        "https://evil.example/team/repo/actions/runs/2765/jobs/1",
        "/other/repo/actions/runs/2765/jobs/1",
        "/team/repo/actions/runs/0/jobs/1",
        "/team/repo/actions/runs/2765/jobs/1/logs",
    ] {
        assert!(job_location(&forge, target).is_err(), "{target}");
    }
    assert_eq!(
        job_url(&http, "/team/repo/actions/runs/2765/jobs/1"),
        "http://forge.example/team/repo/actions/runs/2765/jobs/1"
    );
    let runs =
        json!({"workflow_runs": [{"id": 9331, "index_in_repo": 2765, "commit_sha": "head"}]});
    assert_eq!(run_id(&runs, 2765, "head").unwrap(), 9331);
    assert!(run_id(&runs, 2765, "other").is_err());
    let jobs = json!([{"id": 18344, "run_id": 9331, "status": "success"}, {"id": 18345, "run_id": 9331, "status": "failure", "task_id": 9850}]);
    assert_eq!(job_id(&jobs, 1, 9331).unwrap(), 18345);
    assert!(job_id(&jobs, 0, 9331).is_err());
    assert!(job_id(&jobs, 1, 7).is_err());
}

#[test]
fn failed_steps_are_the_marked_or_unfinished_step() {
    let long = format!(
        "⭐ Run Main Test\n{}\n⚙️ [runner]: exitcode '1': failure\n",
        "🦀".repeat(10_000)
    );
    assert_eq!(failed_step(&long).as_deref(), Some("Main Test"));
    let marked = "⭐ Run Main Checkout\n  ✅  Success - Main Checkout\n⭐ Run Main Test\n\
                  test result: FAILED\n  ❌  Failure - Main Test\n⚙️ [runner]: exitcode '101': failure\n";
    assert_eq!(failed_step(marked).as_deref(), Some("Main Test"));
    // Later steps of a job log can go unmarked.
    let unmarked = "⭐ Run Main Checkout\n  ✅  Success - Main Checkout\nrustc 1.98.1\n\
                    test result: FAILED\n⚙️ [runner]: exitcode '101': failure\n🏁  Job failed\n";
    assert_eq!(failed_step(unmarked), None);
}

#[test]
fn listed_failed_steps_are_the_first_step_marked_failing() {
    let job = |steps: Value| json!({"id": 7, "steps": steps});
    let listed = job(json!([
        {"name": "Set up job", "number": 0, "status": "success"},
        {"name": "cargo test", "number": 1, "status": "failure"},
        {"name": "Complete job", "number": 2, "status": "failure"}
    ]));
    assert_eq!(failed_listed_step(&listed).as_deref(), Some("cargo test"));
    let cancelled = job(json!([
        {"name": "cargo test", "number": 1, "status": "cancelled"}
    ]));
    assert_eq!(failed_listed_step(&cancelled), None);
    // Forgejo before 17 lists no steps.
    assert_eq!(failed_listed_step(&json!({"id": 7})), None);
}
