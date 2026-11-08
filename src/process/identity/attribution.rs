//! Attributing every process this user runs to the execution, workspace,
//! chat or one-shot run that owns it, by its own environment, its
//! ancestors' or what earlier inventories remember.
use super::{
    Identity, Lineage, MAX_ANCESTRY, alive, listed,
    marks::{Evidence, Marker, Marks, Observed, observe, observe_elevated},
    owners, platform,
};
use crate::{client::space_ids::SpaceId, protocol::timing};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    time::Duration,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OwnedProcess {
    pub execution_id: String,
    pub identity: Identity,
}
#[derive(Default)]
pub struct Scan {
    pub processes: Vec<OwnedProcess>,
    pub unreadable: Vec<Unattributed>,
}

/// A live process no environment or ancestor attributes.
#[derive(Debug, Clone)]
pub struct Unattributed {
    pub identity: Identity,
    /// The oldest process of its unattributed ancestry, possibly exited; no
    /// execution started after it owns any of them.
    pub head: Identity,
}

#[cfg(test)]
impl Unattributed {
    pub fn alone(identity: Identity) -> Self {
        Self {
            head: identity.clone(),
            identity,
        }
    }
}

/// Live same-user processes carrying one of the execution markers `ids`.
pub async fn scan(ids: HashSet<String>) -> Result<Scan> {
    scan_marked(Marker::Execution, ids).await
}

/// Live same-user processes carrying the identity of chat `id`: its agent
/// and what the agent started with its environment.
pub async fn scan_chat(id: &SpaceId) -> Result<Vec<Identity>> {
    let scan = scan_marked(Marker::Chat, HashSet::from([id.to_string()])).await?;
    Ok(scan.processes.into_iter().map(|p| p.identity).collect())
}

/// Live same-user processes carrying the identity of one of the workspaces
/// `ids`: their tracked executions, hooks and processes started with their
/// exported environment, each paired with the workspace it carries.
/// Unreadable environments cannot be attributed and are left out.
pub async fn scan_workspaces(ids: HashSet<SpaceId>) -> Result<Vec<(SpaceId, Identity)>> {
    let ids = ids.into_iter().map(String::from).collect();
    let scan = scan_marked(Marker::Workspace, ids).await?;
    Ok(scan
        .processes
        .into_iter()
        .map(|p| (SpaceId::from(p.execution_id), p.identity))
        .collect())
}

/// Live same-user processes of the one-shot runs whose run directories are
/// `runs`, wherever they work: those started with a run's marker and what
/// they started.
pub async fn scan_oneshot_runs(runs: HashSet<String>) -> Result<Vec<Identity>> {
    let scan = scan_marked(Marker::OneShot, runs).await?;
    Ok(scan.processes.into_iter().map(|p| p.identity).collect())
}

/// Inventory processes whose environment sets `marker` to one of `ids`; each
/// match records the value it carries.
async fn scan_marked(marker: Marker, ids: HashSet<String>) -> Result<Scan> {
    scan_remembering(&owners::OWNERS, marker, ids).await
}

/// `scan_marked`, consulting and updating `owners`.
pub(super) async fn scan_remembering(
    owners: &'static owners::Owners,
    marker: Marker,
    ids: HashSet<String>,
) -> Result<Scan> {
    if ids.is_empty() {
        return Ok(Scan::default());
    }
    inventory(owners, move |inventory| inventory.attribute(marker, &ids)).await
}

/// Observe every process this user runs, consulting and updating `owners`,
/// and `read` the inventory off the async runtime.
pub(super) async fn inventory<T: Send + 'static>(
    owners: &'static owners::Owners,
    read: impl FnOnce(&Inventory<'static>) -> Result<T> + Send + 'static,
) -> Result<T> {
    let turn = owners::TURN.lock().await;
    let subreaper = platform::subreaper().await?;
    tokio::task::spawn_blocking(move || {
        let _turn = turn;
        let listing = list_processes().context("process inventory failed")?;
        let settle_deadline = std::time::Instant::now() + timing::PROCESS_SETTLE_TIMEOUT;
        let mut inventory = Inventory::new(owners);
        for pid in listing.own {
            if let Some(mut process) = observe(pid, settle_deadline)? {
                process.lineage.note_subreaper(subreaper);
                inventory.observed.insert(pid, process);
            }
        }
        for pid in listing.elevated {
            if let Some(mut process) = observe_elevated(pid)? {
                process.lineage.note_subreaper(subreaper);
                inventory.observed.insert(pid, process);
            }
        }
        read(&inventory)
    })
    .await
    .context("process inventory worker failed")?
}

