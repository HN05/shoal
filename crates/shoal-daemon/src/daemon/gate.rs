//! The gate: an agent that decides the approval requests a permission of
//! `gate` governs before anyone else sees them. Each request runs it once
//! in the background, read-only in its repository's checkout, continuing
//! the session of the request's subject; it allows the request, denies it
//! with a reason, or passes it on for a case the user wrote down.
use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex},
};

use anyhow::{Result, bail};
use tokio::task::JoinSet;

use crate::client::space_ids::SpaceId;
use crate::daemon::{
    approvals::{ApprovalRequest, DecisionStatus, GateStep},
    log,
    workspace::Manager,
};

mod context;
mod leftovers;
mod outcome;
mod prompt;
mod run;
mod store;
mod subject;
mod verdict;

/// The denials of one action after which it is refused without asking the
/// gate again, until the action changes.
const DENIALS: u32 = 3;

/// The daemon's gate runs.
#[derive(Default)]
pub(crate) struct Gatekeeper {
    /// Wakes the scheduler when a request goes to the gate.
    queued: tokio::sync::Notify,
    /// One lock per space and subject, so the requests of a session run one
    /// after another.
    sessions: Mutex<HashMap<(SpaceId, String), SessionLock>>,
}

/// Held while one request of a session runs.
type SessionLock = Arc<tokio::sync::Mutex<()>>;

impl Gatekeeper {
    /// The lock of the session of `space` for `subject`.
    fn session(&self, space: &SpaceId, subject: &str) -> SessionLock {
        let mut sessions = self.sessions.lock().unwrap();
        let key = (space.clone(), subject.to_owned());
        sessions.entry(key).or_default().clone()
    }

    /// Forget the lock of the session of `space` for `subject` once no
    /// request holds or waits for it.
    fn release(&self, space: &SpaceId, subject: &str) {
        let mut sessions = self.sessions.lock().unwrap();
        let key = (space.clone(), subject.to_owned());
        if sessions
            .get(&key)
            .is_some_and(|lock| Arc::strong_count(lock) == 1)
        {
            sessions.remove(&key);
        }
    }
}

/// Run the gate on every request it is deciding, now and whenever one goes
/// to it, as after a restart, which first carries out the allows it cut
/// short.
pub(crate) async fn run(manager: Arc<Manager>) {
    manager.carry_out_stranded_allows().await;
    let mut running = JoinSet::new();
    let mut started = HashSet::new();
    loop {
        let queued = manager.gate.queued.notified();
        tokio::pin!(queued);
        queued.as_mut().enable();
        match manager
            .store
            .read(|tx| crate::daemon::approvals::gated::deciding(tx))
            .await
        {
            Ok(requests) => {
                for request in requests
                    .into_iter()
                    .filter(|r| started.insert(r.key.clone()))
                {
                    let manager = manager.clone();
                    running.spawn(async move {
                        let key = request.key.clone();
                        manager.decide_by_gate(request).await;
                        key
                    });
                }
            }
            Err(error) => log!("requests for the gate: {error:#}"),
        }
        tokio::select! {
            () = &mut queued => {}
            Some(ended) = running.join_next(), if !running.is_empty() => {
                if let Ok(key) = ended {
                    started.remove(&key);
                }
            }
        }
    }
}

impl Manager {
    /// Give the gate `request`, pending and not yet routed, when the
    /// requesting space's permissions for what governs it narrow to `gate`;
    /// false leaves it to the normal route. A request the gate has is the
    /// gate's until it decides or passes it on.
    pub(crate) async fn give_gate(&self, request: &ApprovalRequest) -> Result<bool> {
        match &request.gate {
            Some(GateStep::Deciding) => return Ok(true),
            Some(
                GateStep::Allowed
                | GateStep::Unapplied { .. }
                | GateStep::Unjudged { .. }
                | GateStep::Escalated { .. },
            ) => return Ok(false),
            None if request.routed || !self.is_gated(request).await? => return Ok(false),
            None => {}
        }
        let key = request.key.clone();
        let given = self
            .store
            .write(move |tx| crate::daemon::approvals::gated::give_gate(tx, &key))
            .await?;
        if given {
            self.gate.queued.notify_one();
        }
        Ok(given)
    }

    /// Whether the narrowest of the requesting space's permissions that
    /// govern `request` is `gate`, in the repository of its action.
    async fn is_gated(&self, request: &ApprovalRequest) -> Result<bool> {
        let governing = crate::daemon::approvals::governing(request);
        if governing.is_empty() {
            return Ok(false);
        }
        let space = self.space_by_id(&request.space_id).await?;
        let repository = (request.repository_id.as_ref()).or(space.repository_id());
        let permissions = self
            .space_permissions(&request.space_id, repository)
            .await?;
        Ok(permissions.gated(&governing))
    }

    /// Carry out the requests the gate allowed whose approval a stopped
    /// daemon never started.
    pub(crate) async fn carry_out_stranded_allows(&self) {
        let stranded = self
            .store
            .read(|tx| crate::daemon::approvals::gated::stranded(tx))
            .await;
        match stranded {
            Ok(stranded) => {
                for allowed in &stranded {
                    self.carry_out_allow(allowed).await;
                }
            }
            Err(error) => log!("requests the gate allowed: {error:#}"),
        }
    }

