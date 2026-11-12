use serde_json::json;

use super::*;
use crate::ci::JobState;

#[test]
fn retried_jobs_report_their_newest_attempt() {
    // A retried job's newest attempt decides; its failed first one is history.
    let gitlab = [
        json!({"id": 7, "name": "rust", "status": "failed", "target_url": "old"}),
        json!({"id": 9, "name": "rust", "status": "running", "target_url": "new"}),
        json!({"id": 8, "name": "docs", "status": "canceled"}),
        json!({"id": 10, "name": "deploy", "status": "waiting_for_callback"}),
        json!({"id": 11, "name": "audit", "status": "pending"}),
        // A job being cancelled runs until it ends.
        json!({"id": 12, "name": "lint", "status": "canceling"}),
    ];
    assert_eq!(
        latest_checks(&gitlab).unwrap(),
        [
            ("audit".into(), JobState::Waiting, String::new()),
            ("deploy".into(), JobState::Running, String::new()),
            ("docs".into(), JobState::Failed, String::new()),
            ("lint".into(), JobState::Running, String::new()),
            ("rust".into(), JobState::Running, "new".into())
        ]
    );
    let unknown = [json!({"id": 1, "name": "x", "status": "later"})];
    assert!(latest_checks(&unknown).is_err());
}
