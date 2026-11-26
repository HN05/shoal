//! The `balanced` placement policy.
use std::cmp::Reverse;

use super::{
    Placed, Placement, Placer, Reason, Skipped,
    snapshot::{Capacity, Machine, Request, Snapshot},
};

/// The `balanced` policy. It ranks only the `always_on` machines with room
/// and the caller's machine, so a start no machine has room for stays on
/// the caller's.
#[derive(Debug, Clone, Copy, Default)]
pub struct Balanced;

impl Placer for Balanced {
    fn place(&self, snapshot: &Snapshot) -> Placement {
        let request = &snapshot.request;
        let mut placement = Placement::default();
        let mut ranked = Vec::new();
        for machine in &snapshot.machines {
            match request.unfit(machine) {
                Some(why) => placement.skipped.push(Skipped {
                    machine: machine.id.clone(),
                    why,
                }),
                None => ranked.extend(rank(request, machine)),
            }
        }
        ranked.sort_by_key(|(key, _)| *key);
        placement.ranked = ranked.into_iter().map(|(_, placed)| placed).collect();
        placement
    }
}

/// Where `machine` ranks, lowest first, with why: the `always_on` machines
/// with room, the caller's first, then the caller's machine. None for any
/// other machine.
fn rank<'a>(request: &Request, machine: &'a Machine) -> Option<((u8, RoomKey, &'a str), Placed)> {
    let caller = machine.id == request.caller;
    let ((group, key), reason) = match room(request, machine) {
        Some((key, reason)) => ((u8::from(!caller), key), reason),
        None if caller => ((2, RoomKey::default()), Reason::Caller),
        None => return None,
    };
    let placed = Placed {
        machine: machine.id.clone(),
        reason,
    };
    Some(((group, key, &machine.name), placed))
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
/// capacity it costs there, since dynamic admission gives heavy agents the
/// same memory. None where admission holds light agents back.
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

#[cfg(test)]
mod tests;
