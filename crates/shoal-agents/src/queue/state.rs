//! What the daemon keeps in memory about the agent queue.
use shoal_ids::execution_ids::ExecutionId;
use shoal_ids::repository_ids::RepositoryId;
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    sync::{
        Mutex as StdMutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use tokio::sync::{Mutex, MutexGuard, Notify, futures::Notified, watch};

use super::Grant;
use crate::model::HeldBy;
use shoal_ids::{queue_entry_ids::QueueEntryId, space_ids::SpaceId};
use shoal_protocol::time::UnixTime;

/// How long a waiting chat launch stays out of admission for the caller
/// that records its chat, should the caller never attach it.
const ATTACH_KEEP: Duration = Duration::from_secs(60);

#[derive(Default)]
pub struct AgentQueue {
    admission: Mutex<()>,
    /// Open holds per starting queue entry.
    holds: StdMutex<HashMap<QueueEntryId, usize>>,
    /// Waiting chat launches kept out of admission until their caller
    /// attaches the chat it records, with since when.
    attach_keeps: StdMutex<HashMap<QueueEntryId, Instant>>,
    /// Executions whose wrapper waits over its link to restore its stopped
    /// agent, every process of which has stopped, with what admitted the
    /// restore once admission did.
    restore_links: StdMutex<HashMap<ExecutionId, Option<Grant>>>,
    /// The recovery epoch whose healthy readings admitted each starting
    /// restore the daemon starts, which its agent checks as it takes over.
    recovery_starts: StdMutex<HashMap<QueueEntryId, u64>>,
    /// One-shot runs by request number, so the oldest comes first.
    oneshots: StdMutex<OneShots>,
    /// What held back each waiting entry at the last admission pass.
    held_back: StdMutex<HashMap<QueueEntryId, HeldBy>>,
    /// Since when each entry has been held back for its own memory, pass
    /// after pass.
    memory_since: StdMutex<HashMap<QueueEntryId, Instant>>,
    wake: Notify,
    /// Admission waits while the system shuts down.
    suspended: AtomicBool,
    /// Admission waits while the machine sleeps, from its announcement
    /// until it fully wakes, so a dark wake starts nothing.
    asleep: AtomicBool,
    /// Admission waits while the user is away from a machine whose
    /// `keep_awake` admits nothing new then.
    away: AtomicBool,
    /// Since when the user paused the machine, while admission waits for
    /// `shoal unpause`.
    paused: StdMutex<Option<UnixTime>>,
}

#[derive(Default)]
struct OneShots {
    next: u64,
    runs: BTreeMap<u64, OneShot>,
}

/// A one-shot run's request for admission in `repository`, for the work it
/// serves; it holds no heavy slot, but its memory counts.
struct OneShot {
    repository: RepositoryId,
    serving: Option<Serving>,
    work: OneShotWork,
    /// The memory, in bytes, it holds now, as its last report said.
    using: u64,
    /// Its processes, once it said where they run.
    processes: Option<RunProcesses>,
    requested_at: UnixTime,
    /// Whether protection stopped its earlier run, so it waits for healthy
    /// readings as a stopped agent's restore does.
    after_stop: bool,
    /// When admission let it start, which it holds until it ends.
    admitted: watch::Sender<Option<Instant>>,
    /// Stops the run for protection, which names why, until it is used or
    /// the run cannot be stopped.
    stop: Option<StopRun>,
}

impl OneShot {
    /// Whether it still waits for admission. Cancelling a waiting run takes
    /// its stop, which keeps admission from starting it before its
    /// connection learns of the cancel.
    fn waits(&self) -> bool {
        self.admitted.borrow().is_none() && self.stop.is_some()
    }
}

/// What a one-shot run runs, which decides how admission weighs it and
/// where protection stops it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OneShotWork {
    /// A review's agent, which counts as an agent's start and goes first
    /// under load, since it starts over at no loss.
    Review,
    /// A command or seed build, which counts at its expected memory and
    /// goes after CI jobs under load.
    Measured(Measured),
}

/// A command or seed build, expected to use `expected` bytes of memory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Measured {
    pub kind: MeasuredKind,
    pub expected: u64,
}

