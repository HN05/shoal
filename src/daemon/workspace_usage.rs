//! What each workspace's processes use, sampled on a fixed cadence from the
//! processes carrying its identity.
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::Duration,
};

use anyhow::{Context, Result};
use tokio::time::Instant;

use crate::{
    daemon::{log, workspace::Manager},
    model::WorkspaceUsage,
    process::{
        identity::{self, Identity},
        usage::{self, Reading},
    },
};

const SAMPLE_INTERVAL: Duration = Duration::from_secs(10);
/// How late a sample may be before the sampler counts as stalled.
const STALE: Duration = Duration::from_secs(30);

/// Every workspace process with its workspace and reading at one time.
struct Sample {
    time: Instant,
    processes: HashMap<Identity, (String, Reading)>,
    /// Processes that exited but whose parent has not reaped them, so their
    /// time has not reached it yet. They use no memory and are not counted.
    unreaped: HashSet<Identity>,
}

#[derive(Default)]
pub(crate) struct WorkspaceUsages {
    latest: Option<Sample>,
    usage: HashMap<String, WorkspaceUsage>,
    /// Counted time of ended processes, per live ancestor that may reap
    /// them, that its reaped time does not show yet: a parent read just
    /// before it reaped a child gone by its own read shows the child's time
    /// one sample later.
    pending: HashMap<Identity, Vec<Counted>>,
}

impl WorkspaceUsages {
    /// Add a sample. A failed or late one forgets the previous sample, so no
    /// CPU use spans a gap in what was measured.
    fn record(&mut self, sample: Result<Sample>) {
        match sample {
            Ok(sample) => {
                let previous = self
                    .latest
                    .take()
                    .filter(|previous| sample.time.duration_since(previous.time) <= STALE);
                self.usage = usage(previous.as_ref(), &sample, &mut self.pending);
                self.latest = Some(sample);
            }
            Err(error) => {
                // Once per gap: a platform without readings would repeat it forever.
                if self.latest.take().is_some() {
                    log!("workspace usage: {error:#}");
                }
                self.usage.clear();
            }
        }
    }

    /// The processes of the latest sample, which the next one follows until
    /// their parents reap them.
    fn tracked(&self) -> Vec<(String, Identity)> {
        self.latest.as_ref().map_or_else(Vec::new, |latest| {
            latest
                .processes
                .iter()
                .map(|(identity, (workspace, _))| (workspace.clone(), identity.clone()))
                .collect()
        })
    }

    /// The latest usage of the workspace `id`, while it has processes and
    /// sampling has not stalled.
    pub(crate) fn get(&self, id: &str, now: Instant) -> Option<WorkspaceUsage> {
        let latest = self.latest.as_ref()?;
        if now.duration_since(latest.time) > STALE {
            return None;
        }
        self.usage.get(id).cloned()
    }
}

/// CPU nanoseconds an ended process had counted, waiting to be found in
/// the reaped time of an ancestor.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Counted {
    nanoseconds: u64,
    /// Its parent is the ancestor, which therefore reaps it. A process that
    /// reaches the ancestor only through parents that ended too may have
    /// been orphaned to init instead.
    direct: bool,
    /// It waited a sample already; a reaping that late is not expected.
    carried: bool,
}

