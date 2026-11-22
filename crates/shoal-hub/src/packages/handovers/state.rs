//! What the hub keeps in memory of the hand-overs under way: where it
//! placed each space, which machine may claim it, the order in which a
//! chat follows the workspace whose worktree it runs in, and the machines
//! still shutting down.
use std::{
    collections::{HashMap, HashSet},
    sync::Mutex,
};

use anyhow::{Result, anyhow, bail};
use shoal_ids::{member_ids::MemberId, space_ids::SpaceId};
use shoal_protocol::hub::placement::Decision;
use tokio::sync::watch;

/// Where the hub placed a space handed over.
struct Placed {
    /// The machine that handed it over.
    from: MemberId,
    decision: Decision,
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
}

/// A space its machine released, which goes where `decision` says once
/// `after` holds, when it follows a workspace.
pub(super) struct Released {
    pub decision: Decision,
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
    /// not release; those it released go on, and the chats in their
    /// worktrees still follow them.
    pub(crate) fn stays(&self, member: &MemberId) {
        self.leaving.lock().unwrap().remove(member);
        (self.placed.lock().unwrap()).retain(|_, placed| &placed.from != member || placed.released);
    }

    /// Record that `member` hands spaces over for its shutdown.
    pub(super) fn leaves(&self, member: &MemberId) {
        self.leaving.lock().unwrap().insert(member.clone());
    }

    pub(super) fn is_leaving(&self, member: &MemberId) -> bool {
        self.leaving.lock().unwrap().contains(member)
    }

    /// Record that the hub sends the space `id`, which `from` handed over
    /// following the workspace `within`, where `decision` says.
    pub(super) fn place(
        &self,
        id: &SpaceId,
        (from, within): (&MemberId, Option<&SpaceId>),
        decision: Decision,
    ) {
        let placed = Placed {
            from: from.clone(),
            decision,
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
    pub(super) fn placed_workspace(&self, id: &SpaceId, from: &MemberId) -> Result<Decision> {
        let placed = self.placed.lock().unwrap();
        match placed.get(id) {
            Some(placed) if &placed.from == from => Ok(placed.decision.clone()),
            _ => bail!("the hub placed no workspace {id} of this machine for the chat to follow"),
        }
    }

    /// Take `from`'s release of the space `id`, once: where it goes, and
    /// the continuing of the workspace it follows, which its machine must
    /// have released first.
    pub(super) fn release(&self, id: &SpaceId, from: &MemberId) -> Result<Released> {
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
            decision: space.decision.clone(),
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
    pub(super) fn start(&self, id: &SpaceId, to: &MemberId) {
        self.claims.lock().unwrap().insert(id.clone(), to.clone());
    }

    pub(super) fn end(&self, id: &SpaceId) {
        self.claims.lock().unwrap().remove(id);
    }
}
