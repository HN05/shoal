use super::Balanced;
use crate::placement::{
    Placer, Reason, Skipped, SkippedBy,
    snapshot::{
        AgentStart, Capacity, DEMAND_WINDOW, Demand, Exclusive, Lifetime, Machine, Registered,
        Request, Snapshot,
    },
};

const ORIGIN: &str = "example.com/owner/repo";

fn request(heavy: bool) -> Request {
    Request {
        origin: Some(ORIGIN.into()),
        base: None,
        agent: Some(AgentStart {
            agent: "claude".into(),
            mode: "worker".into(),
        }),
        heavy,
        lifetime: Lifetime::Lasting,
        caller: "laptop".into(),
        parent: None,
    }
}

/// A machine that launches claude in the repository and outside one, of
/// `capacity`.
fn machine(id: &str, always_on: bool, capacity: Capacity) -> Machine {
    Machine {
        id: id.into(),
        name: id.into(),
        always_on,
        capacity: Some(Capacity {
            repositories: vec![registered(&["claude"])],
            agents: vec!["claude".into()],
            ..capacity
        }),
        speed: None,
        exclusive: Exclusive::default(),
    }
}

fn registered(agents: &[&str]) -> Registered {
    Registered {
        origin: ORIGIN.into(),
        agents: agents.iter().map(|agent| (*agent).into()).collect(),
        ..Registered::default()
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

fn disposable(heavy: bool) -> Request {
    Request {
        lifetime: Lifetime::Disposable,
        ..request(heavy)
    }
}

fn laptop() -> Machine {
    machine("laptop", false, capacity(1, 8, 0))
}

/// A machine whose admission starts nothing now.
fn held(id: &str, always_on: bool) -> Machine {
    let held = Capacity {
        light_free: false,
        ..capacity(0, 0, 0)
    };
    machine(id, always_on, held)
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

/// An `always_on` machine with room for a heavy agent, at `speed`.
fn at_speed(id: &str, speed: Option<u32>) -> Machine {
    Machine {
        speed,
        ..machine(id, true, capacity(1, 32, 0))
    }
}

#[test]
fn a_heavy_start_goes_to_the_fastest_machine_and_a_light_one_to_the_slowest() {
    let machines = vec![
        laptop(),
        at_speed("unknown", None),
        at_speed("fast", Some(200)),
        at_speed("slow", Some(50)),
        // Less than a step faster than average, which leaves the order to
        // its room.
        Machine {
            speed: Some(120),
            ..machine("roomy", true, capacity(1, 64, 0))
        },
    ];
    let heavy = ranked(request(true), machines.clone());
    let order: Vec<&str> = heavy.iter().map(|(machine, _)| machine.as_str()).collect();
    // A machine without a speed ranks as an average one, not a slow one.
    assert_eq!(order, ["fast", "roomy", "unknown", "slow", "laptop"]);
    assert_eq!(heavy[0].1, Reason::Faster { speed: 200 });
    let light = ranked(request(false), machines);
    let order: Vec<&str> = light.iter().map(|(machine, _)| machine.as_str()).collect();
    assert_eq!(order, ["slow", "roomy", "unknown", "fast", "laptop"]);
    assert_eq!(light[0].1, Reason::Slower { speed: 50 });
}

#[test]
fn a_speed_less_than_a_step_from_average_leaves_the_order_to_room() {
    let speed = |id: &str, speed, memory| Machine {
        speed: Some(speed),
        ..machine(id, true, capacity(1, memory, 0))
    };
    let order = |heavy, machines| -> Vec<String> {
        (ranked(request(heavy), machines).into_iter())
            .map(|(machine, _)| machine)
            .collect()
    };
    let heavy = vec![speed("quicker", 120, 8), speed("average", 100, 64)];
    assert_eq!(order(true, heavy), ["average", "quicker"]);
    let light = vec![speed("slower", 81, 8), speed("average", 100, 64)];
    assert_eq!(order(false, light), ["average", "slower"]);
    // A step past average leads whatever the room.
    let heavy = vec![speed("quicker", 130, 8), speed("average", 100, 64)];
    assert_eq!(order(true, heavy), ["quicker", "average"]);
}

#[test]
fn a_lasting_start_stays_on_the_callers_machine_without_an_always_on_machine_with_room() {
    let machines = vec![
        machine("desk", false, capacity(4, 64, 0)),
        laptop(),
        held("server", true),
    ];
    let caller = Reason::Caller {
        lifetime: Lifetime::Lasting,
    };
    // An intermittent machine other than the caller's never takes lasting
    // work, whatever its room.
    let all = |heavy| ranked(request(heavy), machines.clone());
    assert_eq!(all(true), [("laptop".into(), caller)]);
    assert_eq!(all(false), [("laptop".into(), caller)]);
    // A caller skipped for its agent tool, which the hub may not know of,
    // leaves no machine to go to rather than one without room.
    let mut machines = machines.clone();
    (machines[1].capacity.as_mut().into_iter()).for_each(|capacity| {
        capacity.repositories = vec![registered(&[])];
        capacity.agents.clear();
    });
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
    assert_eq!(
        ranked(request, machines)[0],
        ("home".into(), Reason::CallerHasRoom)
    );
}

#[test]
fn a_disposable_start_goes_to_an_intermittent_machine_with_room_before_an_always_on_one() {
    let machines = vec![
        held("laptop", false),
        machine("server", true, capacity(4, 64, 0)),
        machine("desk", false, capacity(1, 8, 0)),
        held("tablet", false),
    ];
    let ranked = ranked(disposable(true), machines);
    let order: Vec<&str> = ranked.iter().map(|(machine, _)| machine.as_str()).collect();
    assert_eq!(order, ["desk", "server", "laptop"]);
    assert_eq!(ranked[0].1, Reason::Intermittent);
    assert_eq!(
        ranked[1].1,
        Reason::HeavySlot {
            memory_free: Some(64)
        }
    );
}

#[test]
fn a_disposable_start_leaves_an_always_on_caller_for_an_intermittent_machine() {
    let mut request = disposable(false);
    request.caller = "home".into();
    let machines = vec![
        machine("home", true, capacity(1, 64, 0)),
        machine("desk", false, capacity(1, 8, 0)),
    ];
    let order: Vec<String> = (ranked(request, machines).into_iter())
        .map(|(machine, _)| machine)
        .collect();
    assert_eq!(order, ["desk", "home"]);
}

#[test]
fn a_disposable_start_stays_on_an_intermittent_caller_with_room() {
    let machines = vec![
        machine("desk", false, capacity(4, 64, 0)),
        laptop(),
        machine("server", true, capacity(4, 64, 0)),
    ];
    let order: Vec<String> = (ranked(disposable(true), machines).into_iter())
        .map(|(machine, _)| machine)
        .collect();
    assert_eq!(order, ["laptop", "desk", "server"]);
}

#[test]
fn a_disposable_start_stays_on_the_callers_machine_without_a_machine_with_room() {
    let machines = vec![held("desk", false), held("server", true)];
    let mut request = disposable(true);
    request.caller = "desk".into();
    let caller = Reason::Caller {
        lifetime: Lifetime::Disposable,
    };
    assert_eq!(ranked(request, machines), [("desk".into(), caller)]);
}

#[test]
fn a_start_skips_machines_without_its_repository_or_agent_tool() {
    let mut elsewhere = machine("elsewhere", true, capacity(1, 64, 0));
    (elsewhere.capacity.as_mut().into_iter()).for_each(|capacity| capacity.repositories.clear());
    let mut codex = machine("codex", true, capacity(1, 64, 0));
    (codex.capacity.as_mut().into_iter()).for_each(|capacity| {
        capacity.repositories = vec![registered(&["codex"])];
        capacity.agents = vec!["codex".into()];
    });
    let mut undescribed = machine("undescribed", true, capacity(1, 64, 0));
    undescribed.capacity = None;
    let machines = vec![laptop(), elsewhere, codex, undescribed];
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
            skipped("codex", SkippedBy::NoAgent),
            skipped("undescribed", SkippedBy::Undescribed),
        ]
    );
    // The caller's machine has the repository the start found there.
    let mut caller = laptop();
    (caller.capacity.as_mut().into_iter()).for_each(|capacity| capacity.repositories.clear());
    let snapshot = Snapshot {
        request: request(true),
        machines: vec![caller],
    };
    let caller = Reason::Caller {
        lifetime: Lifetime::Lasting,
    };
    assert_eq!(Balanced.place(&snapshot).ranked[0].reason, caller);
    // A chat without a repository needs none.
    let mut chat = request(true);
    chat.origin = None;
    assert_eq!(ranked(chat, machines)[0].0, "elsewhere");
}

#[test]
fn a_machine_launches_the_agents_of_each_repository_by_its_settings() {
    let mut server = machine("server", true, capacity(1, 64, 0));
    (server.capacity.as_mut().into_iter())
        .for_each(|capacity| capacity.repositories = vec![registered(&["claude", "reviewer"])]);
    let mut start = request(true);
    (start.agent.iter_mut()).for_each(|start| start.agent = "reviewer".into());
    let machines = vec![laptop(), server];
    assert_eq!(ranked(start.clone(), machines.clone())[0].0, "server");
    // Outside the repository, its config defines no such agent.
    start.origin = None;
    let placement = Balanced.place(&Snapshot {
        request: start,
        machines,
    });
    let skipped: Vec<_> = (placement.skipped.iter())
        .map(|skipped| (skipped.machine.to_string(), skipped.why))
        .collect();
    let no_agent = |id: &str| (id.to_owned(), SkippedBy::NoAgent);
    assert_eq!(skipped, [no_agent("laptop"), no_agent("server")]);
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

#[test]
fn a_start_skips_a_paused_machine_even_while_its_user_is_away() {
    let mut paused = machine("desktop", true, capacity(4, 64, 0));
    (paused.capacity.as_mut().into_iter()).for_each(|capacity| {
        capacity.paused = Some(shoal_time::UnixTime::from_seconds(10));
        capacity.away = true;
    });
    let machines = vec![laptop(), paused];
    let placement = Balanced.place(&Snapshot {
        request: request(true),
        machines,
    });
    let skipped: Vec<_> = (placement.skipped.iter())
        .map(|skipped| (skipped.machine.to_string(), skipped.why))
        .collect();
    assert_eq!(skipped, [("desktop".to_owned(), SkippedBy::Paused)]);
    assert_eq!(placement.chosen().unwrap().machine.to_string(), "laptop");
}

/// `machine`, which alone serves another repository, of `demand`.
fn serving_alone(machine: Machine, demand: Demand) -> Machine {
    Machine {
        exclusive: Exclusive {
            repositories: vec!["example.com/owner/app".into()],
            demand,
        },
        ..machine
    }
}

/// The demand of a repository that kept a machine busy for `percent` of
/// the demand window, with `heavy_waiting` heavy agents waiting.
fn demand(percent: u64, heavy_waiting: u32) -> Demand {
    Demand {
        busy_seconds: DEMAND_WINDOW.as_secs() * percent / 100,
        heavy_waiting,
    }
}

fn order(request: Request, machines: Vec<Machine>) -> Vec<String> {
    (ranked(request, machines).into_iter())
        .map(|(machine, _)| machine)
        .collect()
}

#[test]
fn work_of_another_repository_avoids_a_machine_whose_exclusive_work_waits() {
    let mac = serving_alone(machine("mac", true, capacity(1, 64, 1)), demand(0, 1));
    let linux = machine("linux", true, capacity(1, 8, 0));
    assert_eq!(
        order(request(true), vec![laptop(), mac.clone(), linux]),
        ["linux", "mac", "laptop"]
    );
    // While no other machine fits, the start still goes there.
    assert_eq!(
        order(
            request(true),
            vec![laptop(), mac.clone(), held("linux", true)]
        ),
        ["mac", "laptop"]
    );
    // Its own repository's work is no other repository's.
    let own = Request {
        origin: Some("example.com/owner/app".into()),
        ..request(true)
    };
    let mac = Machine {
        capacity: (mac.capacity).map(|capacity| Capacity {
            repositories: vec![Registered {
                origin: "example.com/owner/app".into(),
                ..registered(&["claude"])
            }],
            ..capacity
        }),
        ..mac
    };
    assert_eq!(order(own, vec![laptop(), mac]), ["mac", "laptop"]);
}

#[test]
fn a_machine_whose_exclusive_repositories_kept_it_busy_ranks_later() {
    let busy = serving_alone(machine("mac", true, capacity(1, 64, 0)), demand(30, 0));
    let linux = machine("linux", true, capacity(1, 8, 0));
    assert_eq!(
        order(request(true), vec![busy, linux.clone()]),
        ["linux", "mac"]
    );
    // A rarely used exclusive repository costs nothing.
    let rarely = serving_alone(machine("mac", true, capacity(1, 64, 0)), demand(5, 0));
    assert_eq!(order(request(true), vec![rarely, linux]), ["mac", "linux"]);
}

#[test]
fn a_ci_job_ranks_machines_without_agent_tools_by_where_it_has_room() {
    let mut job = disposable(true);
    job.agent = None;
    job.caller = "server".into();
    let launches_none = |mut machine: Machine| {
        for capacity in machine.capacity.iter_mut() {
            capacity.agents.clear();
            (capacity.repositories.iter_mut()).for_each(|registered| registered.agents.clear());
        }
        machine
    };
    let machines = vec![
        launches_none(machine("server", true, capacity(1, 64, 0))),
        launches_none(machine("desk", false, capacity(1, 8, 0))),
        held("tablet", false),
    ];
    let placement = Balanced.place(&Snapshot {
        request: job,
        machines,
    });
    assert_eq!(placement.rank_of(&"desk".into()), 0);
    assert_eq!(placement.rank_of(&"server".into()), 1);
    // A machine without room ranks after all that have it.
    assert_eq!(placement.rank_of(&"tablet".into()), 2);
}
