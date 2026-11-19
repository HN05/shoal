//! Dispatch: each pass ends abandoned starts, takes over the restores whose
//! wrapper is gone, queues the restores stopped agents request, leaves those
//! recovery no longer restores for `shoal restore`, then admits the waiting
//! entries that fit and starts what the daemon starts itself.
use shoal_ids::execution_ids::ExecutionId;
use shoal_protocol::space_names::SpaceName;
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    time::Duration,
};

use anyhow::{Context, Result};
use rusqlite::params;

use super::{
    Grant, blockers,
    carrier::Carrier,
    expiry,
    limits::{Admission, Waiting, Wake, Work, admit},
    store::{QueuedRow, queued_rows},
    weight::Light,
};
use crate::{
    daemon::{manager::Manager, notifications::AlertKind},
    model::HeldBy,
    state::{QueueKind, QueueState},
};
use shoal_ids::queue_entry_ids::QueueEntryId;
use shoal_protocol::time::UnixTime;

/// How long a starting entry keeps its slot without a holder: long enough for
/// a launch to hand over to the place a host opens or reconnect after a
/// daemon restart.
pub(super) const START_GRACE: Duration = Duration::from_secs(120);

/// An admitted entry the daemon starts itself.
pub(crate) struct Start {
    pub id: QueueEntryId,
    pub name: SpaceName,
    pub env: BTreeMap<String, String>,
}

/// A queued restore as admission sees it: its agent's execution, what it
/// waits for and what carries it out once admitted.
struct QueuedRestore {
    execution: ExecutionId,
    wake: Wake,
    carrier: Carrier,
}

impl QueuedRestore {
    /// Whether its agent holds no slot while it waits: every process of it
    /// has stopped, and it does not wait for healthy readings in its
    /// wrapper, which keeps the slot it takes back.
    fn dormant(&self) -> bool {
        self.carrier == Carrier::Daemon || self.wake != Wake::Recovery
    }
}

/// The waiting entries as admission sees them.
struct WaitingWork {
    entries: Vec<Waiting>,
    /// Each restore that something can carry out, by entry ID.
    restores: HashMap<QueueEntryId, QueuedRestore>,
    /// The jobs left out of `entries` while their repository's permits are
    /// all held.
    permits_busy: Vec<QueueEntryId>,
    /// The entries of `entries` whose tool's upstream is under a limit,
    /// which wait it out.
    upstream_limited: HashMap<QueueEntryId, HeldBy>,
    /// Which agents are light, read once for the pass while any entry waits.
    light: Light,
}

impl WaitingWork {
    /// The entries admission may start: those no upstream limit holds.
    fn admissible(&self) -> Vec<Waiting> {
        (self.entries.iter())
            .filter(|entry| !self.upstream_limited.contains_key(&entry.id))
            .cloned()
            .collect()
    }

    /// What holds back the entries of `entries` that an upstream limit
    /// holds, leaving out those blockers took away.
    fn upstream_holds(&self) -> impl Iterator<Item = (QueueEntryId, HeldBy)> + '_ {
        (self.entries.iter())
            .filter_map(|entry| Some((entry.id.clone(), *self.upstream_limited.get(&entry.id)?)))
    }

    /// The entries that still wait once `admitted` start.
    fn unstarted(&self, admitted: &[QueueEntryId]) -> HashSet<QueueEntryId> {
        (self.entries.iter())
            .map(|entry| entry.id.clone())
            .filter(|id| !admitted.contains(id))
            .collect()
    }

    /// Whether `admitted` holds an entry whose work is of `kind`.
    fn admits(&self, admitted: &[QueueEntryId], kind: fn(&Work) -> bool) -> bool {
        self.entries
            .iter()
            .any(|entry| kind(&entry.work) && admitted.contains(&entry.id))
    }

    /// The restores in `admitted`, by entry ID.
    fn admitted_restores<'a>(
        &'a self,
        admitted: &'a [QueueEntryId],
    ) -> impl Iterator<Item = (&'a QueueEntryId, &'a QueuedRestore)> {
        admitted
            .iter()
            .filter_map(|id| Some((id, self.restores.get(id)?)))
    }

    /// Whether `admitted` holds an entry that waited for healthy readings: a
    /// restore of an agent load or disk space stopped mid-turn or a one-shot
    /// run protection stopped.
    fn needs_recovery(&self, admitted: &[QueueEntryId]) -> bool {
        self.admitted_restores(admitted)
            .any(|(_, restore)| restore.wake == Wake::Recovery)
            || self.admits(admitted, |work| *work == Work::OneShot { after_stop: true })
    }

    /// The executions of the restores whose agent holds no slot while it
    /// waits.
    fn dormant(&self) -> HashSet<ExecutionId> {
        (self.restores.values())
            .filter(|restore| restore.dormant())
            .map(|restore| restore.execution.clone())
            .collect()
    }
}

