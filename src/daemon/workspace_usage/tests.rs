use super::*;

const SECOND: u64 = 1_000_000_000;

fn process(pid: u32, parent: u32, workspace: &str, cpu: u64) -> (Identity, (String, Reading)) {
    reaping(pid, parent, workspace, cpu, 0)
}

/// `process` with `reaped` nanoseconds of children it waited for.
fn reaping(
    pid: u32,
    parent: u32,
    workspace: &str,
    cpu: u64,
    reaped: u64,
) -> (Identity, (String, Reading)) {
    (
        Identity {
            pid,
            birth: format!("birth-{pid}"),
            unique: None,
        },
        (
            workspace.into(),
            Reading {
                parent,
                cpu,
                reaped,
                memory: 1024,
            },
        ),
    )
}

fn sample(
    time: Instant,
    processes: impl IntoIterator<Item = (Identity, (String, Reading))>,
) -> Sample {
    Sample {
        time,
        processes: processes.into_iter().collect(),
        unreaped: HashSet::new(),
    }
}

#[tokio::test(start_paused = true)]
async fn cpu_counts_new_and_reaped_processes_once() {
    let start = Instant::now();
    let first = sample(
        start,
        [
            process(10, 1, "a", 5 * SECOND),
            process(11, 10, "a", 2 * SECOND),
            process(20, 1, "b", SECOND),
        ],
    );
    let alone = usage(None, &first, &mut HashMap::new());
    assert_eq!(alone["a"].processes, 2);
    assert_eq!(alone["a"].memory_bytes, 2048);
    assert_eq!(alone["a"].cpu_percent, None);
    // Over ten seconds, 11 used one more second before 10 reaped it,
    // 12 started and used two, and 10 used one itself.
    let second = sample(
        start + Duration::from_secs(10),
        [
            reaping(10, 1, "a", 6 * SECOND, 3 * SECOND),
            process(12, 10, "a", 2 * SECOND),
            process(20, 1, "b", SECOND),
        ],
    );
    let usage = usage(Some(&first), &second, &mut HashMap::new());
    assert_eq!(usage["a"].cpu_percent, Some(40.0));
    assert_eq!(usage["b"].cpu_percent, Some(0.0));
    assert_eq!(usage.len(), 2);
}

#[tokio::test(start_paused = true)]
async fn descendants_reaped_through_ended_parents_count_once() {
    let start = Instant::now();
    let first = sample(
        start,
        [
            process(10, 1, "a", 0),
            process(11, 10, "a", SECOND),
            process(12, 11, "a", 2 * SECOND),
        ],
    );
    // 12 used one more second before 11 reaped it, and 10 then reaped 11,
    // which had used one more of its own.
    let second = sample(
        start + Duration::from_secs(10),
        [reaping(10, 1, "a", 0, 5 * SECOND)],
    );
    assert_eq!(
        usage(Some(&first), &second, &mut HashMap::new())["a"].cpu_percent,
        Some(20.0)
    );
}

#[tokio::test(start_paused = true)]
async fn exited_children_count_once_their_parent_reaps_them() {
    let start = Instant::now();
    let first = sample(
        start,
        [process(10, 1, "a", 0), process(11, 10, "a", 2 * SECOND)],
    );
    // 11 used one more second and exited; 10 has not reaped it yet.
    let mut second = sample(
        start + Duration::from_secs(10),
        [process(10, 1, "a", 0), process(11, 10, "a", 3 * SECOND)],
    );
    second.unreaped.insert(process(11, 10, "a", 0).0);
    let exited = usage(Some(&first), &second, &mut HashMap::new());
    assert_eq!(exited["a"].cpu_percent, Some(10.0));
    assert_eq!(exited["a"].processes, 1);
    assert_eq!(exited["a"].memory_bytes, 1024);
    let third = sample(
        start + Duration::from_secs(20),
        [reaping(10, 1, "a", 0, 3 * SECOND)],
    );
    assert_eq!(
        usage(Some(&second), &third, &mut HashMap::new())["a"].cpu_percent,
        Some(0.0)
    );
}

#[test]
fn processes_exiting_after_the_scan_are_followed_unreaped() {
    let own = own_process();
    let candidates = vec![("a".to_owned(), own.clone())];
    let exited = read(candidates.clone(), Vec::new(), usage::read, |_| {
        Ok(State::Unreaped)
    })
    .unwrap();
    assert!(exited.processes.contains_key(&own));
    assert_eq!(exited.unreaped, HashSet::from([own.clone()]));
    let gone = read(candidates, Vec::new(), usage::read, |_| Ok(State::Gone)).unwrap();
    assert!(gone.processes.is_empty() && gone.unreaped.is_empty());
}

