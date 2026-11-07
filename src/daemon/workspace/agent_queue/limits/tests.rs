use super::*;

fn limits(global: Option<u32>, repositories: &[(&str, Option<u32>)]) -> Limits {
    Limits {
        global,
        starts: None,
        repositories: repositories
            .iter()
            .map(|(id, limit)| (id.to_string(), *limit))
            .collect(),
        disk: None,
        jobs: JobLimits::default(),
    }
}

fn waiting(entries: &[(&str, &str)]) -> Vec<Waiting> {
    entries
        .iter()
        .map(|(id, repository)| Waiting {
            id: id.to_string(),
            repository: repository.to_string(),
            work: if id.starts_with('r') {
                Work::Restore {
                    waiting: true,
                    wake: Wake::Recovery,
                }
            } else {
                Work::Launch
            },
        })
        .collect()
}

#[test]
fn admission_fills_free_slots_in_order_and_passes_over_full_repositories() {
    let queue = waiting(&[("a1", "a"), ("a2", "a"), ("b1", "b"), ("b2", "b")]);
    // Repository a may run one agent and already does; b has no limit of its own.
    let limits = limits(Some(3), &[("a", Some(1)), ("b", None)]);
    assert_eq!(admit(&["a".into()], &queue, &limits, false), ["b1", "b2"]);
    // A full machine admits nothing, whatever the repositories allow.
    assert!(admit(&vec!["b".to_owned(); 3], &queue, &limits, false).is_empty());
    // Without any limit every entry starts.
    assert_eq!(admit(&[], &queue, &Limits::default(), false).len(), 4);
    // Low disk holds the machine below its fixed limit.
    let low_disk = Limits {
        disk: Some(2),
        ..limits
    };
    assert_eq!(admit(&["a".into()], &queue, &low_disk, false), ["b1"]);
    // Measured load starts as many as it allows, whatever the slots in use.
    let dynamic = Limits {
        starts: Some(1),
        ..Limits::default()
    };
    assert_eq!(
        admit(&vec!["b".to_owned(); 9], &queue, &dynamic, false),
        ["a1"]
    );
}

#[test]
fn jobs_count_only_against_their_own_limits_and_share_the_dynamic_start() {
    let job = |id: &str, cores| Waiting {
        id: id.into(),
        repository: "a".into(),
        work: Work::Job {
            cost: capacity::JobCost {
                cores,
                memory: None,
            },
        },
    };
    let mut queue = waiting(&[("a1", "a")]);
    queue.insert(0, job("j1", 2.0));
    queue.push(job("j2", 2.0));
    // A full agent limit holds back agents, not jobs; max_jobs holds jobs.
    let fixed = Limits {
        jobs: JobLimits {
            running: 1,
            fixed: Some(2),
            room: None,
        },
        ..limits(Some(1), &[])
    };
    assert_eq!(admit(&["a".into()], &queue, &fixed, false), ["j1"]);
    // Measured load starts one entry a pass, a job or an agent, where it fits.
    let dynamic = |percent| Limits {
        starts: Some(1),
        jobs: JobLimits {
            room: Some(capacity::Room::Free {
                percent,
                parallelism: 16,
                memory: None,
            }),
            ..JobLimits::default()
        },
        ..Limits::default()
    };
    assert_eq!(admit(&[], &queue, &dynamic(50.0), false), ["j1"]);
    // A job too big for the room keeps its place and the agent behind it starts.
    assert_eq!(admit(&[], &queue, &dynamic(10.0), false), ["a1"]);
    // Room too small for an agent's load still starts a smaller job.
    let no_agent = Limits {
        starts: Some(0),
        ..dynamic(50.0)
    };
    assert_eq!(admit(&[], &queue, &no_agent, false), ["j1"]);
    // An agent that started takes the pass's start from the job behind it.
    let agent_first = [queue[1].clone(), queue[0].clone()];
    assert_eq!(admit(&[], &agent_first, &dynamic(50.0), false), ["a1"]);
}

