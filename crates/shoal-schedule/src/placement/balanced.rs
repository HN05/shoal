//! The `balanced` placement policy. It ranks machines by stages, each a
//! function that yields a key and, where it prefers the machine, a reason;
//! `docs/design/hub.md#policies` says why they come in this order.
use std::cmp::Reverse;

use super::{
    Placed, Placement, Placer, Reason, Skipped,
    snapshot::{Capacity, Lifetime, Machine, Request, Snapshot},
};

/// The factor of the steps by which the speed stage ranks machines, counted
/// from an average one toward zero, so a machine less than a step faster
/// or slower ranks as average and the noise of medians leaves the choice to
/// room.
const SPEED_STEP: f64 = 1.25;

/// The `balanced` policy. It ranks only the machines with room the start's
/// lifetime may go to and the caller's machine, so a start no such machine
/// has room for stays on the caller's.
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
        ranked.sort_by(|(first, _), (second, _)| first.cmp(second));
        placement.ranked = ranked.into_iter().map(|(_, placed)| placed).collect();
        placement
    }
}

/// Where a machine ranks, lowest first: each stage's key in the order the
/// stages apply, then its name. A `bool` key is false where its stage
/// prefers the machine.
#[derive(PartialEq, Eq, PartialOrd, Ord)]
struct Rank<'a> {
    fit: Fit,
    lifetime: bool,
    caller: bool,
    speed: i32,
    room: RoomKey,
    name: &'a str,
}

/// What a stage makes of a machine: its key, and the reason when it
/// prefers the machine.
type Stage<K> = (K, Option<Reason>);

/// Where `machine` ranks, with the reason of the first stage that prefers
/// it. None for a machine the hard rules keep the start from.
fn rank<'a>(request: &Request, machine: &'a Machine) -> Option<(Rank<'a>, Placed)> {
    let room = room(request, machine);
    let fit = fit(request, machine, room.is_some())?;
    let lifetime = lifetime(request, machine);
    let caller = caller(request, machine);
    let speed = speed(request, machine);
    let (room, room_reason) = room.unzip();
    let rank = Rank {
        fit: fit.0,
        lifetime: lifetime.0,
        caller: caller.0,
        speed: speed.0,
        room: room.unwrap_or_default(),
        name: &machine.name,
    };
    let reason = [fit.1, lifetime.1, caller.1, speed.1, room_reason];
    let placed = Placed {
        machine: machine.id.clone(),
        reason: reason.into_iter().flatten().next()?,
    };
    Some((rank, placed))
}

/// Whether a start may go to a machine by the hard rules: one with room it
/// may go to before the caller's without.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Fit {
    Room,
    Caller,
}

/// The hard rules, past `Request::unfit`: lasting work goes only to an
/// `always_on` machine with room, disposable work to any with room, and
/// otherwise the start stays on the caller's machine. None for any other
/// machine.
fn fit(request: &Request, machine: &Machine, room: bool) -> Option<Stage<Fit>> {
    let lifetime = request.lifetime;
    let may_go = room && (machine.always_on || lifetime == Lifetime::Disposable);
    match (may_go, machine.id == request.caller) {
        (true, _) => Some((Fit::Room, None)),
        (false, true) => Some((Fit::Caller, Some(Reason::Caller { lifetime }))),
        (false, false) => None,
    }
}

/// Disposable work prefers an `intermittent` machine, so the `always_on`
/// machines keep their room for lasting work, which only they take.
fn lifetime(request: &Request, machine: &Machine) -> Stage<bool> {
    let preferred = request.lifetime == Lifetime::Disposable && !machine.always_on;
    (!preferred, preferred.then_some(Reason::Intermittent))
}

/// The caller's machine comes first, beside the start's parent and
/// unpushed work.
fn caller(request: &Request, machine: &Machine) -> Stage<bool> {
    let caller = machine.id == request.caller;
    (!caller, caller.then_some(Reason::CallerHasRoom))
}

/// Heavy work prefers the machines fastest at its repository, and light
/// work the slowest, which keeps the fast ones free. A machine without a
/// speed there ranks as one of average speed.
fn speed(request: &Request, machine: &Machine) -> Stage<i32> {
    let Some(speed) = machine.speed else {
        return (0, None);
    };
    let steps = (f64::from(speed) / 100.0).log(SPEED_STEP).trunc() as i32;
    match request.heavy {
        true => (-steps, (steps > 0).then_some(Reason::Faster { speed })),
        false => (steps, (steps < 0).then_some(Reason::Slower { speed })),
    }
}

/// How a machine with room for `request` ranks among the others by its
/// room, lowest first, with why; none for a machine without room.
fn room(request: &Request, machine: &Machine) -> Option<(RoomKey, Reason)> {
    let capacity = machine.capacity.as_ref()?;
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