#[test]
fn processes_the_scan_misses_count_only_while_unreaped() {
    let own = own_process();
    let missed = || vec![("a".to_owned(), own.clone())];
    // Running without the identity, such as after an exec that dropped it.
    let running = read(Vec::new(), missed(), usage::read, |_| Ok(State::Running)).unwrap();
    assert!(running.processes.is_empty());
    let exited = read(Vec::new(), missed(), usage::read, |_| Ok(State::Unreaped)).unwrap();
    assert_eq!(exited.unreaped, HashSet::from([own.clone()]));
}

#[test]
fn every_process_is_read_before_any_is_kept() {
    let child = process(11, 10, "a", 0);
    let parent = process(10, 1, "a", 0);
    let calls = std::cell::RefCell::new(Vec::new());
    // The child was reaped after its reading, before the parent's.
    let sample = read(
        vec![child.clone(), parent.clone()]
            .into_iter()
            .map(|(identity, (workspace, _))| (workspace, identity))
            .collect(),
        Vec::new(),
        |pid| {
            calls.borrow_mut().push(format!("read {pid}"));
            Ok(Some(child.1.1))
        },
        |identity| {
            calls.borrow_mut().push(format!("state {}", identity.pid));
            Ok(if identity.pid == 11 {
                State::Gone
            } else {
                State::Running
            })
        },
    )
    .unwrap();
    assert_eq!(
        *calls.borrow(),
        ["read 11", "read 10", "state 11", "state 10"]
    );
    assert_eq!(sample.processes.keys().collect::<Vec<_>>(), [&parent.0]);
}

/// A process whose usage reads: this test's own.
fn own_process() -> Identity {
    Identity {
        pid: std::process::id(),
        birth: "own".into(),
        unique: None,
    }
}

#[tokio::test(start_paused = true)]
async fn time_a_reaper_shows_a_sample_late_counts_once() {
    let start = Instant::now();
    let mut pending = HashMap::new();
    let first = sample(
        start,
        [process(10, 1, "a", 0), process(11, 10, "a", 5 * SECOND)],
    );
    usage(None, &first, &mut pending);
    // 10 was read before it reaped 11, which used one more second and was
    // gone by its own read; 10 used one second itself.
    let second = sample(
        start + Duration::from_secs(10),
        [process(10, 1, "a", SECOND)],
    );
    assert_eq!(
        usage(Some(&first), &second, &mut pending)["a"].cpu_percent,
        Some(10.0)
    );
    assert_eq!(
        pending
            .values()
            .flatten()
            .map(|c| c.nanoseconds)
            .collect::<Vec<_>>(),
        [5 * SECOND]
    );
    // Now 10 shows 11's six seconds and one more of its own.
    let third = sample(
        start + Duration::from_secs(20),
        [reaping(10, 1, "a", 2 * SECOND, 6 * SECOND)],
    );
    assert_eq!(
        usage(Some(&second), &third, &mut pending)["a"].cpu_percent,
        Some(20.0)
    );
    assert!(pending.is_empty());
    // A gap in sampling forgets what was pending with the readings.
    let waiting = Counted {
        nanoseconds: SECOND,
        direct: true,
        carried: true,
    };
    let mut pending = HashMap::from([(process(10, 1, "a", 0).0, vec![waiting])]);
    usage(None, &third, &mut pending);
    assert!(pending.is_empty());
}

#[tokio::test(start_paused = true)]
async fn orphans_reaped_outside_the_workspace_are_not_subtracted_again() {
    let start = Instant::now();
    let mut pending = HashMap::new();
    let first = sample(
        start,
        [
            process(10, 1, "a", 0),
            process(11, 10, "a", SECOND),
            process(12, 11, "a", 5 * SECOND),
        ],
    );
    usage(None, &first, &mut pending);
    // 11 used one more second and exited, and 10 reaped it; its child 12
    // was orphaned to init, which reaped it.
    let second = sample(
        start + Duration::from_secs(10),
        [reaping(10, 1, "a", 0, 2 * SECOND)],
    );
    assert_eq!(
        usage(Some(&first), &second, &mut pending)["a"].cpu_percent,
        Some(10.0)
    );
    // What 10 never reaps is dropped after one sample rather than owed.
    let third = sample(
        start + Duration::from_secs(20),
        [reaping(10, 1, "a", SECOND, 2 * SECOND)],
    );
    assert_eq!(
        usage(Some(&second), &third, &mut pending)["a"].cpu_percent,
        Some(10.0)
    );
    assert!(pending.is_empty());
}

