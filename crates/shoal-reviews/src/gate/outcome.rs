//! Carrying out the gate's verdict: an allowed request is approved through
//! the path an approval takes, a denied one ends with the reason, and a
//! passed one takes the normal route. Each verdict goes to the requesting
//! space's log and agent, and as one line to its coordinator.
use super::{
    DENIALS,
    context::Case,
    run::Consulted,
    store::{self, Session},
    verdict::Verdict,
};
use crate::{Reviews, agent::Agent, daemon::GateDaemon, log};
use shoal_daemon_base::events;
use shoal_ids::agent_session_ids::AgentSessionId;
use shoal_protocol::{
    alerts::AlertKind,
    approvals::{ApprovalRequest, GateVerdict},
    events::GateEvent,
    time::UnixTime,
};

/// A verdict as the gate's records keep it.
struct Recorded<'a> {
    kind: GateVerdict,
    reason: &'a str,
    rule: Option<&'a str>,
    agent: Option<&'a Agent>,
    session: Option<&'a AgentSessionId>,
}

impl Reviews {
    /// Carry out what the gate made of `request`.
    pub(super) async fn conclude_gate(
        &self,
        daemon: &dyn GateDaemon,
        request: &ApprovalRequest,
        case: &Case,
        consulted: Consulted,
    ) {
        let (verdict, agent, session) = match consulted {
            Consulted::Decided {
                verdict,
                agent,
                session,
            } => (verdict, agent, session),
            Consulted::Failed(failure) => {
                return self
                    .conclude_gate_failure(daemon, request, Some(case), failure)
                    .await;
            }
        };
        if agent == case.agent
            && let Some(session) = &session
        {
            self.keep_gate_session(request, case, session).await;
        }
        let recorded = |kind, rule| Recorded {
            kind,
            reason: verdict.reason(),
            rule,
            agent: Some(&agent),
            session: session.as_ref(),
        };
        match &verdict {
            Verdict::Allow { .. } => {
                (self.gate_allows(daemon, request, case, recorded(GateVerdict::Allow, None))).await
            }
            Verdict::Deny { .. } => {
                let recorded = recorded(GateVerdict::Deny, None);
                self.gate_denies(daemon, request, &case.action, recorded)
                    .await
            }
            Verdict::Escalate { rule, .. } => {
                let recorded = recorded(GateVerdict::Escalate, Some(rule));
                self.gate_escalates(daemon, request, &case.action, recorded)
                    .await
            }
        }
    }

    /// Deny `request`, which the gate could not decide because of `failure`;
    /// such a denial does not count toward the ones that stop asking.
    pub(super) async fn conclude_gate_failure(
        &self,
        daemon: &dyn GateDaemon,
        request: &ApprovalRequest,
        case: Option<&Case>,
        failure: String,
    ) {
        let action = case.map_or_else(
            || super::subject::action(request),
            |case| case.action.clone(),
        );
        let recorded = Recorded {
            kind: GateVerdict::Failed,
            reason: &failure,
            rule: None,
            agent: case.map(|case| &case.agent),
            session: None,
        };
        self.gate_denies(daemon, request, &action, recorded).await
    }

    /// Keep `session` as the one the request's subject resumes, having seen
    /// the commit the request binds.
    async fn keep_gate_session(
        &self,
        request: &ApprovalRequest,
        case: &Case,
        session: &AgentSessionId,
    ) {
        let (space, subject) = (request.space_id.clone(), case.subject.key());
        let session = Session {
            agent: case.agent.clone(),
            rules: case.opened_with.clone(),
            session: session.clone(),
            seen: (case.subject.follows_commits())
                .then(|| case.head.as_ref().map(ToString::to_string))
                .flatten(),
        };
        let now = UnixTime::now();
        let saved = self
            .store
            .write(move |tx| store::save_session(tx, &space, &subject, &session, now))
            .await;
        if let Err(error) = saved {
            log!("approval {}: the gate's session: {error:#}", request.number);
        }
    }

