//! A move to files of spaces other joined machines run, or of copies of
//! them. Each such machine packs its spaces and holds their packages,
//! which this one reads over the hub and writes where the caller named; for
//! a file or a directory it then ends each space there, and the command
//! that copies a package to its standard output ends it once copied, as
//! for a space of this machine. A copy's package goes there once read, and
//! the space keeps running. With spaces of several machines, this one's
//! included, the move asks once what ending all of them needs before any
//! ends.
mod consent;
mod delivery;

use std::collections::BTreeMap;

use anyhow::{Result, anyhow, bail};

use super::Steps;
use crate::{
    client::{
        connection::CommandPaths,
        operations::{Followed, Following, MoveStarted},
    },
    protocol::operations::{Ended, Step},
};
use delivery::Reserved;
use shoal_core::transfer::{Consent, MoveDestination, MoveOrder, MoveReport};
use shoal_ids::{member_ids::MemberId, space_ids::SpaceId};
use shoal_protocol::model::Located;
use shoal_protocol::space_names::{SpaceName, SpaceSelector};

/// The spaces of a move to files that other joined machines run, by the
/// machine that runs them.
#[derive(Default)]
pub(super) struct Relays(Vec<Relay>);

/// The spaces of a move that one other joined machine runs.
struct Relay {
    member: MemberId,
    /// The machine's name, as the move tells of it.
    machine: String,
    spaces: Vec<(SpaceId, SpaceName)>,
}

impl Relays {
    /// The spaces `order` moves to files that this machine neither runs
    /// nor removed, by the other joined machine that runs each; none for a
    /// move to another machine.
    pub(super) async fn find(paths: &CommandPaths, order: &MoveOrder) -> Result<Self> {
        let (MoveDestination::File { .. }
        | MoveDestination::Directory { .. }
        | MoveDestination::Client { .. }) = order.destination
        else {
            return Ok(Self::default());
        };
        let present = crate::client::spaces::list(paths).await?;
        let mut relays = Self::default();
        for id in &order.spaces {
            if present.iter().any(|space| space.id == *id) || removed_here(paths, id).await {
                continue;
            }
            let located = crate::client::spaces::locate(paths, SpaceSelector::id(id)).await?;
            if let Located::Elsewhere { space, machine } = located {
                let space = (space.space.id, space.space.name);
                relays.add(machine.id, machine.name, space);
            }
        }
        Ok(relays)
    }

    /// Whether no other machine runs a space of the move.
    pub(super) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Add `space`, which the machine `member`, named `machine`, runs.
    fn add(&mut self, member: MemberId, machine: String, space: (SpaceId, SpaceName)) {
        match self.0.iter_mut().find(|relay| relay.member == member) {
            Some(relay) => relay.spaces.push(space),
            None => self.0.push(Relay {
                member,
                machine,
                spaces: vec![space],
            }),
        }
    }

    /// `order` for the spaces of it that this machine has.
    fn left_here(&self, order: &MoveOrder) -> MoveOrder {
        let relayed = |id: &SpaceId| (self.0.iter()).any(|relay| relay.ids().contains(id));
        MoveOrder {
            spaces: (order.spaces.iter())
                .filter(|id| !relayed(id))
                .cloned()
                .collect(),
            ..order.clone()
        }
    }
}

/// Whether the space `id` was removed here, which the move takes as a
/// space of this machine.
async fn removed_here(paths: &CommandPaths, id: &SpaceId) -> bool {
    let removed = crate::client::removal::removed_space(paths, SpaceSelector::id(id));
    removed.await.is_ok()
}