/// What a measured run is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MeasuredKind {
    /// A `shoal run` command, as `shoal queue` names it.
    Command(String),
    SeedBuild,
}

/// Where a command's or seed build's processes run, which take its place
/// in the kernel's OOM scores.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunProcesses {
    /// A `shoal run` command's, which carry its execution's marker.
    Execution(ExecutionId),
    /// The process group of a seed build's step.
    Group(u32),
}

/// An admitted command or seed build, as protection weighs its stop.
pub struct RunningMeasured {
    /// Its queue ID while it waited, by which protection stops it.
    pub id: QueueEntryId,
    pub repository: RepositoryId,
    /// The space whose work it serves; none for a seed build.
    pub space: Option<SpaceId>,
    /// How long it has run.
    pub running_for: Duration,
    /// The memory, in bytes, it holds now.
    pub using: u64,
}

/// An admitted one-shot run, as measured memory counts it.
pub struct AdmittedOneShot {
    pub repository: RepositoryId,
    pub work: OneShotWork,
    /// How long it has run.
    pub running_for: Duration,
    pub using: u64,
}

/// What stops a one-shot run, given why.
type StopRun = Box<dyn FnOnce(&str) + Send + Sync>;

/// Which admitted one-shot runs protection stops.
pub enum OneShotStop {
    /// The run that started last.
    Newest,
    All,
}

/// A one-shot request waiting for admission.
pub struct WaitingOneShot {
    /// Its queue ID, which no stored entry has.
    pub id: QueueEntryId,
    pub repository: RepositoryId,
    /// What it serves, which it ranks with.
    pub serving: Option<Serving>,
    pub work: OneShotWork,
    pub requested_at: UnixTime,
    pub after_stop: bool,
}

/// The work a one-shot run serves, which places it in the queue.
#[derive(Clone)]
pub struct Serving {
    /// The space whose work it serves.
    pub space: SpaceId,
    /// The sequence number of the newest queue entry when it asked, so it
    /// arrives after that entry and before any queued later.
    pub queued_after: i64,
}

/// The queue ID of one-shot request `number`, which no stored entry has.
fn oneshot_id(number: u64) -> QueueEntryId {
    format!("{ONESHOT_PREFIX}{number}").into()
}

const ONESHOT_PREFIX: &str = "oneshot:";

/// The number of the one-shot request whose queue ID is `id`, if it names
/// one.
fn oneshot_number(id: &QueueEntryId) -> Option<u64> {
    id.as_str().strip_prefix(ONESHOT_PREFIX)?.parse().ok()
}

