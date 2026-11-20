use super::*;

const GIB: u64 = 1 << 30;

fn sample(memory_room: u64) -> HostSample {
    HostSample {
        memory: MemoryState::Healthy,
        memory_room: Some(memory_room),
        disk_free: Some(u64::MAX),
    }
}

fn running(age: Duration, memory: Option<u64>) -> Activity {
    Activity::Running { age, memory }
}

/// A job admitted `age` ago, expected to use `expected` and using `used`.
fn job(age: Duration, expected: Option<u64>, used: Option<u64>) -> Activity {
    Activity::Job {
        age,
        cost: JobCost { memory: expected },
        memory: used,
    }
}

const MINUTE: Duration = Duration::from_secs(60);

#[test]
fn disk_pressure_includes_workspaces_and_requires_complete_readings() {
    let state = Path::new("/state");
    let workspace = Path::new("/external/workspace");
    let paths = [state, workspace];
    assert_eq!(
        sample_disk(&paths, true, |path| Ok(if path == state { 100 } else { 1 })),
        Some(1),
    );
    assert_eq!(
        sample_disk(&paths, true, |path| {
            if path == state {
                Ok(100)
            } else {
                Err(std::io::ErrorKind::PermissionDenied.into())
            }
        }),
        None,
    );
    assert_eq!(
        sample_disk(&paths, false, |_| panic!("disabled disk probe")),
        Some(u64::MAX),
    );
    assert_eq!(
        sample_disk(&paths, true, |path| {
            if path == state {
                Ok(100)
            } else {
                Err(std::io::ErrorKind::NotFound.into())
            }
        }),
        Some(100),
    );
    assert_eq!(
        sample_disk(&paths, true, |_| Err(std::io::ErrorKind::NotFound.into())),
        None,
    );
}

#[test]
fn slots_count_by_how_far_their_memory_shows() {
    assert_eq!(
        Agents::count([
            Activity::Starting(MINUTE * 5),
            running(MINUTE, Some(GIB)),
            running(WARM_UP, Some(3 * GIB)),
            running(WARM_UP, None),
            job(MINUTE, Some(GIB), None),
            job(MINUTE, None, Some(GIB)),
            job(WARM_UP, Some(GIB), Some(GIB / 2)),
            Activity::Stopped,
        ]),
        Agents {
            total: 8,
            warming: 2,
            settled: 2,
            jobs: 3,
            settled_memory: 3 * GIB,
            measured: 1,
            // 1 GiB, then 3 GiB a starting agent counts at less the 1 GiB
            // used, then half of 1 GiB.
            job_reservation: GIB + 2 * GIB + GIB / 2,
            newest: Some(MINUTE),
        }
    );
    assert_eq!(Agents::count([Activity::Stopped]).newest, None);
}

#[test]
fn a_starting_agent_counts_at_the_memory_settled_agents_use() {
    let settings = Overload::default();
    let room = |activities: &[Activity]| {
        room(
            sample(20 * GIB),
            Agents::count(activities.iter().copied()),
            &settings,
        )
    };
    // Before any agent has settled, a warming one counts at 2 GiB.
    assert_eq!(
        room(&[Activity::Starting(MINUTE)]),
        Room::Free {
            memory: 18 * GIB,
            agent: 2 * GIB,
        }
    );
    // Settled agents set it: their workspaces' average, here 4 GiB.
    let learned = [
        running(WARM_UP, Some(3 * GIB)),
        running(WARM_UP, Some(5 * GIB)),
        running(MINUTE, None),
    ];
    assert_eq!(
        room(&learned),
        Room::Free {
            memory: 16 * GIB,
            agent: 4 * GIB,
        }
    );
    // An agent counts at 1 GiB at least.
    let idle = [running(WARM_UP, Some(GIB / 10)), running(MINUTE, None)];
    assert_eq!(
        room(&idle),
        Room::Free {
            memory: 19 * GIB,
            agent: GIB,
        }
    );
    // A job without earlier runs counts as an agent, one with them at what
    // they reached.
    let jobs = [job(MINUTE, None, None), job(MINUTE, Some(3 * GIB), None)];
    assert_eq!(
        room(&jobs),
        Room::Free {
            memory: 15 * GIB,
            agent: 2 * GIB,
        }
    );
}

