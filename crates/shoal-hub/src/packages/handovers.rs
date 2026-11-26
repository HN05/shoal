//! The hub's side of a machine's hand-over as it shuts down.
use std::{sync::Arc, time::Duration};

use anyhow::{Result, anyhow};
use shoal_daemon_base::alerts::About;
use shoal_ids::{member_ids::MemberId, space_ids::SpaceId};
use shoal_protocol::{
    alerts::AlertKind,
    hub::{
        link::{Call, Reply},
        packages::Departure,
        placement::Decision,
    },
};
use shoal_schedule::placement::snapshot::Request;

use super::{Continuation, Sent};
use crate::{
    Hub,
    answers::unexpected,
    daemon::Daemon,
    log,
    streams::{Outgoing, Streams},
};
pub(crate) use state::Handovers;
pub(super) use state::Released;

/// How long the hub waits for a machine to continue a space handed over,
/// which may clone its repository and set its worktree up first.
const CONTINUE_WITHIN: Duration = Duration::from_secs(30 * 60);

impl Hub {
    /// Take the final package `sent` that a machine shutting down writes up
    /// the duplex stream `stream` of `streams`, keeping it as
    /// [`Self::keep_package`] does, then place its space for its start
    /// among the other connected machines, or for a chat in a workspace's
    /// worktree where it placed that workspace, write that placement on
    /// this side of the stream and end it, for the machine to leave the
    /// space there; refused at once as `keep_package` refuses a package.
    pub(crate) async fn take_handover(
        self: &Arc<Self>,
        daemon: &Arc<dyn Daemon>,
        sent: Sent,
        streams: (&Streams, u64),
    ) -> Result<()> {
        let (out, up) = self.accept_package(&**daemon, &sent, streams).await?;
        self.handovers.leaves(&sent.member);
        let (hub, daemon) = (self.clone(), daemon.clone());
        tokio::spawn(async move {
            let (space, from) = (sent.space.clone(), sent.member.clone());
            let Continuation { start, within } = sent.continuation.clone();
            let (kept, operation) = hub.take_package(sent, up).await;
            let placed = match (kept, &within, start) {
                (Ok(()), Some(workspace), _) => {
                    (hub.handovers.placed_workspace(workspace, &from)).map(Some)
                }
                (Ok(()), None, Some(start)) => hub.place_elsewhere(&*daemon, &from, start).await,
                (Ok(()), None, None) => Ok(None),
                (Err(error), _, _) => Err(error),
            };
            // Placed before the machine reads it, which may release it at once.
            let decision = placed.as_ref().ok().cloned().flatten();
            if let Some(decision) = decision.clone() {
                (hub.handovers).place(&space, (&from, within.as_ref()), decision);
            }
            if answer(out, placed).await.is_none() && decision.is_some() {
                hub.handovers.unplace(&space);
            }
            drop(operation);
        });
        Ok(())
    }