impl Manager {
    /// End abandoned starts and those resolved another way, with the
    /// restarts they started, return the starts whose setup failed to wait
    /// for it, take over the restores whose wrapper is gone, queue the
    /// restores stopped agents request, then start the waiting entries that
    /// fit.
    pub async fn dispatch_queue(&self) -> Result<()> {
        let _gate = self.agent_queue.lock_admission().await;
        self.expire_starting_entries().await?;
        self.return_failed_starts().await?;
        self.adopt_orphaned_restores().await?;
        self.forget_resolved_restores().await?;
        self.fail_lost_renewals().await?;
        self.queue_requested_restores().await?;
        self.leave_unrecovered_restores().await?;
        self.admit_waiting(None).await?;
        Ok(())
    }

    /// Admit waiting entries that fit, record what holds back the others,
    /// start launches other than `caller`'s, and return whether `caller`'s
    /// entry, which the caller may hold, was admitted. An admitted restore
    /// is left to its waiting agent; nothing is admitted while the system
    /// shuts down. Requires the queue gate.
    pub(super) async fn admit_waiting(&self, caller: Option<&QueueEntryId>) -> Result<bool> {
        if self.agent_queue.suspended() {
            return Ok(false);
        }
        let mut work = self.waiting_work(caller).await?;
        self.hold_back_for_exit(&mut work);
        let bases = self.hold_back_blocked(&mut work.entries).await?;
        let published = self.overload_recovery.published();
        let admission = self.fitting_entries(&work, published.is_some()).await?;
        self.record_held_back(&work, &admission);
        if admission.admitted.is_empty() {
            return Ok(false);
        }
        let Some(admitted) = self.grant_admission(&work, admission.admitted, published) else {
            return Ok(false);
        };
        let starts = self.mark_starting(admitted, bases).await?;
        self.start_launches(starts, caller).await
    }

    /// Leave out of `work` every entry the daemon would start while an exit
    /// holds back new work, so a stop keeps them queued for the next daemon
    /// rather than failing them; a restore its waiting wrapper carries out
    /// stays, since the exit may be waiting for it.
    fn hold_back_for_exit(&self, work: &mut WaitingWork) {
        if self.gates.exiting().is_some() {
            let restores = &work.restores;
            work.entries.retain(|entry| {
                (restores.get(&entry.id)).is_some_and(|restore| restore.carrier == Carrier::Wrapper)
            });
        }
    }

    /// Record what held back the entries of `work` that `admission` passed
    /// over, now.
    fn record_held_back(&self, work: &WaitingWork, admission: &Admission) {
        let unstarted = work.unstarted(&admission.admitted);
        let held = admission.held.clone();
        (self.agent_queue).record_held_back(held, &unstarted, std::time::Instant::now());
    }

