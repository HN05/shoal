use serde_json::json;

use super::*;

#[test]
fn forgejo_statuses_need_every_status_and_known_states() {
    let combined = json!({"total_count": 5, "statuses": [
        {"context": "ci / rust", "status": "success"},
        {"context": "review", "status": "pending", "description": "Has started running"},
        {"context": "ci / docs", "status": "pending", "description": "Waiting to run"},
        {"context": "ci / musl", "status": "failure", "description": "Has been cancelled"},
        {"context": "ci / crates", "status": "failure", "description": "Failing after 2m"}
    ]});
    assert_eq!(
        statuses(&combined).unwrap(),
        [
            HeadCheck {
                name: "ci / rust".into(),
                state: CheckState::Success
            },
            HeadCheck {
                name: "review".into(),
                state: CheckState::Pending
            },
            HeadCheck {
                name: "ci / docs".into(),
                state: CheckState::Waiting
            },
            HeadCheck {
                name: "ci / musl".into(),
                state: CheckState::Cancelled
            },
            HeadCheck {
                name: "ci / crates".into(),
                state: CheckState::Failure
            },
        ]
    );
    assert!(
        statuses(&json!({"total_count": 0, "statuses": null}))
            .unwrap()
            .is_empty()
    );
    let partial = json!({"total_count": 4, "statuses": combined["statuses"]});
    assert!(statuses(&partial).is_err());
    let unknown = json!({"total_count": 1, "statuses": [{"context": "x", "status": "later"}]});
    assert!(statuses(&unknown).is_err());
}