    /// Where a space that leaves the machine `from` goes for `start`, among
    /// the connected machines but `from`, the hub's own as the caller's.
    pub(super) async fn place_elsewhere(
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
    /// as the space then stays on `from`, and having `from` retire its
    /// copy when it did while `from` stays.
    async fn continue_handover(
        self: &Arc<Self>,
        daemon: &dyn Daemon,
        id: &SpaceId,
        from: &MemberId,
        decision: &Decision,
    ) {
        self.handovers.start(id, &decision.machine);
        let departure = Departure::HandedOver;
        let continued = self.ask_to_continue(daemon, id, (from, &decision.machine), departure);
        let continued = continued.await;
        self.handovers.end(id);
        if continued.is_ok() {
            self.retire_after_continuing(daemon, from).await;
        }
        if let Err(error) = continued {
            log!("hub: space {id} handed over did not continue: {error:#}");
            let failed = (decision, departure);
            let message = self.failed_message(daemon, id, from, failed, &error).await;
            let about = About::named(None);
            (self.notify(daemon, about, AlertKind::HandoverFailed, message)).await;
        }
    }

    /// As the hub: take `member`'s word that it leaves the space `id` where
    /// the hub placed it, and have that machine continue it, in the
    /// background, after the workspace a chat follows; refused unless the
    /// hub placed it for `member` and took the release of that workspace.
    pub(crate) fn hear_release(
        self: &Arc<Self>,
        daemon: &Arc<dyn Daemon>,
        member: &MemberId,
        id: &SpaceId,
    ) -> Result<()> {
        let released = self.handovers.release(id, member)?;
        let (daemon, from, id) = (daemon.clone(), member.clone(), id.clone());
        tokio::spawn((self.clone()).continue_released(daemon, from, id, released));
        Ok(())
    }

    /// Have the machine `released` names continue the space `id`, which
    /// `from` left to the hub, after the workspace a chat follows.
    pub(super) async fn continue_released(
        self: Arc<Self>,
        daemon: Arc<dyn Daemon>,
        from: MemberId,
        id: SpaceId,
        released: Released,
    ) {
        if let Some(mut after) = released.after {
            let _ = after.wait_for(|ended| *ended).await;
        }
        (self.continue_handover(&*daemon, &id, &from, &released.decision)).await;
        self.handovers.continued(&id);
    }

    /// Have `from`, which a space it handed over left, retire the spaces
    /// taken over from it, unless it is still leaving: here for the hub's
    /// own machine, else over its link.
    async fn retire_after_continuing(self: &Arc<Self>, daemon: &dyn Daemon, from: &MemberId) {
        match self.own_member().await {
            Ok(own) if &own == from && !self.handovers.is_leaving(from) => {
                self.retire_own(daemon, from).await;
            }
            Ok(own) if &own == from => {}
            _ => self.retire_while_staying(from),
        }
    }

    /// As the hub: take `member`'s word that the shutdown it handed spaces
    /// over for was cancelled, so it stays and retires the spaces taken
    /// over from it now, and those claimed later as each is.
    pub(crate) fn hear_stay(self: &Arc<Self>, member: &MemberId) {
        self.handovers.stays(member);
        self.retire_while_staying(member);
    }

    /// Have `member` retire the spaces taken over from it now, unless a
    /// shutdown of it is under way or its link is down, when it retires
    /// them once its link opens.
    fn retire_while_staying(self: &Arc<Self>, member: &MemberId) {
        if self.handovers.is_leaving(member) {
            return;
        }
        if let Some(calls) = self.links.calls(member) {
            tokio::spawn((self.clone()).retire_taken_over(member.clone(), calls));
        }
    }

    /// Ask the machine `to` to continue the space `id`, which leaves the
    /// machine `from` for `departure`, answering the call here for the
    /// hub's own.
    pub(super) async fn ask_to_continue(
        &self,
        daemon: &dyn Daemon,
        id: &SpaceId,
        (from, to): (&MemberId, &MemberId),
        departure: Departure,
    ) -> Result<()> {
        let from = self.hub_machine(daemon, from).await?.name;
        let call = Call::Continue {
            space: id.clone(),
            from,
            departure,
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

    /// What the user reads of the space `id`, which left the machine
    /// `from` for `departure` and did not continue on the machine
    /// `decision` names, failing with `error`.
    pub(super) async fn failed_message(
        &self,
        daemon: &dyn Daemon,
        id: &SpaceId,
        from: &MemberId,
        (decision, departure): (&Decision, Departure),
        error: &anyhow::Error,
    ) -> String {
        let from = self.member_name(daemon, from).await;
        let (left, back) = match departure {
            Departure::HandedOver => (
                format!("which machine {from} handed over"),
                format!("where shoal resume {id} continues it once that machine is back"),
            ),
            Departure::Slept => (
                format!("which the hub took over from machine {from} as it slept"),
                "where it goes on once that machine wakes".to_owned(),
            ),
        };
        format!(
            "Space {id}, {left}, did not continue on machine {}: {error:#}; it stays on {from}, {back}, while shoal restore {id} takes it over from the hub's package meanwhile",
            decision.name
        )
    }

    /// The name of the machine `member`, its ID where the hub lists none.
    pub(crate) async fn member_name(&self, daemon: &dyn Daemon, member: &MemberId) -> String {
        match self.hub_machine(daemon, member).await {
            Ok(machine) => machine.name,
            Err(_) => member.to_string(),
        }
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

mod state;
#[cfg(test)]
mod tests;
