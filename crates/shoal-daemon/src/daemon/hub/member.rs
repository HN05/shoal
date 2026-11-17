//! A member's side of its link to the hub. The daemon of a machine that
//! joined a hub keeps one link to it, over `[machine.hub] connect` when set
//! and otherwise to the URL it paired at, and opens it again with backoff
//! once it is lost, as after sleep or a network change. A refusal it cannot
//! outwait, such as of a revoked credential, stops it until the user acts.
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use anyhow::{Result, anyhow};
use shoal_protocol::hub::{
    HubRole, Membership, Route,
    link::{Call, Intent, Refusal, RefusalReason, Reply},
};
use tokio::sync::Notify;

use super::{
    identity::Identity,
    membership::{self, Joined},
    session::{self, Calls, End},
};
use crate::{
    daemon::{log, notifications::NotificationKind, workspace::Manager},
    refusal::{Reason, Refusal as Refused},
};

mod connect;
mod join;
mod open;

use connect::Transport;
use open::{Expected, Opened, Plan};

/// The first wait before opening a lost link again, which doubles up to
/// [`MAX_BACKOFF`].
const FIRST_BACKOFF: Duration = Duration::from_secs(1);
const MAX_BACKOFF: Duration = Duration::from_secs(5 * 60);

/// This machine's link to the hub, while it is a member of one.
#[derive(Default)]
pub(super) struct Link {
    state: Mutex<State>,
    /// Wakes the link to read its membership and route again.
    wake: Notify,
}

#[derive(Default)]
struct State {
    /// The calls to the hub, while the link is open.
    calls: Option<Arc<Calls>>,
    /// Why the link is down.
    problem: Option<String>,
}

impl Link {
    fn calls(&self) -> Option<Arc<Calls>> {
        self.state.lock().unwrap().calls.clone()
    }

    fn set(&self, calls: Option<Arc<Calls>>, problem: Option<String>) {
        *self.state.lock().unwrap() = State { calls, problem };
    }
}

/// What the link does next.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Attempt {
    /// Nothing, for why when it is a problem.
    Idle(Option<String>),
    /// Connect as the member `joined` records, by `route`.
    Connect { joined: Box<Joined>, route: Route },
}

/// Keep this machine's link to the hub it joined, until the daemon stops.
pub(super) async fn run(manager: Arc<Manager>) {
    let mut reloaded = manager.config_state.watch();
    let mut backoff = FIRST_BACKOFF;
    loop {
        let attempt = manager.link_attempt().await;
        let retry = match attempt {
            Ok(Attempt::Idle(problem)) => {
                manager.hub.member.set(None, problem);
                None
            }
            Ok(attempt) => match manager.keep_link(attempt).await {
                Kept::Served => {
                    backoff = FIRST_BACKOFF;
                    Some(backoff)
                }
                Kept::Failed => Some(backoff),
                Kept::Stopped => None,
            },
            Err(error) => {
                log!("hub: {error:#}");
                manager.hub.member.set(None, Some(format!("{error:#}")));
                Some(backoff)
            }
        };
        if let Some(delay) = retry {
            backoff = (backoff * 2).min(MAX_BACKOFF);
            tokio::select! {
                () = tokio::time::sleep(delay) => continue,
                _ = reloaded.changed() => {}
                () = manager.hub.member.wake.notified() => {}
            }
        } else {
            tokio::select! {
                _ = reloaded.changed() => {}
                () = manager.hub.member.wake.notified() => {}
            }
        }
        backoff = FIRST_BACKOFF;
        reloaded.borrow_and_update();
    }
}

/// How keeping a link ended.
enum Kept {
    /// It was open, then ended.
    Served,
    /// It did not open.
    Failed,
    /// The hub refused it for good, or what the link reads changed.
    Stopped,
}

impl Manager {
    /// What the link does next, from the hub role, the membership and
    /// `[machine.hub] connect`.
    async fn link_attempt(&self) -> Result<Attempt> {
        let Some(settings) = self.hub_settings().filter(|hub| !hub.enabled) else {
            return Ok(Attempt::Idle(None));
        };
        let Some(joined) = self.store.read(membership::load).await? else {
            return Ok(Attempt::Idle(None));
        };
        if joined.revoked {
            return Ok(Attempt::Idle(Some(
                "the hub revoked this machine's credential; run shoal hub join to join it again"
                    .into(),
            )));
        }
        let route = route(settings.connect, &joined.membership);
        Ok(Attempt::Connect {
            joined: Box::new(joined),
            route,
        })
    }

    /// Open the link `attempt` names and serve it until it ends, the hub
    /// refuses it, or what the link reads changes.
    async fn keep_link(&self, attempt: Attempt) -> Kept {
        let stop = self.link_changed(&attempt);
        let plan = match self.link_plan(&attempt) {
            Ok(Some(plan)) => plan,
            Ok(None) => return Kept::Stopped,
            Err(error) => return self.link_failed(&error),
        };
        let route = plan.route.clone();
        let transport = match self.open_attempt(plan).await {
            Ok(Ok(transport)) => transport,
            Ok(Err((refusal, sealed))) => return self.refused(&route, refusal, sealed).await,
            Err(error) => return self.link_failed(&error),
        };
        self.serve_hub(transport, &route, stop).await
    }

