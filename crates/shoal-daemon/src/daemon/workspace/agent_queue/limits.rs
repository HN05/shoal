//! Admission limits: the machine's and each repository's limits on agents
//! and CI jobs, as configured and as measured memory and disk allow, and
//! which waiting entries fit them beside the slots in use.
use crate::client::repository_ids::RepositoryId;
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
};

use anyhow::Result;

use super::{Grant, slots::Slot};
use crate::client::queue::QueueEntryId;
use crate::{
    config::{RunLimit, overload::Overload},
    daemon::workspace::{
        Manager,
        capacity::{self, Activity, Agents, Unfit},
    },
    model::HeldBy,
    protocol::ConfigTarget,
};

/// The machine's agent limits and each repository's.
#[derive(Debug, Default)]
pub(crate) struct Limits {
    /// The machine's fixed slot count.
    pub global: Option<u32>,
    /// What measured memory and disk leave for an agent's start, under
    /// dynamic admission.
    pub room: Option<capacity::Room>,
    pub repositories: HashMap<RepositoryId, Option<u32>>,
    /// The machine-wide limit free disk space sets while a fixed limit applies.
    pub disk: Option<u32>,
    pub jobs: JobLimits,
}

/// The machine's limits on CI jobs, which agents' limits do not count.
#[derive(Debug, Default)]
pub(crate) struct JobLimits {
    /// The jobs starting or running.
    pub running: u32,
    /// `machine.max_jobs` when fixed.
    pub fixed: Option<u32>,
    /// The limit free disk space sets while `machine.max_jobs` is fixed, as
    /// it does beside a fixed agent limit.
    pub disk: Option<u32>,
    /// What measured memory and disk leave for a job's start, under dynamic
    /// admission.
    pub room: Option<capacity::Room>,
}

impl Limits {
    fn repository(&self, id: &RepositoryId) -> Option<u32> {
        self.repositories.get(id).copied().flatten()
    }

    pub(super) fn apply_to(&self, repository: &RepositoryId) -> bool {
        self.global.is_some() || self.room.is_some() || self.repository(repository).is_some()
    }
}

/// What a waiting entry starts, as admission sees it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Work {
    Launch,
    /// A CI job expected to use `cost`.
    Job {
        cost: capacity::JobCost,
    },
    /// A one-shot run, which the daemon starts without a session; one
    /// protection stopped waits for healthy readings `after_stop`.
    OneShot {
        after_stop: bool,
    },
    /// A stopped agent's restore. `waiting` says its agent waits for it with
    /// every process stopped.
    Restore {
        waiting: bool,
        wake: Wake,
    },
}

/// What a queued restore waits for besides a slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Wake {
    /// Sustained healthy readings, for an agent stopped mid-turn, whose slot
    /// `occupied` still counts.
    Recovery,
    /// An update for an agent stopped between turns, which holds no slot
    /// meanwhile; `due` once one waits.
    Update { due: bool },
    /// Only a free slot, for an agent `shoal queue move` stopped, which
    /// holds none meanwhile.
    Slot,
}

impl Wake {
    /// What admits a restore with this wake: the recovery `epoch` its
    /// healthy readings used up, when it needs them.
    pub(super) fn grant(self, epoch: Option<u64>) -> Option<Grant> {
        match self {
            Wake::Recovery => epoch.map(Grant::Recovery),
            Wake::Update { .. } => Some(Grant::Update),
            Wake::Slot => Some(Grant::Queue),
        }
    }
}

/// A queued entry and the repository it starts an agent in.
#[derive(Clone)]
pub(crate) struct Waiting {
    pub id: QueueEntryId,
    pub repository: RepositoryId,
    pub work: Work,
}

/// What one admission pass decided.
#[derive(Debug, Default)]
pub(crate) struct Admission {
    /// The waiting entries that fit, in queue order.
    pub admitted: Vec<QueueEntryId>,
    /// What held back each other entry the pass could have started.
    pub held: HashMap<QueueEntryId, HeldBy>,
}

impl Admission {
    fn hold(&mut self, entry: &Waiting, held: Unfit) {
        if let Some(held) = held {
            self.held.insert(entry.id.clone(), held);
        }
    }
}

