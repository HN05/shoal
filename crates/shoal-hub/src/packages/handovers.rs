//! The hub's side of a machine's hand-over as it shuts down: the machine
//! sends the final package of each of its spaces with the start its agent
//! would make. The hub keeps it as it keeps any package, places the space
//! among the other connected machines, its own as the caller's, answers
//! with where it goes, and asks that machine to continue the space from
//! the package. That machine's claim takes the space over even while the
//! machine it came from is still connected, and the machine it came from
//! retires its copy once its link opens again, as after any takeover.
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};

use anyhow::{Result, anyhow};
use shoal_daemon_base::alerts::About;
use shoal_ids::{member_ids::MemberId, space_ids::SpaceId};
use shoal_protocol::{
    alerts::AlertKind,
    hub::{
        link::{Call, Reply},
        placement::Decision,
    },
};
use shoal_schedule::placement::snapshot::Request;

use super::Sent;
use crate::{
    Hub,
    answers::unexpected,
    daemon::Daemon,
    log,
    streams::{Outgoing, Streams},
};

/// How long the hub waits for a machine to continue a space handed over,
/// which may clone its repository and set its worktree up first.
const CONTINUE_WITHIN: Duration = Duration::from_secs(30 * 60);

/// The spaces handed over whose new machine has not claimed them yet,
/// each with that machine.
#[derive(Default)]
pub(crate) struct Handovers(Mutex<HashMap<SpaceId, MemberId>>);

impl Handovers {
    /// Whether the space `id` was handed over to `taker`, which may then
    /// take it over while the machine it comes from is still connected.
    pub(crate) fn handed_to(&self, id: &SpaceId, taker: &MemberId) -> bool {
        self.0.lock().unwrap().get(id) == Some(taker)
    }

    fn start(&self, id: &SpaceId, to: &MemberId) {
        self.0.lock().unwrap().insert(id.clone(), to.clone());
    }

    fn end(&self, id: &SpaceId) {
        self.0.lock().unwrap().remove(id);
    }
}

impl Hub {
    /// Take the final package `sent` that a machine shutting down writes up
    /// the duplex stream `stream` of `streams`, keeping it as
    /// [`Self::keep_package`] does, then place its space for `start` among
    /// the other connected machines, write that placement on this side of
    /// the stream and end it, and have the machine it chose continue the
    /// space; refused at once as `keep_package` refuses a package.
    pub(crate) async fn take_handover(
        self: &Arc<Self>,
        daemon: &Arc<dyn Daemon>,
        sent: Sent,
        start: Request,
        streams: (&Streams, u64),
    ) -> Result<()> {
        let (out, up) = self.accept_package(&**daemon, &sent, streams).await?;
        let (hub, daemon) = (self.clone(), daemon.clone());
        tokio::spawn(async move {
            let (space, from) = (sent.space.clone(), sent.member.clone());
            let (kept, operation) = hub.take_package(sent, up).await;
            let placed = match kept {
                Ok(()) => hub.place_handover(&*daemon, &from, start).await,
                Err(error) => Err(error),
            };
            let answered = answer(out, placed).await;
            drop(operation);
            // Only a machine that read where its space goes leaves it.
            if let Some(decision) = answered {
                hub.continue_handover(&*daemon, &space, &from, &decision)
                    .await;
            }
        });
        Ok(())
    }

    /// Where a space that the machine `from` hands over goes for `start`,
    /// among the connected machines but `from`, the hub's own as the
    /// caller's.
    async fn place_handover(
        &self,
        daemon: &dyn Daemon,
        from: &MemberId,
        mut start: Request,
    ) -> Result<Option<Decision>> {
        start.caller = self.own_member().await?;
        (self.place_among(daemon, start, |member| &member.id != from)).await
    }

    /// Have the machine `decision` names continue the space `id`, which
    /// the machine `from` handed over, alerting the user when it does not,
    /// as the space then stays on `from`.
    async fn continue_handover(
        &self,
        daemon: &dyn Daemon,
        id: &SpaceId,
        from: &MemberId,
        decision: &Decision,
    ) {
        self.handovers.start(id, &decision.machine);
        let continued = self.ask_to_continue(daemon, id, from, &decision.machine);
        let continued = continued.await;
        self.handovers.end(id);
        if let Err(error) = continued {
            log!("hub: space {id} handed over did not continue: {error:#}");
            let message = self
                .failed_message(daemon, id, from, decision, &error)
                .await;
            let about = About::named(None);
            (self.notify(daemon, about, AlertKind::HandoverFailed, message)).await;
        }
    }

    /// Ask the machine `to` to continue the space `id`, which the machine
    /// `from` handed over, answering the call here for the hub's own.
    async fn ask_to_continue(
        &self,
        daemon: &dyn Daemon,
        id: &SpaceId,
        from: &MemberId,
        to: &MemberId,
    ) -> Result<()> {
        let from = self.hub_machine(daemon, from).await?.name;
        let call = Call::Continue {
            space: id.clone(),
            from,
        };
        let reply = if to == &self.own_member().await? {
            self.answer_hub_call(daemon, call).await?
        } else {
            let calls =
                (self.links.calls(to)).ok_or_else(|| anyhow!("it is no longer connected"))?;
            calls.call_within(call, CONTINUE_WITHIN).await?
        };
        match reply {
            Reply::Delivered => Ok(()),
            reply => Err(unexpected(reply)),
        }
    }

    /// What the user reads of the space `id`, which the machine `from`
    /// handed over and which did not continue on the machine `decision`
    /// names, failing with `error`.
    async fn failed_message(
        &self,
        daemon: &dyn Daemon,
        id: &SpaceId,
        from: &MemberId,
        decision: &Decision,
        error: &anyhow::Error,
    ) -> String {
        let from = match self.hub_machine(daemon, from).await {
            Ok(machine) => machine.name,
            Err(_) => from.to_string(),
        };
        format!(
            "Space {id}, which machine {from} handed over as it shut down, did not continue on machine {}: {error:#}; it stays on {from}, where shoal resume {id} continues it once that machine is back, while shoal restore {id} takes it over from the hub's package meanwhile",
            decision.name
        )
    }
}

/// Write on `out` where a space handed over goes, as `placed` says, and
/// end it, with why not when `placed` failed; the placement, once written.
async fn answer(mut out: Outgoing, placed: Result<Option<Decision>>) -> Option<Decision> {
    let decision = match placed {
        Ok(decision) => decision,
        Err(error) => {
            out.end(Some(format!("{error:#}"))).await;
            return None;
        }
    };
    let written = async {
        let json = serde_json::to_vec(&decision)?;
        out.write(&json).await
    };
    match written.await {
        Ok(()) => {
            out.end(None).await;
            decision
        }
        Err(error) => {
            log!("hub: could not answer a hand-over: {error:#}");
            out.end(Some(format!("{error:#}"))).await;
            None
        }
    }
}

#[cfg(test)]
mod tests;