    /// The entries of `work` that fit beside the slots in use, and what
    /// holds back the others, its jobs whose permits are all held and its
    /// entries whose tool's upstream is limited among them, where
    /// `recovered` says whether healthy readings are published.
    async fn fitting_entries(&self, work: &WaitingWork, recovered: bool) -> Result<Admission> {
        let mut admission = match work.entries.is_empty() {
            true => Admission::default(),
            false => self.admission(work, recovered).await?,
        };
        let permits = work.permits_busy.iter().cloned();
        admission
            .held
            .extend(permits.map(|id| (id, HeldBy::Permits)));
        admission.held.extend(work.upstream_holds());
        Ok(admission)
    }

    /// One admission pass over the entries of `work`.
    async fn admission(&self, work: &WaitingWork, recovered: bool) -> Result<Admission> {
        let repositories = (work.entries.iter())
            .filter_map(|entry| entry.repository.clone())
            .collect();
        let slots = self.occupied_slots(&work.dormant(), &work.light).await?;
        let jobs = self.job_activities().await?;
        let limits = self.agent_limits(repositories, &slots, &jobs).await?;
        let occupied: Vec<_> = slots.into_iter().map(|slot| slot.repository).collect();
        Ok(admit(&occupied, &work.admissible(), &limits, recovered))
    }

    /// Grant the `admitted` entries of `work` their start: wake the runners
    /// for a CI job, let each restore's waiting wrapper go and admit one-shot
    /// runs, using up the recovery epoch `published` for those that waited
    /// for healthy readings, which a restore the daemon starts keeps for its
    /// agent to check. Return the entries left to mark
    /// starting, or none when a stop, reload or unsafe reading withdrew the
    /// readings since; the pass it wakes decides again.
    fn grant_admission(
        &self,
        work: &WaitingWork,
        admitted: Vec<QueueEntryId>,
        published: Option<u64>,
    ) -> Option<Vec<QueueEntryId>> {
        if work.admits(&admitted, |work| matches!(work, Work::Job { .. })) {
            self.runner.jobs().wake();
        }
        let epoch = match work.needs_recovery(&admitted) {
            true => Some(published.and_then(|epoch| self.overload_recovery.consume(epoch))?),
            false => None,
        };
        for (id, restore) in work.admitted_restores(&admitted) {
            match (restore.carrier, restore.wake.grant(epoch)) {
                (Carrier::Wrapper, Some(grant)) => {
                    self.agent_queue.grant_restore(&restore.execution, grant);
                }
                (Carrier::Daemon, Some(Grant::Recovery(epoch))) => {
                    self.agent_queue.admit_on_recovery(id, epoch);
                }
                (Carrier::Daemon, _) | (Carrier::Wrapper, None) => {}
            }
        }
        Some(
            admitted
                .into_iter()
                .filter(|id| !self.agent_queue.admit_oneshot(id))
                .collect(),
        )
    }

