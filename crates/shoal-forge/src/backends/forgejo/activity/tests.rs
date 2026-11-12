use serde_json::json;

use super::*;

#[test]
fn forgejo_statuses_need_every_status_and_known_states() {
    let combined = json!({"total_count": 3, "statuses": [
        {"context": "ci / rust", "status": "success"},
        {"context": "review", "status": "pending", "description": "Has started running"},
        {"context": "ci / docs", "status": "pending", "description": "Waiting to run"}
    ]});
    assert_eq!(
        statuses(&combined).unwrap(),
        [
            RestCheck {
                name: "ci / rust".into(),
                state: CheckState::Success
            },
            RestCheck {
                name: "review".into(),
                state: CheckState::Pending
            },
            RestCheck {
                name: "ci / docs".into(),
                state: CheckState::Waiting
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