/// Each workspace's processes and memory in `current`, with the CPU they
/// used since `previous`. A process that started since counts all its time,
/// and one that exited counts until its parent reaps it. One that is gone
/// since counted its time already, which its nearest ancestor still in its
/// workspace may have reaped: what that ancestor's reaped time shows of it
/// is not counted again, and what it does not show yet stays `pending` for
/// one sample.
fn usage(
    previous: Option<&Sample>,
    current: &Sample,
    pending: &mut HashMap<Identity, Vec<Counted>>,
) -> HashMap<String, WorkspaceUsage> {
    let mut usage: HashMap<String, WorkspaceUsage> = HashMap::new();
    let mut used: HashMap<&str, u64> = HashMap::new();
    for (identity, (workspace, reading)) in &current.processes {
        let entry = usage.entry(workspace.clone()).or_default();
        if !current.unreaped.contains(identity) {
            entry.processes += 1;
            entry.memory_bytes = entry.memory_bytes.saturating_add(reading.memory);
        }
        let before = previous
            .and_then(|previous| previous.processes.get(identity))
            .map_or(0, |(_, before)| before.total());
        *used.entry(workspace).or_default() += reading.total().saturating_sub(before);
    }
    let Some(previous) = previous else {
        pending.clear();
        return usage;
    };
    let mut owed = routed_to_reapers(previous, current, pending);
    // What waited for a live process is carried; an ended one passed it on.
    for (identity, counted) in pending.drain() {
        if current.processes.contains_key(&identity) {
            owed.entry(identity).or_default().extend(counted);
        }
    }
    for (identity, counted) in owed {
        let Some((workspace, reading)) = current.processes.get(&identity) else {
            continue;
        };
        let before = previous
            .processes
            .get(&identity)
            .map_or(0, |(_, before)| before.reaped);
        let (shown, waiting) = reconcile(reading.reaped.saturating_sub(before), counted);
        if let Some(used) = used.get_mut(workspace.as_str()) {
            *used = used.saturating_sub(shown);
        }
        if !waiting.is_empty() {
            pending.insert(identity, waiting);
        }
    }
    let elapsed = current.time.duration_since(previous.time).as_nanos() as f64;
    for (workspace, entry) in &mut usage {
        let used = used.get(workspace.as_str()).copied().unwrap_or_default();
        entry.cpu_percent = (elapsed > 0.0).then(|| used as f64 * 100.0 / elapsed);
    }
    usage
}

/// How much of `counted` a reaper's reaped time, grown by `reaped`, shows,
/// and what may still show next sample. A direct child is certainly in it,
/// as far as it reaches; a deeper descendant only when it fits whole in
/// what the direct children leave, since otherwise its ended parent did not
/// reap it.
fn reconcile(reaped: u64, mut counted: Vec<Counted>) -> (u64, Vec<Counted>) {
    counted.sort_by_key(|counted| !counted.direct);
    let mut left = reaped;
    let mut waiting = Vec::new();
    for counted in counted {
        let shown = if counted.direct || counted.nanoseconds <= left {
            counted.nanoseconds.min(left)
        } else {
            0
        };
        left -= shown;
        if shown < counted.nanoseconds && !counted.carried {
            waiting.push(Counted {
                nanoseconds: counted.nanoseconds - shown,
                carried: true,
                ..counted
            });
        }
    }
    (reaped - left, waiting)
}

/// What each process gone since `previous` had counted, with what was
/// `pending` for it, per live process in `current` that may have reaped
/// it: its nearest ancestor in the same workspace, reached through
/// ancestors gone too.
fn routed_to_reapers(
    previous: &Sample,
    current: &Sample,
    pending: &HashMap<Identity, Vec<Counted>>,
) -> HashMap<Identity, Vec<Counted>> {
    let live: HashMap<(&str, u32), &Identity> = current
        .processes
        .iter()
        .map(|(identity, (workspace, _))| ((workspace.as_str(), identity.pid), identity))
        .collect();
    let ended: HashMap<(&str, u32), u32> = previous
        .processes
        .iter()
        .filter(|(identity, _)| !current.processes.contains_key(identity))
        .map(|(identity, (workspace, reading))| {
            ((workspace.as_str(), identity.pid), reading.parent)
        })
        .collect();
    let mut routed: HashMap<Identity, Vec<Counted>> = HashMap::new();
    for (identity, (workspace, reading)) in &previous.processes {
        if !ended.contains_key(&(workspace.as_str(), identity.pid)) {
            continue;
        }
        let Some((reaper, direct)) = reaper(&live, &ended, workspace, reading.parent) else {
            continue;
        };
        let counted = routed.entry(reaper.clone()).or_default();
        counted.push(Counted {
            nanoseconds: reading.total(),
            direct,
            carried: false,
        });
        // What it awaited from its own children reaches the reaper through it.
        counted.extend(
            pending
                .get(identity)
                .into_iter()
                .flatten()
                .map(|awaited| Counted {
                    direct: false,
                    ..*awaited
                }),
        );
    }
    routed
}