    /// The queued entries in rank order, each restore with what it waits for
    /// and what carries it out, without those a caller holds to start itself
    /// but `caller`'s own, and those nothing can carry out yet.
    async fn waiting_work(&self, caller: Option<&QueueEntryId>) -> Result<WaitingWork> {
        let now = std::time::Instant::now();
        let held = self.agent_queue.held();
        let mut queued = self.store.run(|db| queued_rows(db)).await?;
        queued.retain(|row| !held.contains(&row.id) || Some(&row.id) == caller);
        let oneshots = self.agent_queue.waiting_oneshots();
        let light = match queued.is_empty() && oneshots.is_empty() {
            true => Light::default(),
            false => self.light().await?,
        };
        let upstream_limited = self.upstream_limited(&queued).await?;
        let carriers = self.carriers();
        let mut restores = HashMap::new();
        let mut permits_busy = Vec::new();
        let mut waiting = Vec::new();
        for QueuedRow {
            id,
            repository,
            kind,
            execution,
            workspace,
            started_here,
            automatic,
            job,
            mode,
            agent: _,
        } in queued
        {
            let is_light = kind != QueueKind::Job
                && light.entry(execution.as_ref(), repository.as_ref(), mode.as_ref());
            let work = match (kind, execution, workspace) {
                (QueueKind::Job, ..) => {
                    let repository = repository
                        .as_ref()
                        .context("a job's entry names no repository")?;
                    // A job waits while its repository's permits are all held.
                    if self.runner.jobs().permits_busy(repository) {
                        permits_busy.push(id);
                        continue;
                    }
                    let job = serde_json::from_str(&job.context("a job's entry names no job")?)?;
                    Work::Job {
                        cost: self.runner.job_cost(repository, &job).await?,
                    }
                }
                (QueueKind::Restore, Some(execution), Some(workspace)) => {
                    let Some(carrier) = carriers.of(started_here, &execution) else {
                        continue;
                    };
                    let wake = (self.restore_wake(&workspace, &execution, automatic)).await?;
                    let restore = QueuedRestore {
                        execution,
                        wake,
                        carrier,
                    };
                    let holds_slot = !restore.dormant();
                    restores.insert(id.clone(), restore);
                    Work::Restore { holds_slot, wake }
                }
                _ => Work::Launch,
            };
            waiting.push(Waiting {
                waited_for_memory: self.agent_queue.waited_for_memory(&id, now),
                id,
                repository,
                work,
                light: is_light,
            });
        }
        let oneshots = oneshots.into_iter().map(|oneshot| Waiting {
            waited_for_memory: self.agent_queue.waited_for_memory(&oneshot.id, now),
            id: oneshot.id,
            repository: Some(oneshot.repository),
            work: Work::OneShot {
                after_stop: oneshot.after_stop,
            },
            light: false,
        });
        waiting.extend(oneshots);
        self.in_rank_order(&mut waiting).await?;
        Ok(WaitingWork {
            entries: waiting,
            restores,
            permits_busy,
            upstream_limited,
            light,
        })
    }

    /// The agents of `queued` whose tool's upstream stands under a limit,
    /// with the hold each waits for.
    async fn upstream_limited(
        &self,
        queued: &[QueuedRow],
    ) -> Result<HashMap<QueueEntryId, HeldBy>> {
        if queued.is_empty() {
            return Ok(HashMap::new());
        }
        let limits = self.tool_limits(UnixTime::now()).await?;
        Ok((queued.iter())
            .filter(|row| row.kind != QueueKind::Job)
            .filter_map(|row| {
                let (limit, until) = *limits.get(&row.agent)?;
                Some((row.id.clone(), HeldBy::UpstreamLimit { limit, until }))
            })
            .collect())
    }

    /// Sort `waiting` in rank order; an entry that arrived since the rank
    /// was read goes last.
    async fn in_rank_order(&self, waiting: &mut [Waiting]) -> Result<()> {
        let order: HashMap<QueueEntryId, usize> = (self.ranked_queue().await?.into_iter())
            .enumerate()
            .map(|(place, ranked)| (ranked.id, place))
            .collect();
        waiting.sort_by_key(|entry| order.get(&entry.id).copied().unwrap_or(usize::MAX));
        Ok(())
    }

    /// Take the launches that blockers hold back out of `waiting`, returning
    /// the branch each remaining launch with a done blocker builds on.
    async fn hold_back_blocked(
        &self,
        waiting: &mut Vec<Waiting>,
    ) -> Result<HashMap<QueueEntryId, String>> {
        let readiness = self.blocker_readiness().await?;
        waiting.retain(|entry| {
            !matches!(
                readiness.get(&entry.id),
                Some(blockers::Readiness::Waiting(_))
            )
        });
        Ok(readiness
            .into_iter()
            .filter_map(|(id, readiness)| match readiness {
                blockers::Readiness::Ready { base } => Some((id, base?)),
                blockers::Readiness::Waiting(_) => None,
            })
            .collect())
    }