    /// Approve `request`, which the gate allowed, as the user's approval
    /// would.
    async fn gate_allows(
        &self,
        daemon: &dyn GateDaemon,
        request: &ApprovalRequest,
        case: &Case,
        recorded: Recorded<'_>,
    ) {
        let (key, action, kind) = (request.key.clone(), case.action.clone(), recorded.kind);
        let (gate, requests) = (gate_event(request, &recorded), self.requests.clone());
        let allowed = self
            .store
            .write(move |tx| {
                let allowed = requests.allow(tx, &key)?;
                record(tx, &allowed, &action, kind, &gate)?;
                Ok(allowed)
            })
            .await;
        let allowed = match allowed {
            Ok(allowed) => allowed,
            Err(error) => return log!("approval {}: {error:#}", request.number),
        };
        if self.carry_out_allow(daemon, &allowed).await {
            (self.tell_gate_coordinator(daemon, &allowed, "allowed", recorded.reason)).await;
        }
    }

    /// Approve `allowed`, which the gate allowed, as the user's approval
    /// would; false when that failed, which hands it to the normal route.
    pub(super) async fn carry_out_allow(
        &self,
        daemon: &dyn GateDaemon,
        allowed: &ApprovalRequest,
    ) -> bool {
        match daemon.approve(allowed).await {
            Ok(()) => true,
            Err(error) => {
                self.pass_on_unapplied(daemon, allowed, error).await;
                false
            }
        }
    }

    /// Give an allowed request whose approval could not be carried out, and
    /// that is still pending, to the normal route, telling its agent why; one
    /// the failure outdated was answered already.
    async fn pass_on_unapplied(
        &self,
        daemon: &dyn GateDaemon,
        allowed: &ApprovalRequest,
        error: anyhow::Error,
    ) {
        log!("approval {}: allowed, then {error:#}", allowed.number);
        let (key, failure) = (allowed.key.clone(), format!("{error:#}"));
        let requests = self.requests.clone();
        let marked = self
            .store
            .write(move |tx| requests.unapplied(tx, &key, &failure))
            .await;
        let pending = match marked {
            Ok(Some(pending)) => pending,
            Ok(None) => return,
            Err(error) => return log!("approval {}: {error:#}", allowed.number),
        };
        let message = unapplied_message(&pending, &error);
        (daemon.tell_space(&pending.space_id, AlertKind::GateDecided, message)).await;
        daemon.announce(&pending).await;
    }

    /// End `request` as the gate denied it, or as its failure did, and tell
    /// the agent why and that it may ask again once it changed what the
    /// reason names.
    async fn gate_denies(
        &self,
        daemon: &dyn GateDaemon,
        request: &ApprovalRequest,
        action: &str,
        recorded: Recorded<'_>,
    ) {
        let (key, action, kind) = (request.key.clone(), action.to_owned(), recorded.kind);
        let (gate, requests) = (gate_event(request, &recorded), self.requests.clone());
        let denied = self
            .store
            .write(move |tx| {
                let denied = requests.deny(tx, &key)?;
                record(tx, &denied, &action, kind, &gate)?;
                Ok(denied)
            })
            .await;
        let denied = match denied {
            Ok(denied) => denied,
            Err(error) => return log!("approval {}: {error:#}", request.number),
        };
        if denied.target.is_proposal() {
            daemon.prune_proposal_files(&denied).await;
        }
        let message = denial_message(&denied, &recorded);
        (daemon.tell_space(&denied.space_id, AlertKind::GateDecided, message)).await;
        let verb = denial_verb(recorded.kind);
        self.tell_gate_coordinator(daemon, &denied, verb, recorded.reason)
            .await;
    }

