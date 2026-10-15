//! Allocation outcomes shared by every acquisition, and the queued wait for them.
use std::time::Duration;

use anyhow::Result;
use serde_json::Value;
use tokio::time::sleep;

use super::{EXIT_BUSY, access};
use crate::{
    cli::{context::Context, output::Style},
    daemon::access::{AccessRequest, DecisionStatus},
    protocol::Body,
};

#[derive(Debug)]
pub(super) enum Acquisition<T> {
    Acquired(T),
    Busy(String),
    ApprovalPending(Box<AccessRequest>),
    ApprovalDenied(Box<AccessRequest>),
}

impl<T> Acquisition<T> {
    fn approval(request: Box<AccessRequest>) -> Self {
        if request.status == DecisionStatus::Denied {
            Self::ApprovalDenied(request)
        } else {
            Self::ApprovalPending(request)
        }
    }

    /// Domain handlers supply their successful payload and capacity details;
    /// approval rendering and exit status are shared by every acquisition.
    pub(super) fn finish(
        self,
        ctx: &Context,
        acquired: impl FnOnce(T) -> Result<()>,
        busy: impl FnOnce(&str) -> Value,
    ) -> Result<i32> {
        match self {
            Self::Acquired(value) => {
                acquired(value)?;
                Ok(0)
            }
            Self::ApprovalPending(request) | Self::ApprovalDenied(request) => {
                access::declined(ctx, &request)
            }
            Self::Busy(message) => {
                ctx.emit_styled(Style::Warning, &message, busy(&message))?;
                Ok(EXIT_BUSY)
            }
        }
    }
}

impl<T: TryFrom<Body, Error = anyhow::Error>> TryFrom<Body> for Acquisition<T> {
    type Error = anyhow::Error;

    fn try_from(body: Body) -> Result<Self> {
        match body {
            Body::AccessRequest(request) => Ok(Self::approval(request)),
            Body::Busy { message } => Ok(Self::Busy(message)),
            body => T::try_from(body).map(Self::Acquired),
        }
    }
}

/// Attempt once, or wait in the queue with elapsed-time feedback on a terminal.
pub(super) async fn obtain<T>(
    ctx: &Context,
    subject: &str,
    queue: bool,
    attempt: impl AsyncFnMut() -> Result<Acquisition<T>>,
) -> Result<Acquisition<T>> {
    if queue {
        let message = format!("Waiting for {subject}");
        ctx.progress(&message, retry(true, attempt)).await
    } else {
        retry(false, attempt).await
    }
}

/// Without a queue, return the first outcome. A queued request polls capacity
/// and pending approvals about once a second, keeping its place, until
/// acquisition, denial or an error.
async fn retry<T>(
    queue: bool,
    mut attempt: impl AsyncFnMut() -> Result<Acquisition<T>>,
) -> Result<Acquisition<T>> {
    loop {
        let outcome = attempt().await?;
        match outcome {
            Acquisition::Busy(_) | Acquisition::ApprovalPending(_) if queue => {
                sleep(Duration::from_secs(1)).await;
            }
            _ => return Ok(outcome),
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
}
