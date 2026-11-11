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

fn job(age: Duration, memory: Option<u64>) -> Activity {
    Activity::Job {
        age,
        cost: JobCost { memory },
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
            job(MINUTE, Some(GIB)),
            job(MINUTE, None),
            job(WARM_UP, Some(GIB)),
            Activity::Stopped,
        ]),
        Agents {
            total: 8,
            warming: 2,
            settled: 2,
            jobs: 3,
            settled_memory: 3 * GIB,
            measured: 1,
            warming_job_memory: GIB,
            warming_new_jobs: 1,
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
    let jobs = [job(MINUTE, None), job(MINUTE, Some(3 * GIB))];
    assert_eq!(
        room(&jobs),
        Room::Free {
            memory: 15 * GIB,
            agent: 2 * GIB,
        }
    );
}

#[test]
fn a_job_that_filled_the_cpu_starts_beside_agents_when_memory_allows() {
    let settings = Overload::default();
    // Earlier runs used the whole CPU and 6 GiB; idle and busy agents run.
    let cost = JobCost {
        memory: Some(6 * GIB),
    };
    let agents = Agents::count([
        running(WARM_UP, Some(GIB)),
        running(WARM_UP * 4, Some(5 * GIB)),
    ]);
    assert_eq!(
        room(sample(7 * GIB), agents, &settings).fits_job(cost),
        Ok(())
    );
    assert_eq!(
        room(sample(6 * GIB), agents, &settings).fits_job(cost),
        Err(Some(HeldBy::Memory {
            needed: 6 * GIB,
            left: 6 * GIB,
        }))
    );
    // Agents start beside a settled job that keeps every core busy.
    let busy = Agents::count([job(WARM_UP, Some(6 * GIB)), running(WARM_UP, Some(GIB))]);
    assert_eq!(room(sample(2 * GIB), busy, &settings).fits_agent(), Ok(()));
    assert!(room(sample(GIB), busy, &settings).fits_agent().is_err());
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
    let recent = Agents::count([job(Duration::from_secs(1), None)]);
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
