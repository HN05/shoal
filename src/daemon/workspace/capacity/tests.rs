use super::*;

fn sample(cpu: f64) -> HostSample {
    HostSample {
        parallelism: 16,
        cpu: Some(CpuSample {
            recent: cpu,
            settled: cpu,
        }),
        memory: MemoryState::Healthy,
        disk_free: Some(u64::MAX),
    }
}

fn agents(warming: u32, settled: u32, newest: u64) -> Agents {
    Agents {
        total: warming + settled,
        warming,
        settled,
        newest: Some(Duration::from_secs(newest)),
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
fn slots_count_by_how_far_their_load_shows() {
    let minute = Duration::from_secs(60);
    assert_eq!(
        Agents::count([
            Activity::Starting(minute * 5),
            Activity::Running(minute),
            Activity::Running(WARM_UP),
            Activity::Stopped,
        ]),
        Agents {
            total: 4,
            warming: 2,
            settled: 1,
            newest: Some(minute),
        }
    );
    assert_eq!(Agents::count([Activity::Stopped]).newest, None);
}

#[test]
fn an_idle_machine_starts_agents_until_warming_ones_fill_the_target() {
    let settings = Overload::default();
    // Sixteen cores, two assumed per agent: 12.5% each below an 80% target.
    let mut warming = 0;
    while limit(sample(5.0), agents(warming, 0, 60), &settings) > warming {
        warming += 1;
    }
    assert_eq!(warming, 5);
    // Starts are staggered even with room.
    assert_eq!(limit(sample(5.0), agents(1, 0, 1), &settings), 1);
}

#[test]
fn settled_agents_set_the_load_a_new_agent_is_expected_to_add() {
    let settings = Overload::default();
    // Ten mostly idle settled agents at 20%: each counts its 3.125% floor.
    assert_eq!(limit(sample(20.0), agents(0, 10, 600), &settings), 11);
    assert_eq!(limit(sample(20.0), agents(15, 10, 60), &settings), 26);
    assert_eq!(limit(sample(20.0), agents(19, 10, 60), &settings), 29);
    // Two busy settled agents at 60% leave no room for a third at 30%.
    assert_eq!(limit(sample(60.0), agents(0, 2, 600), &settings), 2);
    let mut full = Overload::default();
    full.warning.cpu_used_percent = 95;
    assert_eq!(limit(sample(60.0), agents(0, 2, 600), &full), 3);
}

#[test]
fn pressure_pauses_starts_and_critical_readings_block_even_the_first() {
    let settings = Overload::default();
    let busy = sample(90.0);
    assert_eq!(limit(busy, Agents::default(), &settings), 1);
    assert_eq!(limit(busy, agents(0, 1, 600), &settings), 1);
    let unmeasured = HostSample {
        cpu: None,
        ..sample(0.0)
    };
    assert_eq!(limit(unmeasured, Agents::default(), &settings), 1);
    // Stopped agents run nothing, so the machine is still idle.
    let stopped = Agents::count([Activity::Stopped, Activity::Stopped]);
    assert_eq!(limit(unmeasured, stopped, &settings), 3);
    assert_eq!(limit(unmeasured, agents(0, 1, 600), &settings), 1);
    let warning = HostSample {
        memory: MemoryState::Warning,
        ..sample(0.0)
    };
    assert_eq!(limit(warning, agents(0, 1, 600), &settings), 1);
    let low_disk = HostSample {
        disk_free: Some(settings.warning.disk_free_bytes() - 1),
        ..sample(0.0)
    };
    assert_eq!(limit(low_disk, agents(0, 1, 600), &settings), 1);
    for blocked in [
        HostSample {
            memory: MemoryState::Critical,
            ..sample(0.0)
        },
        HostSample {
            memory: MemoryState::Unavailable,
            ..sample(0.0)
        },
        HostSample {
            disk_free: None,
            ..sample(0.0)
        },
        HostSample {
            disk_free: Some(settings.disk.stop_free_bytes() - 1),
            ..sample(0.0)
        },
    ] {
        assert_eq!(limit(blocked, Agents::default(), &settings), 0);
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