    /// Refuse `request` without asking the gate when the gate decides it and
    /// has denied the same action as often as [`DENIALS`]: until the action
    /// changes, asking again would not change the answer.
    pub(crate) async fn ensure_gate_hears(&self, request: &ApprovalRequest) -> Result<()> {
        if !self.is_gated(request).await? {
            return Ok(());
        }
        let (space, action) = (request.space_id.clone(), subject::action(request));
        let (denied, reason) = self
            .store
            .read(move |tx| store::denials(tx, &space, &action))
            .await?;
        if denied >= DENIALS {
            bail!(
                "the gate denied {} {denied} times, last because: {reason}; change it before \
                 asking again",
                request.subject()
            );
        }
        Ok(())
    }

    /// Have the gate decide `request`, after the requests of its session
    /// that came first, unless it was decided or withdrawn meanwhile.
    async fn decide_by_gate(&self, request: ApprovalRequest) {
        let Ok(space) = self.space_by_id(&request.space_id).await else {
            return self.pass_on_unjudged(&request).await;
        };
        let branch = space
            .worktree()
            .and_then(|workspace| workspace.branch.as_ref());
        let subject = subject::Subject::of(&request, branch).key();
        // Taken before anything slow, so a session's requests keep their order.
        let session = (self.gate.session(&request.space_id, &subject).lock_owned()).await;
        if self.gate_still_decides(&request).await {
            self.decide_in_session(&request).await;
        }
        drop(session);
        self.gate.release(&request.space_id, &subject);
    }

    /// Have the gate decide `request` while its session is held: read its
    /// case, ask the gate and carry out the verdict, or deny it when the
    /// case cannot be read; one whose space is gone, before or after the
    /// gate ran, takes the normal route.
    async fn decide_in_session(&self, request: &ApprovalRequest) {
        // The space may have gone while the session's earlier requests ran.
        if self.space_by_id(&request.space_id).await.is_err() {
            return self.pass_on_unjudged(request).await;
        }
        let case = match self.gate_case(request).await {
            Ok(case) => case,
            Err(error) => return self.deny_unreadable(request, error).await,
        };
        let consulted = self.consult_gate(request, &case).await;
        // The space may have gone while the gate ran.
        match self.space_by_id(&request.space_id).await {
            Ok(_) => self.conclude_gate(request, &case, consulted).await,
            Err(_) => self.pass_on_unjudged(request).await,
        }
    }

    /// Deny `request`, whose case could not be read for `error`, telling the
    /// user when the configuration names no agent the gate can run.
    async fn deny_unreadable(&self, request: &ApprovalRequest, error: anyhow::Error) {
        log!(
            "approval {}: the gate cannot decide it: {error:#}",
            request.number
        );
        let failure = format!("the gate could not be set up: {error:#}");
        self.conclude_gate_failure(request, None, failure).await;
        if error.downcast_ref::<context::Unavailable>().is_some() {
            self.report_unavailable_gate(request, &error).await;
        }
    }

    /// Give `request`, whose space is gone, as an issue proposal outlives it,
    /// to the normal route: the gate judges a space's work, and nobody is
    /// left to tell about a denial.
    async fn pass_on_unjudged(&self, request: &ApprovalRequest) {
        let key = request.key.clone();
        let unjudged = self
            .store
            .write(move |tx| {
                crate::daemon::approvals::gated::unjudged(tx, &key, "its space is gone")
            })
            .await;
        match unjudged {
            Ok(unjudged) => self.notify_approval(&unjudged).await,
            Err(error) => log!("approval {}: {error:#}", request.number),
        }
    }

    /// Tell the user that the gate could not be set up for `request`, a
    /// problem of the configuration only the user fixes; a run that fails is
    /// the agent's to report.
    async fn report_unavailable_gate(&self, request: &ApprovalRequest, error: &anyhow::Error) {
        let about = crate::daemon::notifications::About {
            name: request.space.clone(),
            id: Some(request.space_id.clone()),
            subject: None,
        };
        let message = format!(
            "the gate could not decide approval {}, which was denied: {error:#}; set [gate] agent \
             or agent.default to an AI tool with a review template, then run shoal config reload",
            request.number
        );
        let kind = crate::daemon::notifications::NotificationKind::GateUnavailable;
        self.notify_about(about, kind, message).await;
    }

    /// Whether `request` is still pending with the gate, neither decided
    /// nor withdrawn while its session's earlier requests ran.
    async fn gate_still_decides(&self, request: &ApprovalRequest) -> bool {
        match self.approval_by_key(&request.key).await {
            Ok(Some(current)) => {
                current.status == DecisionStatus::Pending
                    && current.gate == Some(GateStep::Deciding)
            }
            Ok(None) => false,
            Err(error) => {
                log!("approval {}: {error:#}", request.number);
                false
            }
        }
    }
}

#[cfg(test)]
mod tests;
