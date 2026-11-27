//! What the hub keeps in memory of the hand-overs under way.
use std::{
    collections::{HashMap, HashSet},
    sync::Mutex,
};

use anyhow::{Result, anyhow, bail};
use shoal_ids::{member_ids::MemberId, space_ids::SpaceId};
use shoal_protocol::hub::{packages::Destination, placement::Decision};
use shoal_schedule::placement::snapshot::Request;
use tokio::sync::watch;

/// Where the hub placed a space handed over.
struct Placed {
    /// The machine that handed it over.
    from: MemberId,
    destination: Destination,
    /// The start that places it, once a machine can take a space the hub
    /// keeps.
    start: Option<Request>,
    /// The workspace whose worktree a chat runs in, which it follows.
    within: Option<SpaceId>,
    /// Whether its machine released it to the hub.
    released: bool,
}

/// What the hub keeps of the hand-overs under way.
#[derive(Default)]
pub(crate) struct Handovers {
    /// The spaces handed over, with where the hub sends each.
    placed: Mutex<HashMap<SpaceId, Placed>>,
    /// The spaces released, each with whether its continuing ended, which
    /// a chat in its worktree waits for, until the hub's daemon stops.
    continuing: Mutex<HashMap<SpaceId, watch::Sender<bool>>>,
    /// The spaces handed over whose new machine has not claimed them yet,
    /// each with that machine.
    claims: Mutex<HashMap<SpaceId, MemberId>>,
    /// The machines that handed spaces over for a shutdown that has not
    /// ended their link, nor been cancelled.
    leaving: Mutex<HashSet<MemberId>>,
    /// Counts the machines that stayed after all, which keep the spaces the
    /// hub kept for them.
    stayed: watch::Sender<u64>,
    /// The spaces the hub kept that another machine took over meanwhile.
    taken_over: Mutex<HashSet<SpaceId>>,
}

/// A space its machine released, which goes where `destination` says once
/// `after` holds, when it follows the workspace `within`. A space the hub
/// keeps goes where `start` is placed.
pub(crate) struct Released {
    pub destination: Destination,
    pub start: Option<Request>,
    pub within: Option<SpaceId>,
    pub after: Option<watch::Receiver<bool>>,
}

impl Handovers {
    /// Whether the space `id` was handed over to `taker`, which may then
    /// take it over while the machine it comes from is still connected.
    pub(crate) fn handed_to(&self, id: &SpaceId, taker: &MemberId) -> bool {
        self.claims.lock().unwrap().get(id) == Some(taker)
    }

    /// Record that `member`, whose link opened again or whose shutdown was
    /// cancelled, is no longer shutting down, and keeps the spaces it did
    /// not release and those the hub kept for want of a machine; those it
    /// released elsewhere go on, and the chats in their worktrees still
    /// follow them.
    pub(crate) fn stays(&self, member: &MemberId) {
        self.leaving.lock().unwrap().remove(member);
        (self.placed.lock().unwrap()).retain(|_, placed| {
            &placed.from != member || (placed.released && placed.destination != Destination::Hub)
        });
        self.stayed.send_modify(|stayed| *stayed += 1);
    }

    /// Changes as each machine stays after all.
    pub(crate) fn watch_stays(&self) -> watch::Receiver<u64> {
        self.stayed.subscribe()
    }

    /// Whether the hub still keeps the space `id`, which `from` released
    /// to it, for want of a machine.
    pub(in crate::packages) fn keeps(&self, id: &SpaceId, from: &MemberId) -> bool {
        let placed = self.placed.lock().unwrap();
        placed.get(id).is_some_and(|placed| kept_from(placed, from))
    }

    /// Take `claimer`'s claim of the space `id`, which the hub did not hand
    /// to it: refused while the hub hands the space to another machine. Once
    /// taken, the hub keeps the space no longer, even should the claim then
    /// fail. Under the lock [`Self::placed_later`] takes, so a claim and a
    /// later placement never both go ahead.
    pub(crate) fn take_over(&self, id: &SpaceId, claimer: &MemberId) -> Result<()> {
        let mut placed = self.placed.lock().unwrap();
        if let Some(to) = (self.claims.lock().unwrap().get(id)).filter(|to| *to != claimer) {
            bail!("the hub hands space {id} over to machine {to}, which claims it");
        }
        if placed
            .get(id)
            .is_some_and(|placed| placed.destination == Destination::Hub)
        {
            placed.remove(id);
            self.taken_over.lock().unwrap().insert(id.clone());
            self.stayed.send_modify(|stayed| *stayed += 1);
        }
        Ok(())
    }

