//! The gate agent, which decides the approval requests a permission of
//! `gate` governs, as `docs/design/reviews.md` describes it.
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use anyhow::{Result, bail};
use shoal_daemon_base::notifications::About;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::alerts::AlertKind;
use shoal_protocol::approvals::{ApprovalRequest, DecisionStatus, GateStep};
use tokio::task::{Id, JoinError, JoinSet};

use crate::{Reviews, daemon::GateDaemon, log};

mod context;
mod leftovers;
mod outcome;
pub mod prompt;
mod run;
pub mod store;
pub mod subject;
pub mod verdict;

/// The denials of one action after which it is refused without asking the
/// gate again, until the action changes.
const DENIALS: u32 = 3;

/// The gate's runs.
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
pub async fn run(reviews: Arc<Reviews>, daemon: Arc<dyn GateDaemon>) {
    reviews.carry_out_stranded_allows(&*daemon).await;
    let mut runs = Runs::default();
    loop {
        let queued = reviews.gate.queued.notified();
        tokio::pin!(queued);
        queued.as_mut().enable();
        let requests = reviews.requests.clone();
        match reviews.store.read(move |tx| requests.deciding(tx)).await {
            Ok(deciding) => runs.start(&reviews, &daemon, deciding),
            Err(error) => log!("requests for the gate: {error:#}"),
        }
        tokio::select! {
            () = &mut queued => {}
            Some(ended) = runs.running.join_next_with_id(), if !runs.running.is_empty() => {
                runs.end(&reviews, &*daemon, ended).await;
            }
        }
    }
}

/// The gate's runs in progress, one for each request it decides.
#[derive(Default)]
struct Runs {
    running: JoinSet<()>,
    /// The request each run decides, by its task.
    requests: HashMap<Id, ApprovalRequest>,
}

impl Runs {
    /// Start a run for each of `requests` that has none.
    fn start(
        &mut self,
        reviews: &Arc<Reviews>,
        daemon: &Arc<dyn GateDaemon>,
        requests: Vec<ApprovalRequest>,
    ) {
        for request in requests {
            if self.requests.values().any(|run| run.key == request.key) {
                continue;
            }
            let (reviews, daemon, decided) = (reviews.clone(), daemon.clone(), request.clone());
            self.spawn(request, async move {
                reviews.decide_by_gate(&*daemon, decided).await
            });
        }
    }

    /// Run `run`, which decides `request`.
    fn spawn(&mut self, request: ApprovalRequest, run: impl Future<Output = ()> + Send + 'static) {
        let task = self.running.spawn(run);
        self.requests.insert(task.id(), request);
    }

    /// Forget the run that `ended`; one that panicked denies its request as
    /// a failed run does, which would otherwise wait for the gate, out of
    /// the inbox, until a restart asked it again.
    async fn end(
        &mut self,
        reviews: &Reviews,
        daemon: &dyn GateDaemon,
        ended: Result<(Id, ()), JoinError>,
    ) {
        let id = match &ended {
            Ok((id, ())) => *id,
            Err(error) => error.id(),
        };
        let Some(request) = self.requests.remove(&id) else {
            return;
        };
        if let Err(error) = ended
            && error.is_panic()
        {
            log!("approval {}: the gate's run panicked", request.number);
            let failure = "the gate's run failed unexpectedly".to_owned();
            (reviews.conclude_gate_failure(daemon, &request, None, failure)).await;
        }
    }
}

impl Reviews {
    /// Give the gate `request`, pending and not yet routed, when the
    /// requesting space's permissions for what governs it narrow to `gate`;
    /// false leaves it to the normal route. A request the gate has is the
    /// gate's until it decides or passes it on.
    pub async fn give_gate(
        &self,
        daemon: &dyn GateDaemon,
        request: &ApprovalRequest,
    ) -> Result<bool> {
        match &request.gate {
            Some(GateStep::Deciding) => return Ok(true),
            Some(
                GateStep::Allowed
                | GateStep::Unapplied { .. }
                | GateStep::Unjudged { .. }
                | GateStep::Escalated { .. },
            ) => return Ok(false),
            None if request.routed || !is_gated(daemon, request).await? => return Ok(false),
            None => {}
        }
        let (key, requests) = (request.key.clone(), self.requests.clone());
        let given = self.store.write(move |tx| requests.give(tx, &key)).await?;
        if given {
            self.gate.queued.notify_one();
        }
        Ok(given)
    }

    /// Carry out the requests the gate allowed whose approval a stopped
    /// daemon never started.
    pub async fn carry_out_stranded_allows(&self, daemon: &dyn GateDaemon) {
        let requests = self.requests.clone();
        let stranded = self.store.read(move |tx| requests.stranded(tx)).await;
        match stranded {
            Ok(stranded) => {
                for allowed in &stranded {
                    self.carry_out_allow(daemon, allowed).await;
                }
            }
            Err(error) => log!("requests the gate allowed: {error:#}"),
        }
    }