/// Run `order`, some of whose spaces `relays` other machines run: alone,
/// those of one machine as that machine asks; together with others, once
/// the caller consents to what ending all of them needs, this machine's
/// first, then each other machine's, going on after one fails and
/// printing a line for each.
pub(super) async fn run(
    paths: &CommandPaths,
    order: &MoveOrder,
    relays: Relays,
    steps: &impl Steps,
) -> Result<MoveReport> {
    let here = relays.left_here(order);
    let mut report = MoveReport::default();
    if let ([relay], true) = (relays.0.as_slice(), here.spaces.is_empty()) {
        relay
            .relay(paths, order, order.consent.clone(), steps, &mut report)
            .await?;
        super::sum_up(order, &report, relay.spaces.len() > 1);
        return Ok(report);
    }
    let planned = match here.spaces.is_empty() {
        true => Vec::new(),
        false => super::plan(paths, &here).await?,
    };
    let asked_here = super::confirmation::question(&here, &planned);
    let consents = consent::ask(paths, order, asked_here, &relays).await?;
    super::move_each(paths, &here, planned, steps, &mut report).await;
    for (relay, consent) in relays.0.iter().zip(consents) {
        let relayed = relay.relay(paths, order, consent, steps, &mut report);
        if let Err(error) = relayed.await {
            relay.failed(order, &mut report, &error);
        }
    }
    super::sum_up(order, &report, true);
    Ok(report)
}

impl Relay {
    /// The IDs of its spaces.
    fn ids(&self) -> Vec<SpaceId> {
        self.spaces.iter().map(|(id, _)| id.clone()).collect()
    }

    /// Move its spaces, or copies of them, as `order` says, with
    /// `consent`: its machine packs them and holds their packages, which
    /// this one writes to their new files, refused before anything ends
    /// there when one cannot be created, ending each space there once its
    /// file holds it, as `report` records.
    async fn relay(
        &self,
        paths: &CommandPaths,
        order: &MoveOrder,
        consent: Option<Consent>,
        steps: &impl Steps,
        report: &mut MoveReport,
    ) -> Result<()> {
        steps.ensure_running()?;
        let reserved = Reserved::all(&order.destination, &self.ids())?;
        let held = MoveOrder {
            spaces: self.ids(),
            destination: MoveDestination::Held,
            consent,
            ..order.clone()
        };
        match pack_there(paths, held).await {
            Ok(packed) => deliver_packed(paths, order, packed, reserved, steps, report).await,
            Err(error) => {
                reserved.into_iter().for_each(Reserved::remove);
                return Err(error);
            }
        }
        Ok(())
    }

    /// Record in `report` that none of its spaces moved as `order` says,
    /// for `error`.
    fn failed(&self, order: &MoveOrder, report: &mut MoveReport, error: &anyhow::Error) {
        for (id, name) in &self.spaces {
            let failed = Err(anyhow!("{error:#}"));
            super::record(order, report, (id.clone(), name.clone()), failed);
        }
    }
}

/// Record in `report` the spaces `packed` failed to pack, as `order` says,
/// then deliver each it packed into its file among `reserved`, deleting
/// those of the spaces that went in another's package.
async fn deliver_packed(
    paths: &CommandPaths,
    order: &MoveOrder,
    packed: MoveReport,
    reserved: Vec<Reserved>,
    steps: &impl Steps,
    report: &mut MoveReport,
) {
    for failed in packed.failed {
        let error = Err(anyhow!(failed.error));
        super::record(order, report, (failed.id, failed.name), error);
    }
    if !packed.moved.is_empty() {
        steps.reached(Step::MoveDelivering).await;
    }
    let mut moved = packed.moved;
    for file in reserved {
        let Some(index) = moved.iter().position(|moved| moved.id == file.id) else {
            file.remove();
            continue;
        };
        let packed = moved.swap_remove(index);
        let identity = (packed.id.clone(), packed.name.clone());
        let delivered = delivery::deliver(paths, order, packed, file).await;
        super::record(order, report, identity, delivered);
    }
}

/// Start the move `held` on the machine of its spaces and follow it to its
/// end there, passing on the consent it asks for.
async fn pack_there(paths: &CommandPaths, held: MoveOrder) -> Result<MoveReport> {
    let mut following = match Following::start_move(paths, held, BTreeMap::new()).await? {
        MoveStarted::Following(_, following) => following,
        MoveStarted::AfterTurn(_) => bail!("a move to a file stops no agent's turn"),
    };
    loop {
        match following.next().await? {
            Followed::Event(_) => {}
            Followed::Ended(Ended::Succeeded(outcome)) => return MoveReport::try_from(*outcome),
            Followed::Ended(Ended::Failed(failure)) => {
                return Err(crate::operations::error(failure));
            }
        }
    }
}
