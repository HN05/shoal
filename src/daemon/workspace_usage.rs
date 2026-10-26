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
    /// Counted CPU nanoseconds of ended processes, per live ancestor that
    /// may reap them, that its reaped time does not show yet: a parent read
    /// just before it reaped a child gone by its own read shows the child's
    /// time one sample later. Kept for that sample only, since a process
    /// orphaned to init never reaches the ancestor.
    pending: HashMap<Identity, u64>,
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

/// Each workspace's processes and memory in `current`, with the CPU they
/// used since `previous`. A process that started since counts all its time,
/// and one that exited counts until its parent reaps it. One that is gone
/// since counted its time already, which its nearest ancestor still in its
/// workspace, reached through ancestors gone too, may have reaped: as much of
/// it as that ancestor's reaped time grew by is not counted again, and the
/// rest stays `pending` for one sample.
fn usage(
    previous: Option<&Sample>,
    current: &Sample,
    pending: &mut HashMap<Identity, u64>,
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
    let routed = routed_to_reapers(previous, current);
    let mut still_pending = HashMap::new();
    for (identity, (workspace, reading)) in &current.processes {
        let carried = pending.get(identity).copied().unwrap_or_default();
        let routed = routed.get(identity).copied().unwrap_or_default();
        if carried == 0 && routed == 0 {
            continue;
        }
        let before = previous
            .processes
            .get(identity)
            .map_or(0, |(_, before)| before.reaped);
        let shown = reading.reaped.saturating_sub(before);
        let from_carried = carried.min(shown);
        let from_routed = routed.min(shown - from_carried);
        if let Some(used) = used.get_mut(workspace.as_str()) {
            *used = used.saturating_sub(from_carried + from_routed);
        }
        if routed > from_routed {
            still_pending.insert(identity.clone(), routed - from_routed);
        }
    }
    *pending = still_pending;
    let elapsed = current.time.duration_since(previous.time).as_nanos() as f64;
    for (workspace, entry) in &mut usage {
        let used = used.get(workspace.as_str()).copied().unwrap_or_default();
        entry.cpu_percent = (elapsed > 0.0).then(|| used as f64 * 100.0 / elapsed);
    }
    usage
}

/// The counted time of the processes gone since `previous`, summed per live
/// process in `current` that may have reaped them: the nearest ancestor in
/// the same workspace, reached through ancestors gone too.
fn routed_to_reapers<'a>(previous: &Sample, current: &'a Sample) -> HashMap<&'a Identity, u64> {
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
    let mut routed: HashMap<&Identity, u64> = HashMap::new();
    for (identity, (workspace, reading)) in &previous.processes {
        if ended.contains_key(&(workspace.as_str(), identity.pid))
            && let Some(reaper) = reaper(&live, &ended, workspace, reading.parent)
        {
            let total = routed.entry(reaper).or_default();
            *total = total.saturating_add(reading.total());
        }
    }
    routed
}

/// The `live` ancestor in `workspace` of an ended process whose parent was
/// `parent`, reached through `ended` ancestors and their parents.
fn reaper<'a>(
    live: &HashMap<(&str, u32), &'a Identity>,
    ended: &HashMap<(&str, u32), u32>,
    workspace: &str,
    mut parent: u32,
) -> Option<&'a Identity> {
    // Each step reaches another ended process, unless a reused PID makes a cycle.
    for _ in 0..=ended.len() {
        if let Some(identity) = live.get(&(workspace, parent)) {
            return Some(identity);
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
    tokio::task::spawn_blocking(move || read(running, missed, state))
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
/// each kept by the `state` its process is in once read: one that exits after
/// the scan is still followed until reaped. A missed process counts only
/// while unreaped, since one still running no longer carries the identity.
fn read(
    found: Vec<(String, Identity)>,
    missed: Vec<(String, Identity)>,
    state: impl Fn(&Identity) -> Result<State>,
) -> Result<Sample> {
    let mut processes = HashMap::new();
    let mut unreaped = HashSet::new();
    let candidates = found
        .into_iter()
        .map(|candidate| (candidate, true))
        .chain(missed.into_iter().map(|candidate| (candidate, false)));
    for ((workspace, identity), scanned) in candidates {
        let Some(reading) = usage::read(identity.pid)? else {
            continue;
        };
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
        let exited = read(candidates.clone(), Vec::new(), |_| Ok(State::Unreaped)).unwrap();
        assert!(exited.processes.contains_key(&own));
        assert_eq!(exited.unreaped, HashSet::from([own.clone()]));
        let gone = read(candidates, Vec::new(), |_| Ok(State::Gone)).unwrap();
        assert!(gone.processes.is_empty() && gone.unreaped.is_empty());
    }

    #[test]
    fn processes_the_scan_misses_count_only_while_unreaped() {
        let own = own_process();
        let missed = || vec![("a".to_owned(), own.clone())];
        // Running without the identity, such as after an exec that dropped it.
        let running = read(Vec::new(), missed(), |_| Ok(State::Running)).unwrap();
        assert!(running.processes.is_empty());
        let exited = read(Vec::new(), missed(), |_| Ok(State::Unreaped)).unwrap();
        assert_eq!(exited.unreaped, HashSet::from([own.clone()]));
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
        assert_eq!(pending.values().copied().collect::<Vec<_>>(), [5 * SECOND]);
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
        let mut pending = HashMap::from([(process(10, 1, "a", 0).0, SECOND)]);
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
            Some(0.0)
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
        use tokio::io::AsyncBufReadExt;
        let id = uuid::Uuid::new_v4().to_string();
        let mut child = tokio::process::Command::new("sh")
            .args(["-c", "echo ready; exec sleep 600"])
            .env(crate::env::WORKSPACE_ID, &id)
            .stdout(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        // The line proves the shell runs with its own environment.
        let mut line = String::new();
        tokio::io::BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut line)
            .await
            .unwrap();
        let processes = identity::scan_workspaces(HashSet::from([id.clone()]))
            .await
            .unwrap();
        let sample = read(processes, Vec::new(), state).unwrap();
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
