//! The text shown for a request: the user's notifications and the agent
//! messages that ask for or report a decision.
use super::{ApprovalRequest, DecisionStatus, GateStep};

/// The text shown for a request to the user and to agents.
pub(super) trait RequestText {
    /// What the user is told about a request no coordinator decides,
    /// including how to approve it.
    fn notification(&self) -> String;

    /// What its decider is told about a withdrawn request whose
    /// notification was already shown.
    fn withdrawal_notification(&self) -> String;

    /// What the coordinating agent the request is routed to is told,
    /// including how to decide it or pass it up.
    fn routed_message(&self) -> String;

    /// The agent message naming a decision and what the requesting agent does next.
    fn decision_message(&self) -> String;
}

impl RequestText for ApprovalRequest {
    fn notification(&self) -> String {
        format!(
            "approval {}: {}{}{}; approve with shoal approvals --approve {}",
            self.number,
            self.subject(),
            reason_suffix(self),
            gate_suffix(self),
            self.number
        )
    }

    fn withdrawal_notification(&self) -> String {
        format!(
            "approval {}: {} was withdrawn; no decision needed",
            self.number,
            self.subject()
        )
    }

    fn routed_message(&self) -> String {
        format!(
            "{}: approval {id}: {}: {}{}; decide with shoal approvals --approve {id} or shoal approvals --deny {id}, or pass it up with shoal approvals --escalate {id}",
            self.space_name(),
            self.subject(),
            self.reason,
            gate_suffix(self),
            id = self.number
        )
    }

    fn decision_message(&self) -> String {
        let next = match self.status {
            DecisionStatus::Approved => "retry the command that requested it",
            _ => "do not retry it",
        };
        format!(
            "Approval {} for {} was {}: {next}.",
            self.number,
            self.subject(),
            self.status
        )
    }
}

/// `: <reason>` for a request that gives one; proposals give none.
fn reason_suffix(request: &ApprovalRequest) -> String {
    match request.reason.as_str() {
        "" => String::new(),
        reason => format!(": {reason}"),
    }
}

/// What the gate did with a request that reaches its decider from it: why
/// it passed it on and the case it quoted, why carrying out its allow
/// failed, or why it did not judge it.
fn gate_suffix(request: &ApprovalRequest) -> String {
    match &request.gate {
        Some(GateStep::Escalated { reason, rule }) => {
            format!(" (the gate passed it on: {reason}; case: {rule})")
        }
        Some(GateStep::Unapplied { error }) => format!(" (the gate allowed it, then: {error})"),
        Some(GateStep::Unjudged { reason }) => format!(" (the gate did not judge it: {reason})"),
        _ => String::new(),
    }
}