impl AgentQueue {
    /// Serialize admission, so two passes cannot fill one slot.
    pub async fn lock_admission(&self) -> MutexGuard<'_, ()> {
        self.admission.lock().await
    }

    pub fn hold(&self, id: &QueueEntryId) {
        *self.holds.lock().unwrap().entry(id.clone()).or_default() += 1;
    }

    pub fn release(&self, id: &QueueEntryId) {
        let mut holds = self.holds.lock().unwrap();
        if let Some(count) = holds.get_mut(id) {
            *count -= 1;
            if *count == 0 {
                holds.remove(id);
            }
        }
    }

    /// The entries with an open hold, or kept for their chat's attach for
    /// less than [`ATTACH_KEEP`].
    pub fn held(&self) -> HashSet<QueueEntryId> {
        let mut held: HashSet<QueueEntryId> = self.holds.lock().unwrap().keys().cloned().collect();
        let mut keeps = self.attach_keeps.lock().unwrap();
        let now = Instant::now();
        keeps.retain(|_, since| now.duration_since(*since) < ATTACH_KEEP);
        held.extend(keeps.keys().cloned());
        held
    }

    /// Keep the waiting launch `id` out of admission until its chat is
    /// attached to it.
    pub fn keep_for_attach(&self, id: &QueueEntryId) {
        let mut keeps = self.attach_keeps.lock().unwrap();
        keeps.insert(id.clone(), Instant::now());
    }

    /// Let admission take the launch `id` again, its chat attached or not.
    pub fn end_attach_keep(&self, id: &QueueEntryId) {
        self.attach_keeps.lock().unwrap().remove(id);
    }

    /// Replace what held back waiting entries with the pass's `held` at `now`.
    /// An entry held for its memory keeps since when. One held for anything
    /// else, or no longer among the `unstarted` ones, waits anew. One the pass
    /// did not measure, since it started another, keeps its wait.
    pub fn record_held_back(
        &self,
        held: HashMap<QueueEntryId, HeldBy>,
        unstarted: &HashSet<QueueEntryId>,
        now: Instant,
    ) {
        let mut since = self.memory_since.lock().unwrap();
        since.retain(|id, _| match held.get(id) {
            Some(held) => matches!(held, HeldBy::Memory { .. }),
            None => unstarted.contains(id),
        });
        for (id, held) in &held {
            if let HeldBy::Memory { .. } = held {
                since.entry(id.clone()).or_insert(now);
            }
        }
        *self.held_back.lock().unwrap() = held;
    }

    /// How long, at `now`, the entry `id` has been held back for its memory.
    pub fn waited_for_memory(&self, id: &QueueEntryId, now: Instant) -> Duration {
        let since = self.memory_since.lock().unwrap();
        since
            .get(id)
            .map_or(Duration::ZERO, |since| now.duration_since(*since))
    }

    /// What held back the entry `id` at the last admission pass.
    pub fn held_back(&self, id: &QueueEntryId) -> Option<HeldBy> {
        self.held_back.lock().unwrap().get(id).cloned()
    }

    /// Mark the wrapper of `execution` as waiting over its link to restore its
    /// stopped agent, while the returned link lives. Every process of that
    /// agent has stopped.
    pub fn link_restore(&self, execution: &ExecutionId) -> RestoreLink<'_> {
        self.restore_links
            .lock()
            .unwrap()
            .insert(execution.to_owned(), None);
        // Its restore may be admitted now.
        self.wake();
        RestoreLink {
            queue: self,
            execution: execution.to_owned(),
        }
    }

    /// The executions whose wrapper waits over its link to restore its agent.
    pub fn restore_links(&self) -> HashSet<ExecutionId> {
        self.restore_links.lock().unwrap().keys().cloned().collect()
    }

    /// Record that `grant` admitted the restore of `execution`, if its
    /// wrapper still waits over its link.
    pub fn grant_restore(&self, execution: &ExecutionId, grant: Grant) {
        if let Some(granted) = self.restore_links.lock().unwrap().get_mut(execution) {
            *granted = Some(grant);
        }
    }

    /// Forget what admitted the restore of `execution`, which returns to its
    /// place in the queue.
    pub fn revoke_restore(&self, execution: &ExecutionId) {
        if let Some(granted) = self.restore_links.lock().unwrap().get_mut(execution) {
            *granted = None;
        }
    }

    /// Record that the healthy readings of recovery `epoch` admitted the
    /// restore `id`, which the daemon starts.
    pub fn admit_on_recovery(&self, id: &QueueEntryId, epoch: u64) {
        self.recovery_starts
            .lock()
            .unwrap()
            .insert(id.to_owned(), epoch);
    }

    /// The recovery epoch that admitted the restore `id`, forgotten as its
    /// agent takes it over; none for an entry readings did not admit.
    pub fn take_recovery_start(&self, id: &QueueEntryId) -> Option<u64> {
        self.recovery_starts.lock().unwrap().remove(id)
    }

    /// What admitted the restore of `execution`, if admission did.
    pub fn restore_grant(&self, execution: &ExecutionId) -> Option<Grant> {
        self.restore_links
            .lock()
            .unwrap()
            .get(execution)
            .copied()
            .flatten()
    }

    /// Ask for admission in `repository` for a one-shot run, such as a
    /// review, of the work it is `serving`, which waits in the queue at the
    /// rank of that space; the request and the admission end with the
    /// returned value.
    /// Protection calls `stop` to stop the run; a run it stopped asks again
    /// `after_stop`.
    pub fn request_oneshot(
        &self,
        repository: &RepositoryId,
        serving: Serving,
        after_stop: bool,
        stop: impl FnOnce() + Send + Sync + 'static,
    ) -> OneShotAdmission<'_> {
        let review = (Some(serving), OneShotWork::Review);
        self.request(repository, review, after_stop, move |_: &str| stop())
    }

    /// Ask for admission in `repository` for a command or seed build,
    /// `measured`, of the work it is `serving`, as [`Self::request_oneshot`]
    /// asks for a review's.
    pub fn request_measured(
        &self,
        repository: &RepositoryId,
        serving: Option<Serving>,
        measured: Measured,
        after_stop: bool,
        stop: impl FnOnce(&str) + Send + Sync + 'static,
    ) -> OneShotAdmission<'_> {
        let run = (serving, OneShotWork::Measured(measured));
        self.request(repository, run, after_stop, stop)
    }

    fn request(
        &self,
        repository: &RepositoryId,
        (serving, work): (Option<Serving>, OneShotWork),
        after_stop: bool,
        stop: impl FnOnce(&str) + Send + Sync + 'static,
    ) -> OneShotAdmission<'_> {
        let (admitted, waiting) = watch::channel(None);
        let mut oneshots = self.oneshots.lock().unwrap();
        let number = oneshots.next;
        oneshots.next += 1;
        oneshots.runs.insert(
            number,
            OneShot {
                repository: repository.clone(),
                serving,
                work,
                using: 0,
                processes: None,
                requested_at: UnixTime::now(),
                after_stop,
                admitted,
                stop: Some(Box::new(stop)),
            },
        );
        drop(oneshots);
        self.wake();
        OneShotAdmission {
            queue: self,
            number,
            admitted: waiting,
        }
    }

    /// Hold an admission in `repository` until the daemon restarts, for a run a
    /// stopped daemon left whose processes could not be proven stopped.
    pub fn hold_oneshot(&self, repository: &RepositoryId) {
        let admission = self.request(repository, (None, OneShotWork::Review), false, |_| {});
        if let Some(run) = self.oneshots.lock().unwrap().runs.get(&admission.number) {
            run.admitted.send_replace(Some(Instant::now()));
        }
        admission.keep();
    }

    /// The one-shot requests still waiting, oldest first.
    pub fn waiting_oneshots(&self) -> Vec<WaitingOneShot> {
        self.oneshots
            .lock()
            .unwrap()
            .runs
            .iter()
            .filter(|(_, run)| run.waits())
            .map(|(number, run)| WaitingOneShot {
                id: oneshot_id(*number),
                repository: run.repository.clone(),
                serving: run.serving.clone(),
                work: run.work.clone(),
                requested_at: run.requested_at,
                after_stop: run.after_stop,
            })
            .collect()
    }

    /// Stop the admitted reviews `which` picks among those not yet stopped
    /// and not held, returning how many. Commands and seed builds take
    /// their place in the stop order instead.
    pub fn stop_oneshots(&self, which: OneShotStop) -> usize {
        let mut oneshots = self.oneshots.lock().unwrap();
        let mut stoppable: Vec<&mut OneShot> = oneshots
            .runs
            .values_mut()
            .filter(|run| run.work == OneShotWork::Review)
            .filter(|run| run.admitted.borrow().is_some() && run.stop.is_some())
            .collect();
        if let OneShotStop::Newest = which {
            stoppable.sort_by_key(|run| *run.admitted.borrow());
            stoppable.drain(..stoppable.len().saturating_sub(1));
        }
        for run in &mut stoppable {
            if let Some(stop) = run.stop.take() {
                stop("");
            }
        }
        stoppable.len()
    }

    /// The admitted one-shot runs.
    pub fn admitted_oneshots(&self) -> Vec<AdmittedOneShot> {
        self.oneshots
            .lock()
            .unwrap()
            .runs
            .values()
            .filter_map(|run| {
                Some(AdmittedOneShot {
                    repository: run.repository.clone(),
                    work: run.work.clone(),
                    running_for: run.admitted.borrow().as_ref()?.elapsed(),
                    using: run.using,
                })
            })
            .collect()
    }

    /// The admitted commands and seed builds protection may stop.
    pub fn running_measured(&self) -> Vec<RunningMeasured> {
        let oneshots = self.oneshots.lock().unwrap();
        (oneshots.runs.iter())
            .filter(|(_, run)| matches!(run.work, OneShotWork::Measured(_)) && run.stop.is_some())
            .filter_map(|(number, run)| {
                Some(RunningMeasured {
                    id: oneshot_id(*number),
                    repository: run.repository.clone(),
                    space: run.serving.as_ref().map(|serving| serving.space.clone()),
                    running_for: run.admitted.borrow().as_ref()?.elapsed(),
                    using: run.using,
                })
            })
            .collect()
    }

    /// Cancel the waiting command `id` for `reason`, returning whether it
    /// waited.
    pub fn cancel_waiting(&self, id: &QueueEntryId, reason: &str) -> bool {
        let Some(number) = oneshot_number(id) else {
            return false;
        };
        let stop = (self.oneshots.lock().unwrap().runs.get_mut(&number))
            .filter(|run| run.waits())
            .and_then(|run| run.stop.take());
        stop.map(|stop| stop(reason)).is_some()
    }

    /// Cancel every command waiting to run in `space` for `reason`,
    /// returning how many waited.
    pub fn cancel_waiting_in(&self, space: &SpaceId, reason: &str) -> usize {
        let stops: Vec<StopRun> = {
            let mut oneshots = self.oneshots.lock().unwrap();
            (oneshots.runs.values_mut())
                .filter(|run| matches!(run.work, OneShotWork::Measured(_)) && run.waits())
                .filter(|run| {
                    run.serving
                        .as_ref()
                        .is_some_and(|serving| serving.space == *space)
                })
                .filter_map(|run| run.stop.take())
                .collect()
        };
        let cancelled = stops.len();
        for stop in stops {
            stop(reason);
        }
        cancelled
    }

    /// Where the admitted command or seed build `id` runs, once it said.
    pub fn measured_processes(&self, id: &QueueEntryId) -> Option<RunProcesses> {
        let number = oneshot_number(id)?;
        let oneshots = self.oneshots.lock().unwrap();
        oneshots.runs.get(&number)?.processes.clone()
    }

    /// What the admitted command or seed build `id` holds now, if it runs.
    pub fn measured_using(&self, id: &QueueEntryId) -> Option<u64> {
        let number = oneshot_number(id)?;
        let oneshots = self.oneshots.lock().unwrap();
        let run = oneshots.runs.get(&number)?;
        run.admitted.borrow().is_some().then_some(run.using)
    }

    /// Stop the admitted command or seed build `id` for protection, which
    /// names its `reason`, returning whether it was there to stop.
    pub fn stop_measured(&self, id: &QueueEntryId, reason: &str) -> bool {
        let Some(number) = oneshot_number(id) else {
            return false;
        };
        let stop = (self.oneshots.lock().unwrap().runs.get_mut(&number))
            .filter(|run| run.admitted.borrow().is_some())
            .and_then(|run| run.stop.take());
        stop.map(|stop| stop(reason)).is_some()
    }

    /// Admit the one-shot request with queue ID `id`, returning whether it
    /// still waited; none when `id` names no one-shot request.
    pub fn admit_oneshot(&self, id: &QueueEntryId) -> Option<bool> {
        let number = oneshot_number(id)?;
        let oneshots = self.oneshots.lock().unwrap();
        let Some(run) = oneshots.runs.get(&number).filter(|run| run.waits()) else {
            return Some(false);
        };
        run.admitted.send_replace(Some(Instant::now()));
        Some(true)
    }

    /// Hold back admission while the system shuts down, so nothing the
    /// shutdown stopped starts again before it; resuming wakes the dispatcher.
    /// Takes the admission gate, so an admission pass already past its
    /// check finishes first.
    pub async fn suspend(&self, suspended: bool) {
        let _gate = self.lock_admission().await;
        self.suspended.store(suspended, Ordering::SeqCst);
        if !suspended {
            self.wake();
        }
    }

    pub fn suspended(&self) -> bool {
        self.suspended.load(Ordering::SeqCst)
    }

    /// Hold back admission while the machine sleeps; waking wakes the
    /// dispatcher. Unlike [`Self::suspend`] it never waits for the
    /// admission gate, which a start may hold for longer than the
    /// platform waits for a sleep: an admission pass already past its
    /// check finishes, and the next one admits nothing.
    pub fn sleep(&self, asleep: bool) {
        self.asleep.store(asleep, Ordering::SeqCst);
        if !asleep {
            self.wake();
        }
    }

    /// Hold back admission while the user is away, as [`Self::sleep`]
    /// does while the machine sleeps; their return wakes the dispatcher.
    pub fn away(&self, away: bool) {
        let was = self.away.swap(away, Ordering::SeqCst);
        if was && !away {
            self.wake();
        }
    }

    /// Whether admission waits for the user's return.
    pub fn held_away(&self) -> bool {
        self.away.load(Ordering::SeqCst)
    }

    /// Hold back admission from `since`, while the user paused the
    /// machine, or with none no longer; unpausing wakes the dispatcher.
    pub fn pause(&self, since: Option<UnixTime>) {
        let was = std::mem::replace(&mut *self.paused.lock().unwrap(), since);
        if was.is_some() && since.is_none() {
            self.wake();
        }
    }

    /// Since when admission waits for the user to unpause the machine.
    pub fn paused_since(&self) -> Option<UnixTime> {
        *self.paused.lock().unwrap()
    }

    pub fn holds_admission(&self) -> bool {
        self.suspended()
            || self.asleep.load(Ordering::SeqCst)
            || self.held_away()
            || self.paused_since().is_some()
    }

    /// Wake the dispatcher, since a slot may have freed or work may fit.
    pub fn wake(&self) {
        self.wake.notify_one();
    }

    /// Resolves once something wakes the dispatcher.
    pub fn woken(&self) -> Notified<'_> {
        self.wake.notified()
    }
}

