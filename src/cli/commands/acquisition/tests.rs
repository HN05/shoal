use super::*;
use serde_json::{json, to_value};

fn approval(status: DecisionStatus) -> Box<ApprovalRequest> {
    serde_json::from_value(json!({
        "id": 1, "workspace_id": "workspace-id", "workspace": "worker",
        "target": "port/web", "name": "web", "reason": "serve preview",
        "specification": {"env": "PORT_WEB", "on_conflict": "suggest",
            "preferred": null, "range": [3000, 3100]},
        "lifetime": "lease", "status": status, "created_at": 123,
        "decided_at": if status == DecisionStatus::Denied { Some(456) } else { None },
        "active": true
    }))
    .unwrap()
}

async fn sequence(queue: bool, outcomes: Vec<Acquisition<u8>>) -> Acquisition<u8> {
    let mut outcomes = outcomes.into_iter();
    let result = retry(queue, async || Ok(outcomes.next().expect("extra attempt")))
        .await
        .unwrap();
    assert!(outcomes.next().is_none(), "stopped before final outcome");
    result
}

#[tokio::test(start_paused = true)]
async fn unqueued_requests_preserve_every_outcome() {
    assert!(matches!(
        sequence(false, vec![Acquisition::Acquired(7)]).await,
        Acquisition::Acquired(7)
    ));
    assert!(matches!(
        sequence(false, vec![Acquisition::Busy("full".into())]).await,
        Acquisition::Busy(message) if message == "full"
    ));
    for status in [DecisionStatus::Pending, DecisionStatus::Denied] {
        let request = approval(status);
        let expected = to_value(&request).unwrap();
        let outcome = sequence(false, vec![Acquisition::approval(request)]).await;
        let request = match (status, outcome) {
            (DecisionStatus::Pending, Acquisition::ApprovalPending(request))
            | (DecisionStatus::Denied, Acquisition::ApprovalDenied(request)) => request,
            other => panic!("wrong approval outcome: {other:?}"),
        };
        assert_eq!(to_value(request).unwrap(), expected);
    }
}

#[tokio::test(start_paused = true)]
async fn queued_requests_wait_through_busy_and_pending_outcomes() {
    let outcome = sequence(
        true,
        vec![
            Acquisition::Busy("full".into()),
            Acquisition::approval(approval(DecisionStatus::Pending)),
            Acquisition::Busy("now full".into()),
            Acquisition::Acquired(7),
        ],
    )
    .await;
    assert!(matches!(outcome, Acquisition::Acquired(7)));
}

#[tokio::test(start_paused = true)]
async fn pending_finishes_on_denial_or_acquisition() {
    let outcome = sequence(
        true,
        vec![
            Acquisition::approval(approval(DecisionStatus::Pending)),
            Acquisition::approval(approval(DecisionStatus::Denied)),
        ],
    )
    .await;
    let Acquisition::ApprovalDenied(request) = outcome else {
        panic!("lost denial: {outcome:?}");
    };
    assert_eq!(
        to_value(request).unwrap(),
        to_value(approval(DecisionStatus::Denied)).unwrap()
    );
    assert!(matches!(
        sequence(
            true,
            vec![
                Acquisition::approval(approval(DecisionStatus::Pending)),
                Acquisition::Acquired(7),
            ]
        )
        .await,
        Acquisition::Acquired(7)
    ));
}

#[tokio::test(start_paused = true)]
async fn errors_stop_polling() {
    let mut attempts = 0;
    let error = retry::<()>(true, async || {
        attempts += 1;
        anyhow::bail!("connection lost")
    })
    .await
    .unwrap_err();
    assert_eq!(error.to_string(), "connection lost");
    assert_eq!(attempts, 1);
}