/// The waiting entries that fit, in queue order, and what holds back the
/// others. `occupied` holds one repository ID per used slot. Under fixed
/// limits a launch, like a restore an update woke, needs a free slot and
/// another restore takes back its own; under dynamic admission each needs
/// room for its memory. Those other restores, like one-shot runs protection
/// stopped, start only on sustained healthy readings (`recovered`), one per
/// healthy interval. A CI job counts only against the jobs' limits, and under
/// dynamic admission needs room for its own memory and takes the pass's one
/// start, as an agent does. An entry that waits or does not fit keeps its
/// place and is passed over.
pub(crate) fn admit(
    occupied: &[RepositoryId],
    waiting: &[Waiting],
    limits: &Limits,
    mut recovered: bool,
) -> Admission {
    let mut total = occupied.len();
    // Whether an agent or a job was admitted this pass, by measured memory
    // or a fixed limit: measured memory admits nothing beside it, since the
    // sample predates it.
    let mut started = false;
    let mut used: HashMap<&str, usize> = HashMap::new();
    for repository in occupied {
        *used.entry(repository.as_str()).or_default() += 1;
    }
    let mut jobs = limits.jobs.running;
    let mut admission = Admission::default();
    for entry in waiting {
        if let Work::Job { cost } = entry.work {
            if let Some(held) = full_job_limit(&limits.jobs, jobs) {
                admission.hold(entry, Some(held));
                continue;
            }
            if let Err(held) = measured_fit(limits.jobs.room, started, |room| room.fits_job(cost)) {
                admission.hold(entry, held);
                continue;
            }
            started = true;
            jobs += 1;
            admission.admitted.push(entry.id.clone());
            continue;
        }
        let own = match entry.work {
            // Admitted above, under the jobs' own limits.
            Work::Job { .. } => continue,
            Work::OneShot { after_stop: true } if !recovered => continue,
            Work::Launch | Work::OneShot { .. } => 0,
            Work::Restore {
                waiting: true,
                wake: Wake::Slot | Wake::Update { due: true },
            } => 0,
            Work::Restore {
                wake: Wake::Slot | Wake::Update { .. },
                ..
            } => continue,
            Work::Restore { .. } if !recovered => continue,
            Work::Restore { waiting: false, .. } => continue,
            Work::Restore { waiting: true, .. } => 1,
        };
        if let Err(held) = measured_fit(limits.room, started, capacity::Room::fits_agent) {
            admission.hold(entry, held);
            continue;
        }
        let used = used.entry(entry.repository.as_str()).or_default();
        if let Some(held) = full_limit(limits, &entry.repository, total, *used, own) {
            admission.hold(entry, Some(held));
            continue;
        }
        *used += 1 - own;
        total += 1 - own;
        started = true;
        // Each healthy interval restarts one stopped agent or one-shot run.
        recovered &= own == 0 && entry.work != Work::OneShot { after_stop: true };
        admission.admitted.push(entry.id.clone());
    }
    admission
}

/// Whether a start fits what measured memory and disk leave, when `room`
/// says so under dynamic admission: none does beside one this pass
/// `started` already.
fn measured_fit(
    room: Option<capacity::Room>,
    started: bool,
    fits: impl FnOnce(capacity::Room) -> Result<(), Unfit>,
) -> Result<(), Unfit> {
    match room {
        None => Ok(()),
        Some(_) if started => Err(None),
        Some(room) => fits(room),
    }
}

/// The fixed limit a job's start beside `running` jobs would exceed.
fn full_job_limit(limits: &JobLimits, running: u32) -> Option<HeldBy> {
    let full = |limit: Option<u32>| limit.is_some_and(|limit| running >= limit);
    if full(limits.fixed) {
        Some(HeldBy::MaxJobs)
    } else if full(limits.disk) {
        Some(HeldBy::Disk)
    } else {
        None
    }
}

/// The fixed limit an agent's start in `repository` would exceed, with
/// `total` slots used on the machine and `used` in the repository, `own` of
/// them its own already.
fn full_limit(
    limits: &Limits,
    repository: &RepositoryId,
    total: usize,
    used: usize,
    own: usize,
) -> Option<HeldBy> {
    let full = |count: usize, limit: Option<u32>| {
        limit.is_some_and(|limit| count.saturating_sub(own) >= limit as usize)
    };
    if full(total, limits.global) {
        Some(HeldBy::MaxAgents)
    } else if full(total, limits.disk) {
        Some(HeldBy::Disk)
    } else if full(used, limits.repository(repository)) {
        Some(HeldBy::RepositoryLimit)
    } else {
        None
    }
}

/// Whether `limits` admit some job beside the `occupied` slots: one under
/// any fixed limit, and under dynamic admission one that needs no more than
/// whatever room is left, since a waiting job's cost may be that small.
pub(crate) fn room_for_a_job(occupied: &[RepositoryId], limits: &Limits) -> bool {
    let probe = [Waiting {
        id: QueueEntryId::from(String::new()),
        repository: String::new().into(),
        work: Work::Job {
            cost: capacity::JobCost { memory: Some(0) },
        },
    }];
    !admit(occupied, &probe, limits, false).admitted.is_empty()
}