#[test]
fn a_running_job_reserves_what_it_has_yet_to_use_until_it_ends() {
    let settings = Overload::default();
    let cost = JobCost {
        memory: Some(6 * GIB),
    };
    let agents = |used| Agents::count([job(WARM_UP * 5, Some(6 * GIB), used)]);
    // Long past its start, a job using 2 GiB of its 6 reserves the other 4,
    // which holds a job that would fit without them.
    assert_eq!(
        room(sample(9 * GIB), agents(Some(2 * GIB)), &settings).fits_job(cost),
        Err(Some(HeldBy::Memory {
            needed: 6 * GIB,
            left: 5 * GIB,
        }))
    );
    assert_eq!(
        room(sample(11 * GIB), agents(Some(2 * GIB)), &settings).fits_job(cost),
        Ok(())
    );
    // Before its first usage sample it reserves the whole of it.
    assert_eq!(
        room(sample(12 * GIB), agents(None), &settings).fits_job(cost),
        Err(Some(HeldBy::Memory {
            needed: 6 * GIB,
            left: 6 * GIB,
        }))
    );
    // Using its expected peak or more, it reserves nothing.
    for used in [6 * GIB, 7 * GIB] {
        assert_eq!(
            room(sample(7 * GIB), agents(Some(used)), &settings).fits_job(cost),
            Ok(())
        );
    }
}

#[test]
fn a_job_without_runs_reserves_an_agents_start_less_its_use() {
    let settings = Overload::default();
    // Settled agents set an agent's start at 4 GiB; the job uses 1 GiB.
    let agents = Agents::count([
        running(WARM_UP, Some(4 * GIB)),
        job(WARM_UP * 5, None, Some(GIB)),
    ]);
    assert_eq!(
        room(sample(20 * GIB), agents, &settings),
        Room::Free {
            memory: 17 * GIB,
            agent: 4 * GIB,
        }
    );
}

#[test]
fn a_job_beside_running_ones_waits_for_a_busy_cpu_and_the_first_never_does() {
    let cpu = |busy| Cpu { busy, level: 95 };
    for busy in [None, Some(0.0), Some(100.0)] {
        assert_eq!(cpu(busy).fits_job(0), Ok(()));
    }
    assert_eq!(cpu(Some(94.9)).fits_job(1), Ok(()));
    for (busy, running) in [(97.5, 1), (95.0, 2)] {
        assert_eq!(
            cpu(Some(busy)).fits_job(running),
            Err(HeldBy::Cpu {
                busy: busy as u8,
                level: 95,
            })
        );
    }
    assert_eq!(cpu(None).fits_job(1), Err(HeldBy::Reading));
}