/// A one-shot run's place in the queue, then its admission, while it lives.
pub struct OneShotAdmission<'a> {
    queue: &'a AgentQueue,
    number: u64,
    admitted: watch::Receiver<Option<Instant>>,
}

impl OneShotAdmission<'_> {
    /// Wait until admission lets the run start.
    pub async fn admitted(&mut self) {
        // The sender lives as long as this request.
        let _ = self.admitted.wait_for(Option::is_some).await;
    }

    /// Its queue ID while it waits.
    pub fn id(&self) -> QueueEntryId {
        oneshot_id(self.number)
    }

    /// Admit it now, whatever the limits, as `--now` starts work.
    pub fn admit_now(&self) {
        self.queue.admit_oneshot(&self.id());
    }

    /// Record that the run's processes are `processes`.
    pub fn runs_as(&self, processes: RunProcesses) {
        let mut oneshots = self.queue.oneshots.lock().unwrap();
        if let Some(run) = oneshots.runs.get_mut(&self.number) {
            run.processes = Some(processes);
        }
    }

    /// Record that the run holds `memory` bytes now.
    pub fn using(&self, memory: u64) {
        let mut oneshots = self.queue.oneshots.lock().unwrap();
        if let Some(run) = oneshots.runs.get_mut(&self.number) {
            run.using = memory;
        }
    }

    /// Keep the admission of a run whose processes may still be running until the
    /// daemon restarts. Protection no longer picks it, as nothing stops it.
    pub fn keep(self) {
        if let Some(run) = self
            .queue
            .oneshots
            .lock()
            .unwrap()
            .runs
            .get_mut(&self.number)
        {
            run.stop = None;
        }
        std::mem::forget(self);
    }
}

impl Drop for OneShotAdmission<'_> {
    fn drop(&mut self) {
        self.queue
            .oneshots
            .lock()
            .unwrap()
            .runs
            .remove(&self.number);
        // A freed slot may let another entry start.
        self.queue.wake();
    }
}

/// The link of a wrapper that waits to restore its stopped agent, through
/// which admission hands it the restore, while it lives.
pub struct RestoreLink<'a> {
    queue: &'a AgentQueue,
    execution: ExecutionId,
}

impl RestoreLink<'_> {
    pub fn execution(&self) -> &ExecutionId {
        &self.execution
    }
}

impl Drop for RestoreLink<'_> {
    fn drop(&mut self) {
        self.queue
            .restore_links
            .lock()
            .unwrap()
            .remove(&self.execution);
    }
}
