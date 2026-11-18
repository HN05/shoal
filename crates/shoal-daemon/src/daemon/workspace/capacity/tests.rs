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

/// The slots `activities` use, in no space.
fn count(activities: impl IntoIterator<Item = Activity>) -> Agents {
    Agents::count(activities.into_iter().map(|activity| Load {
        space: None,
        activity,
    }))
}

/// What `by` reserves in no space.
fn reservation(by: Reserver, bytes: u64) -> MemoryReservation {
    MemoryReservation {
        by,
        space_id: None,
        number: None,
        bytes,
    }
}

/// What room leaves free for memory and counts an agent's start at.
fn free(room: Room) -> (u64, u64) {
    match room {
        Room::Free { memory, agent, .. } => (memory, agent),
        other => panic!("no free room: {other:?}"),
    }
}

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
        count([
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
            // The warming agents at the 3 GiB the settled one uses, then the
            // jobs: 1 GiB, 3 GiB a starting agent counts at less the 1 GiB
            // used, then half of 1 GiB.
            reserved: vec![
                reservation(Reserver::Agent, 3 * GIB),
                reservation(Reserver::Agent, 3 * GIB),
                reservation(Reserver::Job, GIB),
                reservation(Reserver::Job, 2 * GIB),
                reservation(Reserver::Job, GIB / 2),
            ],
            newest: Some(MINUTE),
        }
    );
    assert_eq!(count([Activity::Stopped]).newest, None);
}

#[test]
fn a_memory_hold_names_what_each_space_reserves() {
    let settings = Overload::default();
    let space = |id: &str| Some(SpaceId::from(id.to_owned()));
    let load = |id, activity| Load {
        space: space(id),
        activity,
    };
    let agents = Agents::count([
        load("warming", running(MINUTE, None)),
        load("settled", running(WARM_UP, Some(4 * GIB))),
        load("ended", job(WARM_UP * 50, Some(9 * GIB), Some(GIB))),
        load("peaked", job(WARM_UP, Some(GIB), Some(2 * GIB))),
        Load {
            space: None,
            activity: Activity::Starting(MINUTE),
        },
    ]);
    let reserved = |space_id, by, bytes| MemoryReservation {
        by,
        space_id,
        number: None,
        bytes,
    };
    // A settled agent and a job past its peak reserve nothing.
    assert_eq!(
        room(sample(20 * GIB), agents, &settings).fits_agent(),
        Err(Some(HeldBy::Memory {
            needed: 4 * GIB,
            left: 4 * GIB,
            reserved: vec![
                reserved(space("warming"), Reserver::Agent, 4 * GIB),
                reserved(space("ended"), Reserver::Job, 8 * GIB),
                reserved(None, Reserver::Agent, 4 * GIB),
            ],
        }))
    );
}

#[test]
fn a_starting_agent_counts_at_the_memory_settled_agents_use() {
    let settings = Overload::default();
    let room = |activities: &[Activity]| {
        free(room(
            sample(20 * GIB),
            count(activities.iter().copied()),
            &settings,
        ))
    };
    // Before any agent has settled, a warming one counts at 2 GiB.
    assert_eq!(room(&[Activity::Starting(MINUTE)]), (18 * GIB, 2 * GIB));
    // Settled agents set it: their workspaces' average, here 4 GiB.
    let learned = [
        running(WARM_UP, Some(3 * GIB)),
        running(WARM_UP, Some(5 * GIB)),
        running(MINUTE, None),
    ];
    assert_eq!(room(&learned), (16 * GIB, 4 * GIB));
    // An agent counts at 1 GiB at least.
    let idle = [running(WARM_UP, Some(GIB / 10)), running(MINUTE, None)];
    assert_eq!(room(&idle), (19 * GIB, GIB));
    // A job without earlier runs counts as an agent, one with them at what
    // they reached.
    let jobs = [job(MINUTE, None, None), job(MINUTE, Some(3 * GIB), None)];
    assert_eq!(room(&jobs), (15 * GIB, 2 * GIB));
}

#[test]
fn a_running_job_reserves_what_it_has_yet_to_use_until_it_ends() {
    let settings = Overload::default();
    let cost = JobCost {
        memory: Some(6 * GIB),
    };
    let agents = |used| count([job(WARM_UP * 5, Some(6 * GIB), used)]);
    // Long past its start, a job using 2 GiB of its 6 reserves the other 4,
    // which holds a job that would fit without them.
    assert_eq!(
        room(sample(9 * GIB), agents(Some(2 * GIB)), &settings).fits_job(cost),
        Err(Some(HeldBy::Memory {
            needed: 6 * GIB,
            left: 5 * GIB,
            reserved: vec![reservation(Reserver::Job, 4 * GIB)],
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
            reserved: vec![reservation(Reserver::Job, 6 * GIB)],
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
    let agents = count([
        running(WARM_UP, Some(4 * GIB)),
        job(WARM_UP * 5, None, Some(GIB)),
    ]);
    assert_eq!(
        free(room(sample(20 * GIB), agents, &settings)),
        (17 * GIB, 4 * GIB)
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
    let one = count([running(WARM_UP, Some(GIB))]);
    assert_eq!(
        room(sample(2 * GIB), one.clone(), &settings).fits_agent(),
        Ok(())
    );
    // Memory past its warning leaves no room, whatever is left below it.
    let warning = HostSample {
        memory: MemoryState::Warning,
        ..sample(20 * GIB)
    };
    assert_eq!(
        room(warning, one.clone(), &settings).fits_agent(),
        Err(Some(HeldBy::Memory {
            needed: GIB,
            left: 0,
            reserved: Vec::new(),
        }))
    );
    // So does low disk, a missing memory reading and, without a reason to
    // report, a start just now.
    let low_disk = HostSample {
        disk_free: Some(settings.warning.disk_free_bytes() - 1),
        ..sample(20 * GIB)
    };
    assert_eq!(
        room(low_disk, one.clone(), &settings),
        Room::Held(HeldBy::Disk)
    );
    let unread = HostSample {
        memory_room: None,
        ..sample(0)
    };
    assert_eq!(room(unread, one, &settings), Room::Held(HeldBy::Reading));
    let recent = count([job(Duration::from_secs(1), None, None)]);
    assert_eq!(room(sample(20 * GIB), recent, &settings), Room::Staggered);
    assert_eq!(Room::Staggered.fits_agent(), Err(None));
    // An idle machine starts one whatever memory and disk leave, and stopped
    // agents run nothing.
    for idle in [Agents::default(), count([Activity::Stopped])] {
        assert_eq!(room(warning, idle.clone(), &settings), Room::Idle);
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