    /// How to open the link `attempt` names, none for an idle one.
    fn link_plan(&self, attempt: &Attempt) -> Result<Option<Plan>> {
        let Attempt::Connect { joined, route } = attempt else {
            return Ok(None);
        };
        Ok(Some(Plan {
            route: route.clone(),
            intent: Intent::Connect {
                member: joined.membership.member.clone(),
            },
            key: Identity::new(&joined.key)?,
            code: None,
            expected: Expected::Key(joined.hub_key.clone()),
        }))
    }

    /// Open the link `plan` names, or the hub's refusal and whether it
    /// came sealed.
    async fn open_attempt(&self, plan: Plan) -> Result<Result<Transport, (Refusal, bool)>> {
        let route = plan.route.clone();
        let (transport, opened) = self.open_link(plan).await?;
        let welcomed = match opened {
            Opened::Welcomed(welcomed) => welcomed,
            Opened::Refused { refusal, sealed } => return Ok(Err((refusal, sealed))),
        };
        log!("hub: connected to {route} as {}", welcomed.member);
        Ok(Ok(transport))
    }

    /// Serve the open link on `transport` until it ends or `stop`
    /// resolves, offering its calls meanwhile.
    async fn serve_hub(
        &self,
        mut transport: Transport,
        route: &Route,
        stop: impl Future<Output = ()>,
    ) -> Kept {
        let channel = transport.take_channel();
        let calls = Calls::new(channel.sender());
        self.hub.member.set(Some(calls.clone()), None);
        let answer = |_| async {
            let message = "this machine answers no calls of the hub".into();
            (Reply::Failed { message }, ())
        };
        let problem = match session::serve(channel, &calls, answer, stop).await {
            End::Stopped => return Kept::Stopped,
            End::Closed => format!("the hub at {route} closed the link"),
            End::Lost(error) => format!("lost the link to the hub at {route}: {error:#}"),
        };
        log!("hub: {problem}");
        self.hub.member.set(None, Some(problem));
        Kept::Served
    }

    /// Resolves once what `attempt` read changed: the membership, by a join
    /// or leave, or `[machine.hub]` by a reload.
    fn link_changed(&self, attempt: &Attempt) -> impl Future<Output = ()> + '_ {
        let mut reloaded = self.config_state.watch();
        let attempt = attempt.clone();
        async move {
            loop {
                tokio::select! {
                    () = self.hub.member.wake.notified() => return,
                    changed = reloaded.changed() => {
                        if changed.is_err() {
                            return std::future::pending().await;
                        }
                        let now = self.link_attempt().await;
                        if now.ok().as_ref() != Some(&attempt) {
                            return;
                        }
                    }
                }
            }
        }
    }

    fn link_failed(&self, error: &anyhow::Error) -> Kept {
        log!("hub: {error:#}");
        self.hub.member.set(None, Some(format!("{error:#}")));
        Kept::Failed
    }

    /// Back off after the hub's `refusal`, telling the user. A revocation
    /// that came `sealed`, which only the hub can send, is recorded, so the
    /// link waits for a new join; any other refusal is tried again.
    async fn refused(&self, route: &Route, refusal: Refusal, sealed: bool) -> Kept {
        let message = format!(
            "The hub at {route} refused this machine: {}",
            refusal.message
        );
        log!("hub: {message}");
        // Recorded before the problem shows, so a join that follows it finds
        // the membership revoked.
        if sealed
            && refusal.reason == RefusalReason::Revoked
            && let Err(error) = self.store.write(membership::revoked).await
        {
            log!("hub: could not record the revoked membership: {error:#}");
        }
        self.hub.member.set(None, Some(refusal.message));
        self.notify(None, NotificationKind::HubRefused, &message)
            .await;
        Kept::Failed
    }

    /// The hub's reply to `call`, over this machine's link.
    pub(crate) async fn through_hub(&self, call: Call) -> Result<Reply> {
        if let Some(calls) = self.hub.member.calls() {
            return calls.call(call).await;
        }
        let joined = self.store.read(membership::load).await?;
        let problem = self.hub.member.state.lock().unwrap().problem.clone();
        match (joined, problem) {
            (None, _) => Err(Refused::new(
                Reason::NotFound,
                "this machine is not the hub and joined none; run shoal hub join, or set [machine.hub] enabled on the hub",
            )
            .into()),
            (Some(_), Some(problem)) => Err(anyhow!("this machine is not connected to the hub: {problem}")),
            (Some(_), None) => Err(anyhow!("this machine is not connected to the hub yet")),
        }
    }

    /// This machine's membership, for `shoal status`.
    pub(super) async fn membership_role(&self) -> Result<Option<HubRole>> {
        let Some(joined) = self.store.read(membership::load).await? else {
            return Ok(None);
        };
        let connect = self.config().machine.hub.connect.clone();
        let route = route(connect, &joined.membership);
        let state = self.hub.member.state.lock().unwrap();
        Ok(Some(HubRole::Member {
            membership: joined.membership,
            route,
            connected: state.calls.is_some(),
            problem: state.problem.clone(),
        }))
    }
}

/// The route to the hub: `connect` when it names a command, else what
/// `membership` joined through.
fn route(connect: Vec<String>, membership: &Membership) -> Route {
    match connect.is_empty() {
        true => membership.through.clone(),
        false => Route::Command(connect),
    }
}
