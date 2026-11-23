use super::Balanced;
use crate::placement::{
    Placer, Reason, Skipped, SkippedBy,
    snapshot::{Capacity, Machine, Request, Snapshot},
};

const ORIGIN: &str = "example.com/owner/repo";

fn request(heavy: bool) -> Request {
    Request {
        origin: Some(ORIGIN.into()),
        base: None,
        agent: "claude".into(),
        mode: "worker".into(),
        heavy,
        caller: "laptop".into(),
        parent: None,
    }
}

/// A machine with the repository and the agent tool, of `capacity`.
fn machine(id: &str, always_on: bool, capacity: Capacity) -> Machine {
    Machine {
        id: id.into(),
        name: id.into(),
        always_on,
        agents: vec!["claude".into()],
        capacity: Some(Capacity {
            repositories: vec![ORIGIN.into()],
            ..capacity
        }),
    }
}

fn capacity(heavy_free: u32, memory_free: u64, heavy_waiting: u32) -> Capacity {
    Capacity {
        heavy_free,
        memory_free: Some(memory_free),
        heavy_waiting,
        light_free: true,
        ..Capacity::default()
    }
}

/// The machines `request` goes to among `machines`, first first, with why.
fn ranked(request: Request, machines: Vec<Machine>) -> Vec<(String, Reason)> {
    let placement = Balanced.place(&Snapshot { request, machines });
    (placement.ranked.into_iter())
        .map(|placed| (placed.machine.to_string(), placed.reason))
        .collect()
}

fn laptop() -> Machine {
    machine("laptop", false, capacity(1, 8, 0))
}

#[test]
fn a_heavy_start_goes_to_the_always_on_machine_with_most_memory_and_a_free_slot() {
    let machines = vec![
        laptop(),
        machine("full", true, capacity(0, 64, 0)),
        machine("small", true, capacity(1, 4, 0)),
        machine("large", true, capacity(2, 32, 0)),
    ];
    let order: Vec<String> = (ranked(request(true), machines).into_iter())
        .map(|(machine, _)| machine)
        .collect();
    // A machine without room is never one to go to.
    assert_eq!(order, ["large", "small", "laptop"]);
}

#[test]
fn a_light_start_goes_first_to_a_machine_that_runs_no_heavy_agents() {
    let no_heavy = Capacity {
        max_heavy_agents: Some(0),
        ..capacity(0, 1, 0)
    };
    let machines = vec![
        laptop(),
        machine("waiting", true, capacity(0, 64, 3)),
        machine("idle", true, capacity(1, 32, 0)),
        machine("light", true, no_heavy),
    ];
    let ranked = ranked(request(false), machines);
    let order: Vec<&str> = ranked.iter().map(|(machine, _)| machine.as_str()).collect();
    assert_eq!(order, ["light", "idle", "waiting", "laptop"]);
    assert_eq!(ranked[2].1, Reason::HeavyWaiting { waiting: 3 });
}

#[test]
fn a_start_stays_on_the_callers_machine_without_an_always_on_machine_with_room() {
    let held = Capacity {
        light_free: false,
        ..capacity(0, 0, 0)
    };
    let machines = vec![
        machine("desk", false, capacity(4, 64, 0)),
        laptop(),
        machine("server", true, held),
    ];
    let first = |heavy| ranked(request(heavy), machines.clone()).remove(0);
    assert_eq!(first(true), ("laptop".into(), Reason::Caller));
    assert_eq!(first(false), ("laptop".into(), Reason::Caller));
    // A caller skipped for its agent tool, which the hub may not know of,
    // leaves no machine to go to rather than one without room.
    let mut machines = machines.clone();
    machines[1].agents.clear();
    assert_eq!(ranked(request(true), machines), []);
}

#[test]
fn an_always_on_caller_with_room_keeps_its_start() {
    let mut request = request(true);
    request.caller = "home".into();
    let machines = vec![
        machine("home", true, capacity(1, 4, 0)),
        machine("large", true, capacity(1, 64, 0)),
    ];
    assert_eq!(ranked(request, machines)[0].0, "home");
}

#[test]
fn a_start_skips_machines_without_its_repository_or_agent_tool() {
    let mut elsewhere = machine("elsewhere", true, capacity(1, 64, 0));
    (elsewhere.capacity.as_mut().into_iter()).for_each(|capacity| capacity.repositories.clear());
    let mut codex = machine("codex", true, capacity(1, 64, 0));
    codex.agents = vec!["codex".into()];
    let machines = vec![laptop(), elsewhere, codex];
    let placement = Balanced.place(&Snapshot {
        request: request(true),
        machines: machines.clone(),
    });
    let skipped = |machine: &str, why| Skipped {
        machine: machine.into(),
        why,
    };
    assert_eq!(
        placement.skipped,
        [
            skipped("elsewhere", SkippedBy::NoRepository),
            skipped("codex", SkippedBy::NoAgent)
        ]
    );
    // The caller's machine has the repository the start found there.
    let mut caller = laptop();
    (caller.capacity.as_mut().into_iter()).for_each(|capacity| capacity.repositories.clear());
    let snapshot = Snapshot {
        request: request(true),
        machines: vec![caller],
    };
    assert_eq!(Balanced.place(&snapshot).ranked[0].reason, Reason::Caller);
    // A chat without a repository needs none.
    let mut chat = request(true);
    chat.origin = None;
    assert_eq!(ranked(chat, machines)[0].0, "elsewhere");
}

#[test]
fn a_start_skips_machines_whose_user_walked_away() {
    let away = |id: &str| {
        let mut machine = machine(id, true, capacity(1, 64, 0));
        (machine.capacity.as_mut().into_iter()).for_each(|capacity| capacity.away = true);
        machine
    };
    let mut caller = away("laptop");
    caller.always_on = false;
    let machines = vec![
        caller,
        away("desktop"),
        machine("server", true, capacity(1, 8, 0)),
    ];
    let placement = Balanced.place(&Snapshot {
        request: request(true),
        machines,
    });
    let skipped: Vec<_> = (placement.skipped.iter())
        .map(|skipped| (skipped.machine.to_string(), skipped.why))
        .collect();
    let away = |id: &str| (id.to_owned(), SkippedBy::Away);
    assert_eq!(skipped, [away("laptop"), away("desktop")]);
    assert_eq!(placement.chosen().unwrap().machine.to_string(), "server");
}
