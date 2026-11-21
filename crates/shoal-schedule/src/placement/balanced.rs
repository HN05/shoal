//! The `balanced` placement policy: lasting work goes to `always_on`
//! machines with room, else stays on the caller's machine.
use std::cmp::Reverse;

use super::{
    Placed, Placement, Placer, Reason, Skipped, SkippedBy,
    snapshot::{Capacity, Machine, Request, Snapshot},
};

/// The `balanced` policy. A start may go to each connected machine that
/// has its repository, matched by origin, unless it needs none, as the
/// caller's does, and its agent tool. The `always_on` machines with room come first: for a heavy
/// start those whose admission lets a heavy agent in now, most spare memory
/// first; for a light one, whose memory is what dynamic admission gives
/// heavy agents, those that run no heavy agents, then those where no heavy
/// work waits, most spare memory first, then those where the least heavy
/// work waits. An `always_on` caller with room keeps the start, beside its
/// parent. The caller's machine comes next, then the other machines.
#[derive(Debug, Clone, Copy, Default)]
pub struct Balanced;

impl Placer for Balanced {
    fn place(&self, snapshot: &Snapshot) -> Placement {
        let request = &snapshot.request;
        let mut placement = Placement::default();
        let mut ranked = Vec::new();
        for machine in &snapshot.machines {
            match unfit(request, machine) {
                Some(why) => placement.skipped.push(Skipped {
                    machine: machine.id.clone(),
                    why,
                }),
                None => ranked.push(rank(request, machine)),
            }
        }
        ranked.sort_by_key(|(key, _)| *key);
        placement.ranked = ranked.into_iter().map(|(_, placed)| placed).collect();
        placement
    }
}

/// Where `machine`, which `request` may go to, ranks, lowest first: the
/// `always_on` machines with room, the caller's first, then the caller's
/// machine, then the rest, each group by its rank and name; with why.
fn rank<'a>(request: &Request, machine: &'a Machine) -> ((u8, RoomKey, &'a str), Placed) {
    let caller = machine.id == request.caller;
    let ((group, key), reason) = match room(request, machine) {
        Some((key, reason)) => ((u8::from(!caller), key), reason),
        None if caller => ((2, RoomKey::default()), Reason::Caller),
        None => ((3, RoomKey::default()), without_room(machine)),
    };
    let placed = Placed {
        machine: machine.id.clone(),
        reason,
    };
    ((group, key, &machine.name), placed)
}

/// Why `request` cannot go to `machine`, none when it can. The caller's
/// machine has the start's repository, where the start found it.
fn unfit(request: &Request, machine: &Machine) -> Option<SkippedBy> {
    let caller = machine.id == request.caller;
    if !caller && !machine.has_repository(request.origin.as_deref()) {
        Some(SkippedBy::NoRepository)
    } else if !machine.has_agent(&request.agent) {
        Some(SkippedBy::NoAgent)
    } else {
        None
    }
}

/// How an `always_on` machine with room for `request` ranks among the
/// others, lowest first, with why; none for any other machine.
fn room(request: &Request, machine: &Machine) -> Option<(RoomKey, Reason)> {
    let capacity = machine.capacity.as_ref().filter(|_| machine.always_on)?;
    match request.heavy {
        true => heavy_room(capacity),
        false => light_room(capacity),
    }
}

/// The rank of a machine among those with room: its tier, then most
/// spare memory, then least heavy work waiting.
type RoomKey = (u8, Reverse<Option<u64>>, u32);

/// How a heavy start ranks on a machine of `capacity`, none without a
/// heavy slot free.
fn heavy_room(capacity: &Capacity) -> Option<(RoomKey, Reason)> {
    let memory_free = capacity.memory_free;
    (capacity.heavy_free > 0).then_some((
        (0, Reverse(memory_free), 0),
        Reason::HeavySlot { memory_free },
    ))
}

/// How a light start ranks on a machine of `capacity`, by how little heavy
/// capacity it costs there; none where its admission holds light agents
/// back.
fn light_room(capacity: &Capacity) -> Option<(RoomKey, Reason)> {
    if !capacity.light_free {
        return None;
    }
    let memory_free = capacity.memory_free;
    Some(match (capacity.max_heavy_agents, capacity.heavy_waiting) {
        (Some(0), _) => ((0, Reverse(None), 0), Reason::NoHeavyAgents),
        (_, 0) => (
            (1, Reverse(memory_free), 0),
            Reason::NoHeavyWaiting { memory_free },
        ),
        (_, waiting) => (
            (2, Reverse(None), waiting),
            Reason::HeavyWaiting { waiting },
        ),
    })
}

/// Why `machine`, which is not the caller's and has no room, comes where
/// it does.
fn without_room(machine: &Machine) -> Reason {
    match (&machine.capacity, machine.always_on) {
        (None, _) => Reason::Unreported,
        (Some(_), true) => Reason::Full,
        (Some(_), false) => Reason::Intermittent,
    }
}

#[cfg(test)]
mod tests;