    /// Refuse `request` without asking the gate when the gate decides it and
    /// has denied the same action as often as [`DENIALS`]: until the action
    /// changes, asking again would not change the answer.
    pub async fn ensure_gate_hears(
        &self,
        daemon: &dyn GateDaemon,
        request: &ApprovalRequest,
    ) -> Result<()> {
        if !is_gated(daemon, request).await? {
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
    async fn decide_by_gate(&self, daemon: &dyn GateDaemon, request: ApprovalRequest) {
        let Ok(space) = daemon.space(&request.space_id).await else {
            return self.pass_on_unjudged(daemon, &request).await;
        };
        let branch = space.worktree().and_then(|workspace| workspace.branch());
        let subject = subject::Subject::of(&request, branch).key();
        // Taken before anything slow, so a session's requests keep their order.
        let session = (self.gate.session(&request.space_id, &subject).lock_owned()).await;
        if self.gate_still_decides(&request).await {
            self.decide_in_session(daemon, &request).await;
        }
        drop(session);
        self.gate.release(&request.space_id, &subject);
    }

    /// Have the gate decide `request` while its session is held: read its
    /// case, ask the gate and carry out the verdict, or deny it when the
    /// case cannot be read; one whose space is gone, before or after the
    /// gate ran, takes the normal route.
    async fn decide_in_session(&self, daemon: &dyn GateDaemon, request: &ApprovalRequest) {
        // The space may have gone while the session's earlier requests ran.
        if daemon.space(&request.space_id).await.is_err() {
            return self.pass_on_unjudged(daemon, request).await;
        }
        let case = match context::case(daemon, &self.forges, request).await {
            Ok(case) => case,
            Err(error) => return self.deny_unreadable(daemon, request, error).await,
        };
        let consulted = self.consult_gate(daemon, request, &case).await;
        // The space may have gone while the gate ran.
        match daemon.space(&request.space_id).await {
            Ok(_) => self.conclude_gate(daemon, request, &case, consulted).await,
            Err(_) => self.pass_on_unjudged(daemon, request).await,
        }
    }

    /// Deny `request`, whose case could not be read for `error`, telling the
    /// user when the configuration names no agent the gate can run, before
    /// the denial reaches the space's agent.
    async fn deny_unreadable(
        &self,
        daemon: &dyn GateDaemon,
        request: &ApprovalRequest,
        error: anyhow::Error,
    ) {
        log!(
            "approval {}: the gate cannot decide it: {error:#}",
            request.number
        );
        if error.downcast_ref::<context::Unavailable>().is_some() {
            report_unavailable_gate(daemon, request, &error).await;
        }
        let failure = format!("the gate could not be set up: {error:#}");
        (self.conclude_gate_failure(daemon, request, None, failure)).await;
    }

    /// Give `request`, whose space is gone, as an issue proposal outlives it,
    /// to the normal route: the gate judges a space's work, and nobody is
    /// left to tell about a denial.
    async fn pass_on_unjudged(&self, daemon: &dyn GateDaemon, request: &ApprovalRequest) {
        let (key, requests) = (request.key.clone(), self.requests.clone());
        let unjudged = self
            .store
            .write(move |tx| requests.unjudged(tx, &key, "its space is gone"))
            .await;
        match unjudged {
            Ok(unjudged) => daemon.announce(&unjudged).await,
            Err(error) => log!("approval {}: {error:#}", request.number),
        }
    }

    /// Whether `request` is still pending with the gate, neither decided
    /// nor withdrawn while its session's earlier requests ran.
    async fn gate_still_decides(&self, request: &ApprovalRequest) -> bool {
        let (key, requests) = (request.key.clone(), self.requests.clone());
        match self.store.run(move |db| requests.request(db, &key)).await {
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

/// Whether the narrowest of the requesting space's permissions that govern
/// `request` is `gate`, in the repository of its action.
async fn is_gated(daemon: &dyn GateDaemon, request: &ApprovalRequest) -> Result<bool> {
    let governing = request.governing();
    if governing.is_empty() {
        return Ok(false);
    }
    let space = daemon.space(&request.space_id).await?;
    let repository = (request.repository_id.as_ref()).or(space.repository_id.as_ref());
    let permissions = daemon.permissions(&request.space_id, repository).await?;
    Ok(permissions.gated(&governing))
}

/// Tell the user that the gate could not be set up for `request`, a problem
/// of the configuration only the user fixes; a run that fails is the
/// agent's to report.
async fn report_unavailable_gate(
    daemon: &dyn GateDaemon,
    request: &ApprovalRequest,
    error: &anyhow::Error,
) {
    let about = About {
        name: request.space.clone(),
        id: Some(request.space_id.clone()),
        subject: None,
    };
    let message = format!(
        "the gate could not decide approval {}, which was denied: {error:#}; set [gate] agent \
         or agent.default to an AI tool with a review template, then run shoal config reload",
        request.number
    );
    let kind = AlertKind::GateUnavailable;
    daemon.notify(about, kind, message).await;
}

#[cfg(test)]
mod tests;
