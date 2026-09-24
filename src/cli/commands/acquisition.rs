//! Preserve the daemon's last allocation outcome while waiting for access.
use std::time::Duration;

use anyhow::Result;
use tokio::time::{Instant, sleep};

use crate::daemon::access::{AccessRequest, DecisionStatus};

#[derive(Debug)]
pub(super) enum Acquisition<T> {
    Acquired(T),
    Busy(String),
    ApprovalPending(Box<AccessRequest>),
    ApprovalDenied(Box<AccessRequest>),
}

impl<T> Acquisition<T> {
    pub(super) fn approval(request: Box<AccessRequest>) -> Self {
        if request.status == DecisionStatus::Denied {
            Self::ApprovalDenied(request)
        } else {
            Self::ApprovalPending(request)
        }
    }
}

/// Poll capacity and pending approvals about once a second. On timeout return
/// the last outcome intact; acquisition, denial and errors finish immediately.
pub(super) async fn retry<T>(
    wait_seconds: u64,
    mut attempt: impl AsyncFnMut() -> Result<Acquisition<T>>,
) -> Result<Acquisition<T>> {
    let deadline = Instant::now() + Duration::from_secs(wait_seconds);
    loop {
        let outcome = attempt().await?;
        match outcome {
            Acquisition::Acquired(_) | Acquisition::ApprovalDenied(_) => return Ok(outcome),
            _ if Instant::now() >= deadline => return Ok(outcome),
            Acquisition::Busy(_) | Acquisition::ApprovalPending(_) => {
                let remaining = deadline.saturating_duration_since(Instant::now());
                sleep(Duration::from_secs(1).min(remaining)).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, to_value};

    fn approval(status: DecisionStatus) -> Box<AccessRequest> {
        serde_json::from_value(json!({
            "id": "approval-id", "workspace_id": "workspace-id", "workspace": "worker",
            "target": "port/web", "name": "web", "reason": "serve preview",
            "specification": {"env": "PORT_WEB", "on_conflict": "suggest",
                "preferred": null, "range": [3000, 3100]},
            "lifetime": "lease", "status": status, "created_at": 123,
            "decided_at": if status == DecisionStatus::Denied { Some(456) } else { None },
            "active": true
        }))
        .unwrap()
    }

    async fn sequence(wait: u64, outcomes: Vec<Acquisition<u8>>) -> Acquisition<u8> {
        let mut outcomes = outcomes.into_iter();
        let result = retry(wait, async || Ok(outcomes.next().expect("extra attempt")))
            .await
            .unwrap();
        assert!(outcomes.next().is_none(), "stopped before final outcome");
        result
    }

    #[tokio::test]
    async fn no_wait_preserves_every_outcome() {
        assert!(matches!(
            sequence(0, vec![Acquisition::Acquired(7)]).await,
            Acquisition::Acquired(7)
        ));
        assert!(matches!(
            sequence(0, vec![Acquisition::Busy("full".into())]).await,
            Acquisition::Busy(message) if message == "full"
        ));
        for status in [DecisionStatus::Pending, DecisionStatus::Denied] {
            let request = approval(status);
            let expected = to_value(&request).unwrap();
            let outcome = sequence(0, vec![Acquisition::approval(request)]).await;
            let request = match (status, outcome) {
                (DecisionStatus::Pending, Acquisition::ApprovalPending(request))
                | (DecisionStatus::Denied, Acquisition::ApprovalDenied(request)) => request,
                other => panic!("wrong approval outcome: {other:?}"),
            };
            assert_eq!(to_value(request).unwrap(), expected);
        }
    }

    #[tokio::test]
    async fn timeout_preserves_the_latest_busy_or_pending_outcome() {
        let expected = to_value(approval(DecisionStatus::Pending)).unwrap();
        let outcome = sequence(
            1,
            vec![
                Acquisition::Busy("full".into()),
                Acquisition::approval(approval(DecisionStatus::Pending)),
            ],
        )
        .await;
        let Acquisition::ApprovalPending(request) = outcome else {
            panic!("lost pending approval: {outcome:?}");
        };
        assert_eq!(to_value(request).unwrap(), expected);
        let outcome = sequence(
            1,
            vec![
                Acquisition::approval(approval(DecisionStatus::Pending)),
                Acquisition::Busy("now full".into()),
            ],
        )
        .await;
        assert!(matches!(outcome, Acquisition::Busy(message) if message == "now full"));
    }

    #[tokio::test]
    async fn pending_finishes_on_denial_or_acquisition_before_timeout() {
        let outcome = sequence(
            60,
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
                60,
                vec![
                    Acquisition::approval(approval(DecisionStatus::Pending)),
                    Acquisition::Acquired(7),
                ]
            )
            .await,
            Acquisition::Acquired(7)
        ));
    }

    #[tokio::test]
    async fn errors_stop_polling() {
        let mut attempts = 0;
        let error = retry::<()>(60, async || {
            attempts += 1;
            anyhow::bail!("connection lost")
        })
        .await
        .unwrap_err();
        assert_eq!(error.to_string(), "connection lost");
        assert_eq!(attempts, 1);
    }
}
