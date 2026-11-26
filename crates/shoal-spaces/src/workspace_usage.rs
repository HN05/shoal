//! What each space's processes and containers use, sampled on a fixed
//! cadence, with what of it its commands hold, and the memory of one-shot
//! agent runs, which belong to no space.
use shoal_ids::space_ids::SpaceId;
use std::{
    collections::{HashMap, HashSet},
    time::Duration,
};

use anyhow::{Context, Result};
use shoal_daemon_base::containers::ContainerUsage;
use tokio::time::Instant;

use crate::{
    agent_peaks::AgentPeaks,
    log,
    model::WorkspaceUsage,
    process::{
        identity::{self, Identity},
        load::SampledWork,
        usage::{self, Reading},
    },
};

pub const SAMPLE_INTERVAL: Duration = Duration::from_secs(10);
/// How late a sample may be before the sampler counts as stalled.
pub const STALE: Duration = Duration::from_secs(30);
/// How long a sample waits for the containers' reading. A sample's time is
/// its processes' reading, taken before that wait, so it is recorded up to
/// one wait later, and the next one up to an interval and a wait after that.
pub const CONTAINER_READ_LIMIT: Duration = Duration::from_secs(8);
const _: () = assert!(
    2 * CONTAINER_READ_LIMIT.as_secs() + SAMPLE_INTERVAL.as_secs() < STALE.as_secs(),
    "a runtime that always times out would let every sample go stale"
);

/// The usage of each workspace whose project may run, or why its runtime
/// could not say.
pub type ContainerReadings = Result<Vec<(SpaceId, Result<ContainerUsage>)>>;

pub struct Sample {
    pub time: Instant,
    pub processes: HashMap<Identity, (SpaceId, Reading)>,
    /// Processes that exited but whose parent has not reaped them, so their
    /// time has not reached it yet. They use no memory and are not counted.
    pub unreaped: HashSet<Identity>,
    /// Processes in the work slice, whose memory the slice reads.
    pub in_work_slice: HashSet<Identity>,
    /// One-shot agent runs, such as reviewers, belong to no space.
    pub oneshots: Oneshots,
    /// The PIDs of the commands Shoal runs in spaces besides their agents,
    /// such as `shoal run` commands and demos, whose processes and what
    /// they start count as the commands' memory.
    pub commands: HashSet<u32>,
}

/// In bytes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Oneshots {
    pub memory: u64,
    pub in_work_slice: u64,
}

impl Oneshots {
    /// A process gone meanwhile counts no memory.
    pub fn read(
        found: &[Identity],
        reading: impl Fn(u32) -> Result<Option<Reading>>,
        in_slice: impl Fn(u32) -> bool,
    ) -> Result<Self> {
        let mut oneshots = Self::default();
        for identity in found {
            let Some(reading) = reading(identity.pid)? else {
                continue;
            };
            oneshots.memory = oneshots.memory.saturating_add(reading.memory);
            if in_slice(identity.pid) {
                oneshots.in_work_slice = oneshots.in_work_slice.saturating_add(reading.memory);
            }
        }
        Ok(oneshots)
    }
}

impl Sample {
    /// `holds` tells the work slice's processes by PID.
    pub fn mark_work_slice(&mut self, holds: impl Fn(u32) -> bool) {
        self.in_work_slice = (self.processes.keys())
            .filter(|identity| holds(identity.pid))
            .cloned()
            .collect();
    }

    fn in_work_slice_memory(&self) -> u64 {
        (self.in_work_slice.iter())
            .filter(|identity| !self.unreaped.contains(*identity))
            .filter_map(|identity| self.processes.get(identity))
            .fold(0, |total, (_, reading)| {
                total.saturating_add(reading.memory)
            })
    }
}

#[derive(Default)]
pub struct WorkspaceUsages {
    latest: Option<Sample>,
    usage: HashMap<SpaceId, WorkspaceUsage>,
    /// Counted time of ended processes that their reaper's reaped time does
    /// not show yet, per live ancestor that may reap them. A parent read just
    /// before it reaped a child gone by its own read shows the child's time
    /// one sample later.
    pending: HashMap<Identity, Vec<Counted>>,
    /// The workspaces whose containers could not be read last time. Each
    /// failure is logged once until a reading succeeds.
    unread_containers: HashSet<SpaceId>,
    /// No container could be read last time. This is logged once too.
    containers_unreadable: bool,
    /// The latest sample read every project's containers it tried, so its
    /// memory holds all of theirs.
    containers_complete: bool,
    /// How far each space's agent rose during its latest tool call.
    peaks: AgentPeaks,
}

