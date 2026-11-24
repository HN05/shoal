use serde_json::json;

use super::*;

#[test]
fn a_job_with_a_task_names_its_runner_only_in_its_log() {
    let running = json!({"id": 8, "handle": "h1", "status": "running", "task_id": 31});
    assert_eq!(untaken(&running, "h1"), None);
    let log = "2026-11-24T12:38:55.1234567Z Runner 4247-b920 (version:v13.2.1) received task 31 of job rust, triggered by event: push\n\
               2026-11-24T12:38:55.2Z workflow prepared\n";
    assert_eq!(log_taker(log), JobTaker::Runner("4247-b920".into()));
    assert_eq!(log_taker(""), JobTaker::Pending);
    assert_eq!(log_taker("workflow prepared\n"), JobTaker::Pending);
}

#[test]
fn a_job_without_a_task_waits_unless_it_ended() {
    let waiting = json!({"id": 8, "handle": "h1", "status": "waiting", "task_id": 0});
    assert_eq!(untaken(&waiting, "h1"), Some(JobTaker::Pending));
    let cancelled = json!({"id": 8, "handle": "h1", "status": "cancelled", "task_id": 0});
    assert_eq!(untaken(&cancelled, "h1"), Some(JobTaker::Nobody));
}

#[test]
fn a_later_attempt_leaves_the_claimed_one_untaken() {
    let rerun = json!({"id": 8, "handle": "h2", "status": "running", "task_id": 32});
    assert_eq!(untaken(&rerun, "h1"), Some(JobTaker::Nobody));
    // A Forgejo that names no handle is read by its task.
    let unnamed = json!({"id": 8, "status": "running", "task_id": 32});
    assert_eq!(untaken(&unnamed, "h1"), None);
}