#[test]
fn measured_load_admits_nothing_beside_a_start_under_a_fixed_limit() {
    let job = Waiting {
        id: "j1".into(),
        repository: "a".into(),
        work: Work::Job {
            cost: capacity::JobCost {
                cores: 1.0,
                memory: None,
            },
        },
    };
    let agent = waiting(&[("a1", "a")]).remove(0);
    let room = Some(capacity::Room::Free {
        percent: 50.0,
        parallelism: 16,
        memory: None,
    });
    // Fixed agents and dynamic jobs: an admitted agent holds a job back, and
    // a fixed agent behind a job ignores load as always.
    let dynamic_jobs = Limits {
        jobs: JobLimits {
            room,
            ..JobLimits::default()
        },
        ..limits(Some(5), &[])
    };
    let agent_first = [agent.clone(), job.clone()];
    assert_eq!(admit(&[], &agent_first, &dynamic_jobs, false), ["a1"]);
    let job_first = [job.clone(), agent.clone()];
    assert_eq!(admit(&[], &job_first, &dynamic_jobs, false), ["j1", "a1"]);
    // Dynamic agents and fixed jobs: an admitted job holds an agent back,
    // and a fixed job behind an agent ignores load.
    let dynamic_agents = Limits {
        starts: Some(1),
        jobs: JobLimits {
            fixed: Some(2),
            ..JobLimits::default()
        },
        ..Limits::default()
    };
    assert_eq!(admit(&[], &job_first, &dynamic_agents, false), ["j1"]);
    assert_eq!(
        admit(&[], &agent_first, &dynamic_agents, false),
        ["a1", "j1"]
    );
}

#[test]
fn the_forge_is_read_while_any_room_is_left_for_a_job() {
    let room = |percent| Limits {
        jobs: JobLimits {
            room: Some(capacity::Room::Free {
                percent,
                parallelism: 16,
                memory: None,
            }),
            ..JobLimits::default()
        },
        ..Limits::default()
    };
    // Less room than two cores still fits a job whose runs used less.
    assert!(room_for_a_job(&[], &room(5.0)));
    assert!(!room_for_a_job(&[], &room(0.0)));
    let none = Limits {
        jobs: JobLimits {
            room: Some(capacity::Room::None),
            ..JobLimits::default()
        },
        ..Limits::default()
    };
    assert!(!room_for_a_job(&[], &none));
    let full = Limits {
        jobs: JobLimits {
            running: 2,
            fixed: Some(2),
            room: None,
        },
        ..Limits::default()
    };
    assert!(!room_for_a_job(&[], &full));
}

#[test]
fn restores_take_back_their_own_slot_under_fixed_limits() {
    // Restores are named r…; each one's stopped agent holds a slot in `occupied`.
    let queue = waiting(&[("a1", "a"), ("ra", "a"), ("b1", "b")]);
    let occupied = |repositories: &[&str]| -> Vec<String> {
        repositories.iter().map(|id| id.to_string()).collect()
    };
    // The machine is full: the launch waits and the restore takes its slot back.
    let full = limits(Some(3), &[("a", Some(2))]);
    assert_eq!(
        admit(&occupied(&["a", "a", "b"]), &queue, &full, true),
        ["ra"]
    );
    // A lowered machine limit holds the restore until running agents leave.
    let lowered = limits(Some(2), &[]);
    assert!(admit(&occupied(&["a", "b", "b"]), &queue, &lowered, true).is_empty());
    assert_eq!(
        admit(&occupied(&["a", "b"]), &queue, &lowered, true),
        ["ra"]
    );
    // So does a lowered repository limit, which holds back only its own entries.
    let lowered = limits(Some(4), &[("a", Some(1))]);
    assert_eq!(
        admit(&occupied(&["a", "a"]), &queue, &lowered, true),
        ["b1"]
    );
    // Low disk holds the slots in use, which a restore already has.
    let low_disk = Limits {
        disk: Some(3),
        ..Limits::default()
    };
    assert_eq!(
        admit(&occupied(&["a", "a", "b"]), &queue, &low_disk, true),
        ["ra"]
    );
}