    /// Pass `request` on for the case `recorded` quotes: it takes the
    /// normal route, to the nearest space above with an agent or the user.
    async fn gate_escalates(
        &self,
        daemon: &dyn GateDaemon,
        request: &ApprovalRequest,
        action: &str,
        recorded: Recorded<'_>,
    ) {
        let (key, action, kind) = (request.key.clone(), action.to_owned(), recorded.kind);
        let (reason, rule) = (
            recorded.reason.to_owned(),
            recorded.rule.unwrap_or_default(),
        );
        let rule = rule.to_owned();
        let (gate, requests) = (gate_event(request, &recorded), self.requests.clone());
        let escalated = self
            .store
            .write(move |tx| {
                let escalated = requests.escalate(tx, &key, &reason, &rule)?;
                record(tx, &escalated, &action, kind, &gate)?;
                Ok(escalated)
            })
            .await;
        let escalated = match escalated {
            Ok(escalated) => escalated,
            Err(error) => return log!("approval {}: {error:#}", request.number),
        };
        daemon.announce(&escalated).await;
        let message = escalation_message(&escalated, recorded.reason);
        (daemon.tell_space(&escalated.space_id, AlertKind::GateDecided, message)).await;
    }

    /// Tell the coordinator of the request's space, in one line, that the
    /// gate `did` the request for `reason`.
    async fn tell_gate_coordinator(
        &self,
        daemon: &dyn GateDaemon,
        request: &ApprovalRequest,
        did: &str,
        reason: &str,
    ) {
        let Some(name) = &request.space else {
            return;
        };
        let line = format!(
            "{name}: the gate {did} approval {} to {}: {reason}",
            request.number,
            request.subject()
        );
        let kind = AlertKind::GateDecided;
        (daemon.tell_coordinator(name, &request.space_id, kind, line)).await;
    }
}

/// What the agent is told of `denied`, which the gate denied, or which its
/// failure denied, with the reason `recorded` gives.
fn denial_message(denied: &ApprovalRequest, recorded: &Recorded) -> String {
    let (number, subject, reason) = (denied.number, denied.subject(), recorded.reason);
    match recorded.kind {
        GateVerdict::Failed => format!(
            "The gate could not decide approval {number} to {subject}, so it is denied: \
             {reason}. Run the command again to ask again; tell the user with shoal send \
             or ask with shoal questions ask."
        ),
        _ => format!(
            "The gate denied approval {number} to {subject}: {reason}. Change what it names \
             and run the command again; after {DENIALS} denials of the same action it is \
             refused until the action changes."
        ),
    }
}

/// What the gate did to a request it denied, or failed to decide, in the
/// coordinator's line.
fn denial_verb(kind: GateVerdict) -> &'static str {
    match kind {
        GateVerdict::Failed => "could not decide, so denied,",
        _ => "denied",
    }
}

/// What the agent is told of `escalated`, which the gate passed on for
/// `reason`.
fn escalation_message(escalated: &ApprovalRequest, reason: &str) -> String {
    format!(
        "The gate passed approval {} to {} on for a decision: {reason}. Wait for it.",
        escalated.number,
        escalated.subject()
    )
}

/// What the agent is told of `pending`, which the gate allowed but whose
/// approval failed with `error`.
fn unapplied_message(pending: &ApprovalRequest, error: &anyhow::Error) -> String {
    format!(
        "The gate allowed approval {} to {}, but carrying it out failed: {error:#}. It waits \
         for a decision above.",
        pending.number,
        pending.subject()
    )
}

/// The log entry of the gate's verdict on `request`.
fn gate_event(request: &ApprovalRequest, recorded: &Recorded) -> GateEvent {
    GateEvent {
        number: request.number,
        subject: request.subject(),
        verdict: recorded.kind,
        reason: recorded.reason.to_owned(),
        rule: recorded.rule.map(ToOwned::to_owned),
        agent: recorded.agent.map(|agent| agent.name().to_owned()),
        session: recorded.session.map(|session| session.as_str().to_owned()),
    }
}

/// Record the verdict `gate`, of `kind`, on `request`, the action `action`,
/// in its space's verdicts and log.
fn record(
    tx: &rusqlite::Transaction<'_>,
    request: &ApprovalRequest,
    action: &str,
    kind: GateVerdict,
    gate: &GateEvent,
) -> anyhow::Result<()> {
    let now = UnixTime::now();
    store::record_verdict(tx, &request.space_id, action, kind, &gate.reason, now)?;
    events::record_gate(tx, request, gate)
}