/// The processes an inventory observes, other than init and the daemon.
pub(super) struct Listing {
    /// Processes running as this user.
    pub(super) own: Vec<u32>,
    /// Setuid programs this user started, such as `login`, which run as
    /// another user.
    elevated: Vec<u32>,
}

pub(super) fn list_processes() -> Result<Listing> {
    let user = unsafe { libc::geteuid() };
    let mut listing = Listing {
        own: Vec::new(),
        elevated: Vec::new(),
    };
    for process in listed()? {
        if process.uid == user {
            listing.own.push(process.pid);
        } else if process.real_uid == user {
            listing.elevated.push(process.pid);
        }
    }
    Ok(listing)
}

/// Whether the unattributed process `identity` still keeps the ownership of
/// `execution` from being proven at `deadline`. A short-lived process launchd
/// adopted while its environment was hidden, such as a detached helper still
/// running a platform binary before it execs its own program, exits or shows
/// an environment without the execution's marker by then.
pub async fn stays_unattributed(
    identity: &Identity,
    execution: &str,
    deadline: std::time::Instant,
) -> Result<bool> {
    let identity = identity.clone();
    let execution = execution.to_owned();
    tokio::task::spawn_blocking(move || {
        loop {
            let evidence = observe(identity.pid, deadline)?
                .filter(|observed| observed.lineage.identity == identity)
                .map(|observed| observed.evidence);
            match evidence {
                None | Some(Evidence::Unmarked) => return Ok(false),
                Some(Evidence::Marked(marks)) => {
                    return Ok(marks.get(Marker::Execution) == Some(execution.as_str()));
                }
                _ if std::time::Instant::now() >= deadline => return Ok(true),
                _ => std::thread::sleep(Duration::from_millis(10)),
            }
        }
    })
    .await
    .context("process settle worker failed")?
}

#[derive(Clone)]
pub(super) enum Attribution {
    Owned(Marks),
    Foreign,
    /// What an earlier inventory told about it or its exited parent.
    Remembered(owners::Evidence),
    /// Its owner cannot be told; the identity heads the processes it covers.
    Unknown(Identity),
}

/// The inventoried processes still alive to observe, with the owners earlier
/// inventories remember.
pub(super) struct Inventory<'a> {
    pub(super) observed: HashMap<u32, Observed>,
    pub(super) owners: &'a owners::Owners,
}