/// A configured value as a slot count. Dynamic admission measures the whole
/// machine, so it adds no repository limit of its own.
fn fixed_limit(limit: RunLimit) -> Option<u32> {
    match limit {
        RunLimit::Fixed(value) => Some(value.get()),
        RunLimit::Dynamic | RunLimit::Unlimited => None,
    }
}

impl Manager {
    /// The global limits given the agents' `slots` and the `jobs` in use,
    /// each named repository's effective limit, and the disk limit when any
    /// of them is fixed. A repository without its own value inherits the
    /// global one, which the machine limit already covers.
    pub(super) async fn agent_limits(
        &self,
        repositories: Vec<RepositoryId>,
        slots: &[Slot],
        jobs: &[Activity],
    ) -> Result<Limits> {
        let config = self.config();
        let overload = &config.machine.overload;
        let global = config.machine.max_agents;
        let max_jobs = config.machine.max_jobs;
        let activities: Vec<Activity> = slots
            .iter()
            .map(|slot| slot.activity)
            .chain(jobs.iter().copied())
            .collect();
        let measured = match [global, max_jobs].contains(&RunLimit::Dynamic) {
            true => Some(self.measure(overload, &activities).await?),
            false => None,
        };
        let mut limits = Limits {
            global: fixed_limit(global),
            room: measured.filter(|_| global == RunLimit::Dynamic),
            repositories: HashMap::new(),
            disk: None,
            jobs: JobLimits {
                running: jobs.len().try_into().unwrap_or(u32::MAX),
                fixed: fixed_limit(max_jobs),
                disk: None,
                room: measured.filter(|_| max_jobs == RunLimit::Dynamic),
            },
        };
        for id in repositories {
            if limits.repositories.contains_key(&id) {
                continue;
            }
            let layers = self
                .config_layers_for(ConfigTarget::Repository(id.clone().into()))
                .await?;
            let resolved = config.resolve(&layers)?;
            limits
                .repositories
                .insert(id, fixed_limit(resolved.agent.max_running));
        }
        if fixed_limit(global).is_some() || limits.repositories.values().any(Option::is_some) {
            limits.disk = self.disk_limit(overload, slots.len()).await?;
        }
        if limits.jobs.fixed.is_some() {
            limits.jobs.disk = self.disk_limit(overload, jobs.len()).await?;
        }
        Ok(limits)
    }

    /// Whether some CI job could start now beside the agents and jobs in
    /// use; the forge's waiting jobs are read only then.
    pub(crate) async fn job_could_start(&self) -> Result<bool> {
        let any_runner = self
            .store
            .run(|db| crate::daemon::store::exists(db, "SELECT 1 FROM runners", []))
            .await?;
        if !any_runner {
            return Ok(false);
        }
        let slots = self.occupied_slots(&HashSet::new()).await?;
        let jobs = self.job_activities().await?;
        let limits = self.agent_limits(Vec::new(), &slots, &jobs).await?;
        let occupied: Vec<RepositoryId> = slots.into_iter().map(|slot| slot.repository).collect();
        Ok(room_for_a_job(&occupied, &limits))
    }

    /// What the machine's memory and disk leave beside the slots
    /// `activities` use.
    async fn measure(
        &self,
        overload: &Overload,
        activities: &[Activity],
    ) -> Result<capacity::Room> {
        let paths = self.admission_paths().await?;
        let paths: Vec<&Path> = paths.iter().map(PathBuf::as_path).collect();
        Ok(capacity::room(
            capacity::sample(&paths, overload),
            Agents::count(activities.iter().copied()),
            overload,
        ))
    }

    /// The limit free disk space sets for `used` slots beside a fixed limit.
    async fn disk_limit(&self, overload: &Overload, used: usize) -> Result<Option<u32>> {
        let paths = self.admission_paths().await?;
        let paths: Vec<&Path> = paths.iter().map(PathBuf::as_path).collect();
        Ok(capacity::disk_limit(
            capacity::disk_free(&paths, overload),
            used.try_into().unwrap_or(u32::MAX),
            overload,
        ))
    }

    /// The paths whose free space admission reads: daemon state first, then
    /// every workspace.
    async fn admission_paths(&self) -> Result<Vec<PathBuf>> {
        Ok(std::iter::once(self.paths.state.clone())
            .chain(
                self.list_workspaces()
                    .await?
                    .into_iter()
                    .map(|workspace| workspace.path),
            )
            .collect())
    }
}

#[cfg(test)]
mod tests;