    /// Whether another machine took over the space `id` while the hub kept
    /// it, forgetting that it did.
    pub(in crate::packages) fn was_taken_over(&self, id: &SpaceId) -> bool {
        self.taken_over.lock().unwrap().remove(id)
    }

    /// Record that the space `id`, which the hub keeps for `from`, goes where
    /// `decision` says, as the chats in its worktree do, and that the machine
    /// it names may claim it. False, recording nothing, once the hub no
    /// longer keeps it, as once `from` stayed or another machine took it.
    pub(in crate::packages) fn placed_later(
        &self,
        id: &SpaceId,
        from: &MemberId,
        decision: Decision,
    ) -> bool {
        let mut placed = self.placed.lock().unwrap();
        let Some(placed) = placed.get_mut(id).filter(|placed| kept_from(placed, from)) else {
            return false;
        };
        (self.claims.lock().unwrap()).insert(id.clone(), decision.machine.clone());
        placed.destination = Destination::Machine(decision);
        true
    }

    /// Record that `member` hands spaces over for its shutdown.
    pub(in crate::packages) fn leaves(&self, member: &MemberId) {
        self.leaving.lock().unwrap().insert(member.clone());
    }

    pub(super) fn is_leaving(&self, member: &MemberId) -> bool {
        self.leaving.lock().unwrap().contains(member)
    }

    /// Record that the hub sends the space `id`, which `from` handed over
    /// for `start` following the workspace `within`, where `destination`
    /// says.
    pub(in crate::packages) fn place(
        &self,
        id: &SpaceId,
        (from, within, start): (&MemberId, Option<&SpaceId>, Option<Request>),
        destination: Destination,
    ) {
        let placed = Placed {
            from: from.clone(),
            destination,
            start,
            within: within.cloned(),
            released: false,
        };
        self.placed.lock().unwrap().insert(id.clone(), placed);
    }

    /// Forget where the hub sends the space `id`, whose machine never
    /// read it.
    pub(super) fn unplace(&self, id: &SpaceId) {
        self.placed.lock().unwrap().remove(id);
    }

    /// Where the hub sends the workspace `id` that `from` handed over,
    /// which a chat in its worktree follows.
    pub(in crate::packages) fn placed_workspace(
        &self,
        id: &SpaceId,
        from: &MemberId,
    ) -> Result<Destination> {
        let placed = self.placed.lock().unwrap();
        match placed.get(id) {
            Some(placed) if &placed.from == from => Ok(placed.destination.clone()),
            _ => bail!("the hub placed no workspace {id} of this machine for the chat to follow"),
        }
    }

    /// Take `from`'s release of the space `id`, once: where it goes, and
    /// the continuing of the workspace it follows, which its machine must
    /// have released first.
    pub(in crate::packages) fn release(&self, id: &SpaceId, from: &MemberId) -> Result<Released> {
        let mut placed = self.placed.lock().unwrap();
        let space = (placed.get_mut(id))
            .filter(|placed| &placed.from == from && !placed.released)
            .ok_or_else(|| anyhow!("no hand-over of space {id} waits for its machine's word"))?;
        let mut continuing = self.continuing.lock().unwrap();
        let after = match &space.within {
            Some(workspace) => Some(
                (continuing.get(workspace))
                    .ok_or_else(|| anyhow!("its workspace {workspace} was not released"))?
                    .subscribe(),
            ),
            None => None,
        };
        space.released = true;
        continuing.insert(id.clone(), watch::channel(false).0);
        Ok(Released {
            destination: space.destination.clone(),
            start: space.start.clone(),
            within: space.within.clone(),
            after,
        })
    }

    /// Record that the continuing of the space `id` ended, however it went.
    pub(super) fn continued(&self, id: &SpaceId) {
        if let Some(continuing) = self.continuing.lock().unwrap().get(id) {
            continuing.send_replace(true);
        }
    }

    /// Record that `to` may claim the space `id`, while it continues it.
    pub(in crate::packages) fn start(&self, id: &SpaceId, to: &MemberId) {
        self.claims.lock().unwrap().insert(id.clone(), to.clone());
    }

    pub(super) fn end(&self, id: &SpaceId) {
        self.claims.lock().unwrap().remove(id);
    }
}

/// Whether the hub keeps `placed`, which `from` released to it, for want of
/// a machine.
fn kept_from(placed: &Placed, from: &MemberId) -> bool {
    &placed.from == from && placed.released && placed.destination == Destination::Hub
}