#[test]
fn memory_disk_and_the_stagger_stop_starts_and_critical_readings_block_even_the_first() {
    let settings = Overload::default();
    let one = Agents::count([running(WARM_UP, Some(GIB))]);
    assert_eq!(room(sample(2 * GIB), one, &settings).fits_agent(), Ok(()));
    // Memory past its warning leaves no room, whatever is left below it.
    let warning = HostSample {
        memory: MemoryState::Warning,
        ..sample(20 * GIB)
    };
    assert_eq!(
        room(warning, one, &settings).fits_agent(),
        Err(Some(HeldBy::Memory {
            needed: GIB,
            left: 0,
        }))
    );
    // So does low disk, a missing memory reading and, without a reason to
    // report, a start just now.
    let low_disk = HostSample {
        disk_free: Some(settings.warning.disk_free_bytes() - 1),
        ..sample(20 * GIB)
    };
    assert_eq!(room(low_disk, one, &settings), Room::Held(HeldBy::Disk));
    let unread = HostSample {
        memory_room: None,
        ..sample(0)
    };
    assert_eq!(room(unread, one, &settings), Room::Held(HeldBy::Reading));
    let recent = Agents::count([job(Duration::from_secs(1), None, None)]);
    assert_eq!(room(sample(20 * GIB), recent, &settings), Room::Staggered);
    assert_eq!(Room::Staggered.fits_agent(), Err(None));
    // An idle machine starts one whatever memory and disk leave, and stopped
    // agents run nothing.
    for idle in [Agents::default(), Agents::count([Activity::Stopped])] {
        assert_eq!(room(warning, idle, &settings), Room::Idle);
        assert_eq!(room(low_disk, idle, &settings), Room::Idle);
    }
    for blocked in [
        HostSample {
            memory: MemoryState::Critical,
            ..sample(0)
        },
        HostSample {
            memory: MemoryState::Unavailable,
            ..sample(0)
        },
        HostSample {
            disk_free: None,
            ..sample(0)
        },
        HostSample {
            disk_free: Some(settings.disk.stop_free_bytes() - 1),
            ..sample(0)
        },
    ] {
        assert_eq!(
            room(blocked, Agents::default(), &settings),
            Room::Held(HeldBy::Reading)
        );
    }
}

#[test]
fn low_disk_holds_fixed_limits_and_critical_disk_starts_none() {
    let settings = Overload::default();
    let warning = settings.warning.disk_free_bytes();
    let stop = settings.disk.stop_free_bytes();
    assert_eq!(disk_limit(Some(warning), 3, &settings), None);
    assert_eq!(disk_limit(Some(warning - 1), 3, &settings), Some(3));
    assert_eq!(disk_limit(Some(stop), 0, &settings), Some(1));
    assert_eq!(disk_limit(Some(stop - 1), 0, &settings), Some(0));
    assert_eq!(disk_limit(None, 0, &settings), Some(0));
}

#[test]
fn memory_stall_holds_starts_as_memory_in_use_does() {
    let calm = load::MemoryPressure {
        critical: false,
        stalled: false,
        warning: false,
        safe: true,
    };
    // Stall that has yet to hold stops nothing, but starts nothing either,
    // even on an idle machine.
    let stalled = load::MemoryPressure {
        stalled: true,
        ..calm
    };
    assert_eq!(memory_state(stalled), MemoryState::Critical);
    let settings = Overload::default();
    let idle = HostSample {
        memory: memory_state(stalled),
        ..sample(u64::MAX)
    };
    assert_eq!(
        room(idle, Agents::default(), &settings),
        Room::Held(HeldBy::Reading)
    );
    let warning = load::MemoryPressure {
        warning: true,
        ..calm
    };
    assert_eq!(memory_state(warning), MemoryState::Warning);
    assert_eq!(memory_state(calm), MemoryState::Healthy);
}

#[test]
fn work_under_a_limit_has_the_room_its_limit_leaves() {
    let settings = Overload::default();
    let gib = |count: u64| count << 30;
    let pressure = load::MemoryPressure {
        critical: false,
        stalled: false,
        warning: false,
        safe: true,
    };
    let machine = load::Memory {
        total: gib(100),
        available: gib(90),
    };
    let reading = |work| load::MemoryReading {
        memory: machine,
        work,
        pressure,
    };
    let warning = u64::from(settings.warning.memory_used_percent);
    assert_eq!(
        reading_room(&reading(None), &settings),
        gib(100) * warning / 100 - gib(10)
    );
    let work = load::Memory {
        total: gib(10),
        available: gib(4),
    };
    assert_eq!(
        reading_room(&reading(Some(work)), &settings),
        gib(10) * warning / 100 - gib(6)
    );
}
