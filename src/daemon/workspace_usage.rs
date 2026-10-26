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
                self.usage = usage(previous.as_ref(), &sample);
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
/// and one that exited counts until its parent reaps it.
/// One that ended since was reaped into the time of its nearest ancestor
/// still in its workspace, directly or through ancestors that ended too, and
/// that workspace already counted the time it had before.
fn usage(previous: Option<&Sample>, current: &Sample) -> HashMap<String, WorkspaceUsage> {
    let mut usage: HashMap<String, WorkspaceUsage> = HashMap::new();
    let mut cpu: HashMap<&str, i128> = HashMap::new();
    for (identity, (workspace, reading)) in &current.processes {
        let entry = usage.entry(workspace.clone()).or_default();
        if !current.unreaped.contains(identity) {
            entry.processes += 1;
            entry.memory_bytes = entry.memory_bytes.saturating_add(reading.memory);
        }
        let before = previous
            .and_then(|previous| previous.processes.get(identity))
            .map_or(0, |(_, before)| before.cpu);
        *cpu.entry(workspace).or_default() += i128::from(reading.cpu) - i128::from(before);
    }
    let Some(previous) = previous else {
        return usage;
    };
    let live: HashSet<(&str, u32)> = current
        .processes
        .iter()
        .map(|(identity, (workspace, _))| (workspace.as_str(), identity.pid))
        .collect();
    let ended: HashMap<(&str, u32), u32> = previous
        .processes
        .iter()
        .filter(|(identity, _)| !current.processes.contains_key(identity))
        .map(|(identity, (workspace, reading))| {
            ((workspace.as_str(), identity.pid), reading.parent)
        })
        .collect();
    for (identity, (workspace, reading)) in &previous.processes {
        if ended.contains_key(&(workspace.as_str(), identity.pid))
            && reaped_within(&live, &ended, workspace, reading.parent)
            && let Some(used) = cpu.get_mut(workspace.as_str())
        {
            *used -= i128::from(reading.cpu);
        }
    }
    let elapsed = current.time.duration_since(previous.time).as_nanos() as f64;
    for (workspace, entry) in &mut usage {
        let used = cpu
            .get(workspace.as_str())
            .copied()
            .unwrap_or_default()
            .max(0);
        entry.cpu_percent = (elapsed > 0.0).then(|| used as f64 * 100.0 / elapsed);
    }
    usage
}

/// Whether an ended process of `workspace` whose parent was `parent` was
/// reaped within the workspace: an ancestor of it there, reached through
/// `ended` ancestors and their parents, is `live`.
fn reaped_within(
    live: &HashSet<(&str, u32)>,
    ended: &HashMap<(&str, u32), u32>,
    workspace: &str,
    mut parent: u32,
) -> bool {
    // Each step reaches another ended process, unless a reused PID makes a cycle.
    for _ in 0..=ended.len() {
        if live.contains(&(workspace, parent)) {
            return true;
        }
        match ended.get(&(workspace, parent)) {
            Some(grandparent) => parent = *grandparent,
            None => return false,
        }
    }
    false
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
    let exited: Vec<_> = tracked
        .into_iter()
        .filter(|(_, identity)| !found.contains(identity))
        .collect();
    tokio::task::spawn_blocking(move || {
        let mut processes = read(running, identity::alive)?;
        let unreaped = read(exited, identity::unreaped)?;
        let unreaped_identities = unreaped.keys().cloned().collect();
        processes.extend(unreaped);
        Ok(Sample {
            time: Instant::now(),
            processes,
            unreaped: unreaped_identities,
        })
    })
    .await
    .context("workspace usage worker failed")?
}

/// Each process's reading, leaving out those `still` no longer holds for
/// once read, such as ones that ended since the scan.
fn read(
    processes: Vec<(String, Identity)>,
    still: impl Fn(&Identity) -> Result<bool>,
) -> Result<HashMap<Identity, (String, Reading)>> {
    let mut readings = HashMap::new();
    for (workspace, identity) in processes {
        if let Some(reading) = usage::read(identity.pid)?
            && still(&identity)?
        {
            readings.insert(identity, (workspace, reading));
        }
    }
    Ok(readings)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECOND: u64 = 1_000_000_000;

    fn process(pid: u32, parent: u32, workspace: &str, cpu: u64) -> (Identity, (String, Reading)) {
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
        let alone = usage(None, &first);
        assert_eq!(alone["a"].processes, 2);
        assert_eq!(alone["a"].memory_bytes, 2048);
        assert_eq!(alone["a"].cpu_percent, None);
        // Over ten seconds, 11 used one more second before 10 reaped it,
        // 12 started and used two, and 10 used one itself.
        let second = sample(
            start + Duration::from_secs(10),
            [
                process(10, 1, "a", 9 * SECOND),
                process(12, 10, "a", 2 * SECOND),
                process(20, 1, "b", SECOND),
            ],
        );
        let usage = usage(Some(&first), &second);
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
            [process(10, 1, "a", 5 * SECOND)],
        );
        assert_eq!(usage(Some(&first), &second)["a"].cpu_percent, Some(20.0));
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
        let exited = usage(Some(&first), &second);
        assert_eq!(exited["a"].cpu_percent, Some(10.0));
        assert_eq!(exited["a"].processes, 1);
        assert_eq!(exited["a"].memory_bytes, 1024);
        let third = sample(
            start + Duration::from_secs(20),
            [process(10, 1, "a", 3 * SECOND)],
        );
        assert_eq!(usage(Some(&second), &third)["a"].cpu_percent, Some(0.0));
    }

    #[tokio::test(start_paused = true)]
    async fn processes_reaped_elsewhere_lower_no_usage() {
        let start = Instant::now();
        let first = sample(start, [process(10, 1, "a", 5 * SECOND)]);
        let second = sample(start + Duration::from_secs(10), [process(30, 1, "a", 0)]);
        assert_eq!(usage(Some(&first), &second)["a"].cpu_percent, Some(0.0));
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
        let readings = read(processes, identity::alive).unwrap();
        let [(identity, (workspace, reading))] = Vec::from_iter(readings).try_into().unwrap();
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