impl WorkspaceUsages {
    /// A failed or late sample forgets the previous one, so no CPU use spans
    /// a gap in what was measured. Returns why the containers could not all
    /// be read.
    pub fn record(
        &mut self,
        sample: Result<Sample>,
        containers: ContainerReadings,
    ) -> Option<String> {
        match sample {
            Ok(sample) => {
                let previous = self
                    .latest
                    .take()
                    .filter(|previous| sample.time.duration_since(previous.time) <= STALE);
                self.usage = usage(previous.as_ref(), &sample, &mut self.pending);
                let (containers, missed) = self.read_containers(containers);
                add_containers(&mut self.usage, containers, previous.is_some());
                let agents = self.usage.iter();
                (self.peaks)
                    .observe(agents.map(|(id, usage)| (id.clone(), usage.agent_memory_bytes())));
                self.latest = Some(sample);
                (!self.containers_complete).then(|| missed.join("; "))
            }
            Err(error) => {
                // Log once per gap: a platform without readings would repeat
                // it forever.
                if self.latest.take().is_some() {
                    log!("workspace usage: {error:#}");
                }
                self.usage.clear();
                None
            }
        }
    }

    /// Logs each workspace's failure once. A runtime that cannot be read
    /// leaves its workspaces' usage as their processes'. Also returns why
    /// each reading failed and what clears it.
    fn read_containers(
        &mut self,
        readings: ContainerReadings,
    ) -> (Vec<(SpaceId, ContainerUsage)>, Vec<String>) {
        self.containers_complete = false;
        let readings = match readings {
            Ok(readings) => readings,
            Err(error) => {
                if !std::mem::replace(&mut self.containers_unreadable, true) {
                    log!("container usage: {error:#}");
                }
                return (
                    Vec::new(),
                    vec![format!("the container runtime ({error:#}); start it")],
                );
            }
        };
        self.containers_unreadable = false;
        self.containers_complete = readings.iter().all(|(_, reading)| reading.is_ok());
        let (mut read, mut missed) = (Vec::new(), Vec::new());
        for (workspace, reading) in readings {
            match reading {
                Ok(usage) => {
                    self.unread_containers.remove(&workspace);
                    read.push((workspace, usage));
                }
                Err(error) => {
                    if self.unread_containers.insert(workspace.clone()) {
                        log!("container usage of {workspace}: {error:#}");
                    }
                    missed.push(format!(
                        "the containers of workspace {workspace} ({error:#}); stop them with shoal release containers --space {workspace}"
                    ));
                }
            }
        }
        (read, missed)
    }

    /// The next sample follows these processes until their parents reap
    /// them.
    pub fn tracked(&self) -> Vec<(SpaceId, Identity)> {
        self.latest.as_ref().map_or_else(Vec::new, |latest| {
            latest
                .processes
                .iter()
                .map(|(identity, (workspace, _))| (workspace.clone(), identity.clone()))
                .collect()
        })
    }

    /// Includes what ran outside the work slice. None once sampling stalled,
    /// and when a container's reading failed, since the sample then misses
    /// what those containers hold.
    pub fn memory(&self, now: Instant) -> Option<SampledWork> {
        let latest = self.latest.as_ref()?;
        if now.duration_since(latest.time) > STALE || !self.containers_complete {
            return None;
        }
        let spaces = (self.usage.values()).fold(0u64, |total, usage| {
            total.saturating_add(usage.memory_bytes)
        });
        let all = spaces.saturating_add(latest.oneshots.memory);
        let in_slice =
            (latest.in_work_slice_memory()).saturating_add(latest.oneshots.in_work_slice);
        Some(SampledWork {
            all,
            outside_slice: all.saturating_sub(in_slice),
        })
    }

    /// See [`AgentPeaks::rise_after_call`].
    pub fn rise_after_call(&mut self, space: &SpaceId, now: Instant) -> Option<u64> {
        self.peaks.rise_after_call(space, now)
    }