#[test]
fn restores_wait_for_healthy_readings_without_holding_back_later_entries() {
    let mut queue = waiting(&[("a1", "a"), ("ra", "a"), ("rb", "b"), ("b1", "b")]);
    let occupied = vec!["a".to_owned(), "b".to_owned()];
    let unlimited = Limits::default();
    // Without healthy readings the restores keep their place and the
    // launches around them start.
    assert_eq!(admit(&occupied, &queue, &unlimited, false), ["a1", "b1"]);
    // Each healthy interval restores one agent; the rest keep their place.
    assert_eq!(
        admit(&occupied, &queue, &unlimited, true),
        ["a1", "ra", "b1"]
    );
    // A full machine keeps the launch behind them waiting.
    let full = limits(Some(3), &[]);
    assert_eq!(admit(&occupied, &queue, &full, false), ["a1"]);
    // A restore whose agent is not waiting yet is passed over.
    queue[1].work = Work::Restore {
        waiting: false,
        wake: Wake::Recovery,
    };
    assert_eq!(
        admit(&occupied, &queue, &unlimited, true),
        ["a1", "rb", "b1"]
    );
    // Dynamic admission measures room for a restored agent's load too.
    let starts = |starts| Limits {
        starts: Some(starts),
        ..Limits::default()
    };
    assert_eq!(admit(&occupied, &queue[1..], &starts(1), true), ["rb"]);
    assert!(admit(&occupied, &queue[1..], &starts(0), true).is_empty());
}

#[test]
fn stopped_oneshot_runs_restart_one_per_healthy_interval() {
    let mut queue = waiting(&[("o1", "a"), ("o2", "a"), ("a1", "a")]);
    for entry in &mut queue[..2] {
        entry.work = Work::OneShot { after_stop: true };
    }
    let fixed = limits(Some(5), &[]);
    // Without healthy readings both wait; the launch behind them starts.
    assert_eq!(admit(&[], &queue, &fixed, false), ["a1"]);
    // One healthy interval restarts one of them, under a fixed limit too.
    assert_eq!(admit(&[], &queue, &fixed, true), ["o1", "a1"]);
    // A run nothing stopped needs no healthy readings.
    queue[1].work = Work::OneShot { after_stop: false };
    assert_eq!(admit(&[], &queue, &fixed, false), ["o2", "a1"]);
}

#[test]
fn restores_between_turns_wait_only_for_an_update_and_a_free_slot() {
    let mut queue = waiting(&[("ra", "a"), ("a1", "a"), ("rb", "b")]);
    let update = |waiting, due| Work::Restore {
        waiting,
        wake: Wake::Update { due },
    };
    queue[0].work = update(true, false);
    let occupied = vec!["b".to_owned()];
    let unlimited = Limits::default();
    // Without an update the restore holds back nothing behind it.
    assert_eq!(admit(&occupied, &queue, &unlimited, true), ["a1", "rb"]);
    // An update starts it without healthy readings, which the restore after
    // it still waits for.
    queue[0].work = update(true, true);
    assert_eq!(admit(&occupied, &queue, &unlimited, false), ["ra", "a1"]);
    // Its agent holds no slot, so it needs a free one as a launch does.
    let full = limits(Some(2), &[]);
    assert_eq!(admit(&occupied, &queue, &full, false), ["ra"]);
    assert!(admit(&vec!["b".to_owned(); 2], &queue, &full, false).is_empty());
    // An agent not yet proven stopped is passed over.
    queue[0].work = update(false, true);
    assert_eq!(admit(&occupied, &queue, &unlimited, false), ["a1"]);
}