    /// Mark `admitted` entries starting, each on its base from `bases`,
    /// returning those the daemon starts: launches and the restores that
    /// carry a start environment.
    pub(super) async fn mark_starting(
        &self,
        admitted: Vec<QueueEntryId>,
        mut bases: HashMap<QueueEntryId, String>,
    ) -> Result<Vec<Start>> {
        self.store
            .write(move |tx| {
                let now = UnixTime::now();
                let mut starts = Vec::new();
                for id in admitted {
                    tx.execute(
                        "UPDATE agent_queue SET state=?2,seen_at=?3,base_branch=?4 WHERE id=?1",
                        params![id, QueueState::Starting, now, bases.remove(&id)],
                    )?;
                    let (name, env): (SpaceName, Option<String>) = tx.query_row(
                        "SELECT name,env FROM agent_queue WHERE id=?1",
                        [&id],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )?;
                    if let Some(env) = env {
                        starts.push(Start {
                            id,
                            name,
                            env: serde_json::from_str(&env).context("read queued environment")?,
                        });
                    }
                }
                Ok(starts)
            })
            .await
    }

    /// Start each admitted launch other than `caller`'s, which starts in the
    /// caller, and return whether `caller`'s was among them.
    pub(super) async fn start_launches(
        &self,
        starts: Vec<Start>,
        caller: Option<&QueueEntryId>,
    ) -> Result<bool> {
        let mut caller_admitted = false;
        for start in starts {
            if Some(&start.id) == caller {
                caller_admitted = true;
                continue;
            }
            match crate::daemon::queued_starts::spawn_start(&self.paths, &start) {
                Ok(()) => {
                    self.notify(
                        Some(&start.name),
                        AlertKind::QueueStarted,
                        format!("Starting {} from the agent queue", start.name),
                    )
                    .await;
                }
                Err(error) => {
                    self.fail_queue_entry(&start.id, &format!("{error:#}"))
                        .await?
                }
            }
        }
        Ok(caller_admitted)
    }

    /// End starting launches and restores the daemon started whose holder has
    /// been gone for the grace period, whose agent failed to start, so say
    /// so; a launch stays on its space as failed.
    async fn expire_starting_entries(&self) -> Result<()> {
        let held = self.agent_queue.held();
        let cutoff = UnixTime::now() - START_GRACE;
        let expired = self
            .store
            .write(move |tx| expiry::expire_starts(tx, &held, cutoff))
            .await?;
        for failure in expired {
            self.report_expired_start(failure).await;
        }
        Ok(())
    }

    /// Tell of `failure`: a failed launch to its space's starter, a restore
    /// by its space's identity and a launch that recorded no space by name
    /// alone, which no space's name identifies since names repeat.
    async fn report_expired_start(&self, failure: expiry::FailedStart) {
        let kind = AlertKind::QueueFailed;
        match failure {
            expiry::FailedStart::KeptLaunch(space) => {
                self.tell_launch_failed(&space, expiry::ENDED).await;
            }
            expiry::FailedStart::UnopenedLaunch(name) => {
                let reason = "its launch ended without opening the workspace";
                let message = format!("{name} did not start from the agent queue: {reason}");
                self.notify(Some(&name), kind, message).await;
            }
            expiry::FailedStart::UnstartedRestore(name, workspace) => {
                let reason = "its restore ended before the agent started";
                let message = format!("{name} did not start from the agent queue: {reason}");
                self.notify_space(&name, &workspace, kind, message).await;
            }
        }
    }

    /// Restart every starting entry's grace period, since no holder could
    /// reconnect while the daemon was away.
    pub(crate) async fn refresh_starting_entries(&self) -> Result<()> {
        let now = UnixTime::now();
        self.store
            .run(move |db| {
                db.execute(
                    "UPDATE agent_queue SET seen_at=?2 WHERE state=?1",
                    params![QueueState::Starting, now],
                )?;
                Ok(())
            })
            .await
    }
}