/// Signalling is refused only for a live process of another user.
fn another_users(pid: u32) -> bool {
    let result = unsafe { libc::kill(pid as i32, 0) };
    result != 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

impl<'a> Inventory<'a> {
    pub(super) fn new(owners: &'a owners::Owners) -> Self {
        Self {
            observed: HashMap::new(),
            owners,
        }
    }

    /// Collect the scan of the observed processes whose `marker` is one of
    /// `ids`, checking that owned processes still live.
    pub(super) fn attribute(&self, marker: Marker, ids: &HashSet<String>) -> Result<Scan> {
        let mut scan = Scan::default();
        for (pid, evidence) in self.attribute_all() {
            let identity = &self.observed[&pid].lineage.identity;
            let evidence = match evidence {
                Ok(evidence) => evidence,
                Err(head) => {
                    scan.unreadable.push(Unattributed {
                        identity: identity.clone(),
                        head,
                    });
                    continue;
                }
            };
            if let owners::Told::Owner(marks) = &evidence.told
                && let Some(id) = marks.get(marker).filter(|id| ids.contains(*id))
                && alive(identity)?
            {
                scan.processes.push(OwnedProcess {
                    execution_id: id.to_owned(),
                    identity: identity.clone(),
                });
            }
        }
        Ok(scan)
    }

    /// What tells the owner of each observed process this user runs itself,
    /// or the head of the processes whose owner cannot be told. Later
    /// inventories remember what this one told about each process.
    pub(super) fn attribute_all(&self) -> HashMap<u32, Result<owners::Evidence, Identity>> {
        let mut attributions = HashMap::new();
        let mut found = HashMap::new();
        let mut told = Vec::new();
        for (pid, process) in &self.observed {
            if matches!(process.evidence, Evidence::Elevated) {
                continue;
            }
            let evidence = self.evidence(self.resolve(*pid, &mut attributions), process);
            if let (Ok(evidence), Some(unique)) = (&evidence, process.lineage.unique) {
                told.push((unique, evidence.clone()));
            }
            found.insert(*pid, evidence);
        }
        self.owners.update(told, &self.named());
        found
    }

    /// What `attribution` tells about the owner of `process`, or the head of
    /// the processes it leaves unknown.
    fn evidence(
        &self,
        attribution: Attribution,
        process: &Observed,
    ) -> Result<owners::Evidence, Identity> {
        let told = match attribution {
            Attribution::Remembered(evidence) => evidence.told,
            Attribution::Unknown(head) => return Err(head),
            Attribution::Owned(marks) => owners::Told::Owner(marks),
            Attribution::Foreign => owners::Told::NoExecution,
        };
        Ok(owners::Evidence {
            told,
            direct: matches!(process.evidence, Evidence::Marked(_) | Evidence::Unmarked),
        })
    }

    /// What an earlier inventory read from `lineage`'s own environment,
    /// which outranks its ancestors' while it hides its environment.
    fn read_before(&self, lineage: &Lineage) -> Option<Attribution> {
        let evidence = self.owners.recall(lineage.unique?)?;
        evidence.direct.then_some(Attribution::Remembered(evidence))
    }

    /// The unique IDs observed processes name as their own or their parent's.
    fn named(&self) -> HashSet<u64> {
        self.observed
            .values()
            .flat_map(|process| [process.lineage.unique, process.lineage.parent])
            .flatten()
            .collect()
    }

    /// What an earlier inventory told about `lineage`, or failing that about
    /// its exited parent, otherwise unknown under it. A process told to be no
    /// execution's stays so whatever its parent was.
    fn remembered(&self, lineage: &Lineage) -> Attribution {
        [
            lineage.unique,
            lineage.parent.filter(|_| !self.parent_observed(lineage)),
        ]
        .into_iter()
        .flatten()
        .find_map(|unique| self.owners.recall(unique))
        .map_or_else(
            || Attribution::Unknown(lineage.identity.clone()),
            Attribution::Remembered,
        )
    }

    /// Whether this inventory observed `lineage`'s parent itself, whose own
    /// evidence now outranks what earlier inventories told about it.
    fn parent_observed(&self, lineage: &Lineage) -> bool {
        self.observed
            .get(&lineage.parent_pid)
            .is_some_and(|parent| parent.lineage.unique == lineage.parent)
    }

    /// `pid`'s owner by its own environment, or for a hidden one by the
    /// nearest ancestor whose environment tells, as its environment was
    /// inherited from there through any setuid programs between them. A
    /// hidden process launchd adopted has no ancestor left to tell, nor one
    /// whose parent exited otherwise; for them, and for an unreadable one,
    /// earlier inventories may remember the owner.
    pub(super) fn resolve(
        &self,
        pid: u32,
        attributions: &mut HashMap<u32, Attribution>,
    ) -> Attribution {
        let mut hidden: Vec<&Observed> = Vec::new();
        let mut current = pid;
        let attribution = loop {
            let observed = self.observed.get(&current);
            if let (Some(child), Some(parent)) = (hidden.last(), observed)
                && !child.lineage.forked_from(&parent.lineage)
            {
                // The parent exited and its PID was reused.
                break self.remembered(&child.lineage);
            }
            if let Some(attribution) = attributions.get(&current) {
                break attribution.clone();
            }
            let Some(process) = observed else {
                break self.unobserved_parent(current, hidden.last().copied());
            };
            let lineage = &process.lineage;
            match &process.evidence {
                Evidence::Marked(marks) => break Attribution::Owned(marks.clone()),
                Evidence::Unmarked => break Attribution::Foreign,
                Evidence::Unreadable => break self.remembered(lineage),
                Evidence::Hidden => {
                    hidden.push(process);
                    if let Some(attribution) = self.read_before(lineage) {
                        break attribution;
                    }
                }
                Evidence::Elevated => hidden.push(process),
            }
            if lineage.parent_pid <= 1 {
                break if lineage.adopted {
                    self.remembered(lineage)
                } else {
                    Attribution::Foreign
                };
            }
            if hidden.len() > MAX_ANCESTRY {
                break Attribution::Unknown(lineage.identity.clone());
            }
            current = lineage.parent_pid;
        };
        // A process decided by its own environment needs no walk to decide again.
        for process in hidden {
            attributions.insert(process.lineage.identity.pid, attribution.clone());
        }
        attribution
    }

    /// A parent that is the daemon or belongs to another user carries no
    /// marker it did not start with. Any other parent left unobserved has
    /// exited or escaped the inventory and leaves `child` without an ancestor
    /// to tell.
    fn unobserved_parent(&self, parent: u32, child: Option<&Observed>) -> Attribution {
        match child {
            Some(child) if parent != std::process::id() && !another_users(parent) => {
                self.remembered(&child.lineage)
            }
            _ => Attribution::Foreign,
        }
    }
}