    /// None while the workspace has no processes or containers, or once
    /// sampling stalled.
    pub fn get(&self, id: &SpaceId, now: Instant) -> Option<WorkspaceUsage> {
        let latest = self.latest.as_ref()?;
        if now.duration_since(latest.time) > STALE {
            return None;
        }
        self.usage.get(id).cloned()
    }
}

/// CPU nanoseconds an ended process had counted, waiting to be found in the
/// reaped time of an ancestor.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Counted {
    pub nanoseconds: u64,
    /// Its parent is the ancestor, which therefore reaps it. A process that
    /// reaches the ancestor only through parents that ended too may have
    /// been orphaned to init instead.
    pub direct: bool,
    /// It waited a sample already: a reaping that late is not expected.
    pub carried: bool,
}

/// A process that started since `previous` counts all its time, and one
/// that exited counts until its parent reaps it. A process gone since
/// counted its time already, and its nearest ancestor still in its
/// workspace may have reaped it: what that ancestor's reaped time shows of
/// it is not counted again, and what it does not show yet stays `pending`
/// for one sample.
pub fn usage(
    previous: Option<&Sample>,
    current: &Sample,
    pending: &mut HashMap<Identity, Vec<Counted>>,
) -> HashMap<SpaceId, WorkspaceUsage> {
    let mut usage: HashMap<SpaceId, WorkspaceUsage> = HashMap::new();
    let mut used: HashMap<&SpaceId, u64> = HashMap::new();
    let commands = command_processes(current);
    for (identity, (workspace, reading)) in &current.processes {
        let entry = usage.entry(workspace.clone()).or_default();
        if !current.unreaped.contains(identity) {
            entry.processes += 1;
            entry.memory_bytes = entry.memory_bytes.saturating_add(reading.memory);
            if commands.contains(&identity.pid) {
                entry.command_memory_bytes =
                    entry.command_memory_bytes.saturating_add(reading.memory);
            }
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
        if let Some(used) = used.get_mut(workspace) {
            *used = used.saturating_sub(shown);
        }
        if !waiting.is_empty() {
            pending.insert(identity, waiting);
        }
    }
    let elapsed = current.time.duration_since(previous.time).as_nanos() as f64;
    for (workspace, entry) in &mut usage {
        let used = used.get(workspace).copied().unwrap_or_default();
        entry.cpu_percent = (elapsed > 0.0).then(|| used as f64 * 100.0 / elapsed);
    }
    usage
}

/// The PIDs of `current`'s commands and of the processes below them in
/// their space.
fn command_processes(current: &Sample) -> HashSet<u32> {
    let parents: HashMap<u32, (&SpaceId, u32)> = (current.processes.iter())
        .map(|(identity, (space, reading))| (identity.pid, (space, reading.parent)))
        .collect();
    let mut below = HashSet::new();
    for (&pid, &(space, _)) in &parents {
        // Each step reaches another process, unless a reused PID makes a cycle.
        let mut at = pid;
        for _ in 0..=parents.len() {
            let Some(&(_, parent)) = parents.get(&at).filter(|(owner, _)| *owner == space) else {
                break;
            };
            if current.commands.contains(&at) {
                below.insert(pid);
                break;
            }
            at = parent;
        }
    }
    below
}

/// The containers' CPU counts only once the processes' is `measured` over
/// two samples.
fn add_containers(
    usage: &mut HashMap<SpaceId, WorkspaceUsage>,
    containers: Vec<(SpaceId, ContainerUsage)>,
    measured: bool,
) {
    for (workspace, containers) in containers {
        let entry = usage.entry(workspace).or_default();
        entry.memory_bytes = entry.memory_bytes.saturating_add(containers.memory_bytes);
        if measured {
            entry.cpu_percent = Some(entry.cpu_percent.unwrap_or(0.0) + containers.cpu_percent);
        }
    }
}

/// How much of `counted` a reaper's reaped time, grown by `reaped`, shows,
/// and what may still show next sample. A direct child is certainly in it,
/// as far as it reaches. A deeper descendant is in it only when it fits
/// whole in what the direct children leave; otherwise its ended parent did
/// not reap it.
pub fn reconcile(reaped: u64, mut counted: Vec<Counted>) -> (u64, Vec<Counted>) {
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

/// Routes what each gone process had counted, with what was `pending` for
/// it, to the live process that may have reaped it: its nearest ancestor in
/// the same workspace, reached through ancestors gone too.
fn routed_to_reapers(
    previous: &Sample,
    current: &Sample,
    pending: &HashMap<Identity, Vec<Counted>>,
) -> HashMap<Identity, Vec<Counted>> {
    let live: HashMap<(&SpaceId, u32), &Identity> = current
        .processes
        .iter()
        .map(|(identity, (workspace, _))| ((workspace, identity.pid), identity))
        .collect();
    let ended: HashMap<(&SpaceId, u32), u32> = previous
        .processes
        .iter()
        .filter(|(identity, _)| !current.processes.contains_key(identity))
        .map(|(identity, (workspace, reading))| ((workspace, identity.pid), reading.parent))
        .collect();
    let mut routed: HashMap<Identity, Vec<Counted>> = HashMap::new();
    for (identity, (workspace, reading)) in &previous.processes {
        if !ended.contains_key(&(workspace, identity.pid)) {
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
        // What it awaited from its own children reaches the reaper through
        // it.
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

/// Reaches the live ancestor through `ended` ancestors and their parents,
/// and says whether it is the parent itself.
fn reaper<'a>(
    live: &HashMap<(&SpaceId, u32), &'a Identity>,
    ended: &HashMap<(&SpaceId, u32), u32>,
    workspace: &SpaceId,
    mut parent: u32,
) -> Option<(&'a Identity, bool)> {
    // Each step reaches another ended process, unless a reused PID makes a
    // cycle.
    for step in 0..=ended.len() {
        if let Some(identity) = live.get(&(workspace, parent)) {
            return Some((identity, step == 0));
        }
        parent = *ended.get(&(workspace, parent))?;
    }
    None
}

/// Also reads the processes of `tracked` that exited unreaped, whose
/// environment no scan can read, and which processes are commands'.
pub async fn sample(
    store: &crate::Store,
    tracked: Vec<(SpaceId, Identity)>,
    oneshot_runs: HashSet<String>,
) -> Result<Sample> {
    let (ids, commands) = store
        .read(|db| Ok((crate::spaces::space_ids(db)?, command_pids(db)?)))
        .await?;
    let running = identity::scan_workspaces(ids).await?;
    let oneshots = identity::scan_oneshot_runs(oneshot_runs).await?;
    let found: HashSet<&Identity> = running.iter().map(|(_, identity)| identity).collect();
    let missed: Vec<_> = tracked
        .into_iter()
        .filter(|(_, identity)| !found.contains(identity))
        .collect();
    let reading = move || {
        let mut sample = read(running, missed, usage::read, state)?;
        sample.commands = commands;
        let holds = crate::service::work::holds;
        sample.mark_work_slice(holds);
        sample.oneshots = Oneshots::read(&oneshots, usage::read, holds)?;
        Ok(sample)
    };
    tokio::task::spawn_blocking(reading)
        .await
        .context("workspace usage worker failed")?
}

pub enum State {
    Running,
    Unreaped,
    Gone,
}

pub fn state(identity: &Identity) -> Result<State> {
    Ok(if identity::alive(identity)? {
        State::Running
    } else if identity::unreaped(identity)? {
        State::Unreaped
    } else {
        State::Gone
    })
}

/// Keeps each reading by the `state` its process is in once every reading is
/// taken: a child reaped after its own reading may show in its parent's, so
/// it counts as gone and its time as its reaper's. A process that exits
/// after the scan is followed until reaped. A missed process counts only
/// while unreaped, since one still running no longer carries the identity.
pub fn read(
    found: Vec<(SpaceId, Identity)>,
    missed: Vec<(SpaceId, Identity)>,
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
        in_work_slice: HashSet::new(),
        oneshots: Oneshots::default(),
        commands: HashSet::new(),
    })
}

/// The PIDs of the commands Shoal runs in spaces besides their agents.
fn command_pids(db: &rusqlite::Connection) -> Result<HashSet<u32>> {
    Ok((shoal_store::all_executions(db)?.into_iter())
        .filter(|execution| execution.agent.is_none())
        .filter_map(|execution| Some(execution.child?.pid))
        .collect())
}