#[tokio::test(start_paused = true)]
async fn time_awaited_by_an_ended_parent_passes_to_its_reaper() {
    let start = Instant::now();
    let mut pending = HashMap::new();
    let first = sample(
        start,
        [
            process(20, 1, "a", 0),
            process(10, 20, "a", 0),
            process(11, 10, "a", 5 * SECOND),
        ],
    );
    usage(None, &first, &mut pending);
    // 10 was read before it reaped 11, which used one more second.
    let second = sample(
        start + Duration::from_secs(10),
        [process(20, 1, "a", 0), process(10, 20, "a", 0)],
    );
    assert_eq!(
        usage(Some(&first), &second, &mut pending)["a"].cpu_percent,
        Some(0.0)
    );
    // 10 reaped 11 and exited, and 20 reaped it.
    let third = sample(
        start + Duration::from_secs(20),
        [reaping(20, 1, "a", 0, 6 * SECOND)],
    );
    assert_eq!(
        usage(Some(&second), &third, &mut pending)["a"].cpu_percent,
        Some(10.0)
    );
    assert!(pending.is_empty());
}

#[test]
fn reaped_time_shows_direct_children_first_and_whole_descendants() {
    let counted = |nanoseconds, direct| Counted {
        nanoseconds,
        direct,
        carried: false,
    };
    // A deeper descendant too large for what is left was not reaped there.
    let (shown, waiting) = reconcile(2, vec![counted(5, false), counted(1, true)]);
    assert_eq!(shown, 1);
    assert_eq!(
        waiting,
        [Counted {
            carried: true,
            ..counted(5, false)
        }]
    );
    let (shown, waiting) = reconcile(6, vec![counted(5, false), counted(1, true)]);
    assert_eq!((shown, waiting), (6, Vec::new()));
    // A direct child waits for the rest of its time once.
    let (shown, waiting) = reconcile(1, vec![counted(3, true)]);
    assert_eq!(shown, 1);
    assert_eq!(
        waiting,
        [Counted {
            carried: true,
            ..counted(2, true)
        }]
    );
    let (shown, waiting) = reconcile(0, waiting);
    assert_eq!((shown, waiting), (0, Vec::new()));
}

#[tokio::test(start_paused = true)]
async fn processes_reaped_elsewhere_lower_no_usage() {
    let start = Instant::now();
    let first = sample(start, [process(10, 1, "a", 5 * SECOND)]);
    let second = sample(start + Duration::from_secs(10), [process(30, 1, "a", 0)]);
    assert_eq!(
        usage(Some(&first), &second, &mut HashMap::new())["a"].cpu_percent,
        Some(0.0)
    );
}

#[tokio::test]
async fn readings_cover_the_processes_carrying_a_workspace_identity() {
    let id = uuid::Uuid::new_v4().to_string();
    // The test binary's own helper, since platform binaries such as
    // macOS's sh hide their environment.
    let child = tokio::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "process::identity::tests::marked_child",
            "--ignored",
        ])
        .stdout(std::process::Stdio::null())
        .env(crate::env::WORKSPACE_ID, &id)
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let processes = identity::scan_workspaces(HashSet::from([id.clone()]))
        .await
        .unwrap();
    let sample = read(processes, Vec::new(), usage::read, state).unwrap();
    assert!(sample.unreaped.is_empty());
    let [(identity, (workspace, reading))] = Vec::from_iter(sample.processes).try_into().unwrap();
    assert_eq!(Some(identity.pid), child.id());
    assert_eq!(workspace, id);
    assert_eq!(reading.parent, std::process::id());
    assert!(reading.memory > 0);
}

#[tokio::test(start_paused = true)]
async fn failed_and_stalled_samples_report_nothing() {
    let start = Instant::now();
    let mut usages = WorkspaceUsages::default();
    usages.record(Ok(sample(start, [process(10, 1, "a", 0)])));
    assert_eq!(usages.get("a", start).unwrap().processes, 1);
    assert_eq!(usages.get("a", start + STALE + SAMPLE_INTERVAL), None);
    let late = start + STALE + SAMPLE_INTERVAL;
    usages.record(Ok(sample(late, [process(10, 1, "a", SECOND)])));
    assert_eq!(usages.get("a", late).unwrap().cpu_percent, None);
    usages.record(Err(anyhow::anyhow!("unavailable")));
    assert_eq!(usages.get("a", late), None);
}