/// The `live` ancestor in `workspace` of an ended process whose parent was
/// `parent`, reached through `ended` ancestors and their parents, and
/// whether it is the parent itself.
fn reaper<'a>(
    live: &HashMap<(&str, u32), &'a Identity>,
    ended: &HashMap<(&str, u32), u32>,
    workspace: &str,
    mut parent: u32,
) -> Option<(&'a Identity, bool)> {
    // Each step reaches another ended process, unless a reused PID makes a cycle.
    for step in 0..=ended.len() {
        if let Some(identity) = live.get(&(workspace, parent)) {
            return Some((identity, step == 0));
        }
        parent = *ended.get(&(workspace, parent))?;
    }
    None
}

pub(super) async fn run(manager: Arc<Manager>) {
    loop {
        let tracked = manager.workspace_usage.lock().unwrap().tracked();
        let sample = sample(&manager, tracked).await;
        manager.workspace_usage.lock().unwrap().record(sample);
        tokio::time::sleep(SAMPLE_INTERVAL).await;
    }
}

/// The workspaces' processes, and those of `tracked` that have exited
/// unreaped, whose environment no scan can read.
async fn sample(manager: &Manager, tracked: Vec<(String, Identity)>) -> Result<Sample> {
    let ids = manager.workspace_ids().await?;
    let running = identity::scan_workspaces(ids).await?;
    let found: HashSet<&Identity> = running.iter().map(|(_, identity)| identity).collect();
    let missed: Vec<_> = tracked
        .into_iter()
        .filter(|(_, identity)| !found.contains(identity))
        .collect();
    tokio::task::spawn_blocking(move || read(running, missed, usage::read, state))
        .await
        .context("workspace usage worker failed")?
}

enum State {
    Running,
    Unreaped,
    Gone,
}

fn state(identity: &Identity) -> Result<State> {
    Ok(if identity::alive(identity)? {
        State::Running
    } else if identity::unreaped(identity)? {
        State::Unreaped
    } else {
        State::Gone
    })
}

/// The readings of the processes the scan `found` and of those it `missed`,
/// each kept by the `state` its process is in once every reading is taken:
/// a child reaped after its own reading may show in its parent's, so it
/// counts as gone and its time as its reaper's. One that exits after the
/// scan is still followed until reaped, but a missed process counts only
/// while unreaped, since one still running no longer carries the identity.
fn read(
    found: Vec<(String, Identity)>,
    missed: Vec<(String, Identity)>,
    reading: impl Fn(u32) -> Result<Option<Reading>>,
    state: impl Fn(&Identity) -> Result<State>,
) -> Result<Sample> {
    let candidates = found
        .into_iter()
        .map(|candidate| (candidate, true))
        .chain(missed.into_iter().map(|candidate| (candidate, false)));
    let mut readings = Vec::new();
    for ((workspace, identity), scanned) in candidates {
        if let Some(reading) = reading(identity.pid)? {
            readings.push((workspace, identity, scanned, reading));
        }
    }
    let mut processes = HashMap::new();
    let mut unreaped = HashSet::new();
    for (workspace, identity, scanned, reading) in readings {
        match state(&identity)? {
            State::Running if scanned => {}
            State::Unreaped => {
                unreaped.insert(identity.clone());
            }
            State::Running | State::Gone => continue,
        }
        processes.insert(identity, (workspace, reading));
    }
    Ok(Sample {
        time: Instant::now(),
        processes,
        unreaped,
    })
}

#[cfg(test)]
mod tests {
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
        let [(identity, (workspace, reading))] =
            Vec::from_iter(sample.processes).try_into().unwrap();
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
}
