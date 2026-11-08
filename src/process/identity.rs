//! Same-user process ownership for cooperative recovery: identifying,
//! signalling and following processes through the records of the platform
//! back end it selects. Never expose process arguments/environment; only the
//! explicit execution marker, ancestor PIDs, scoped ancestors' state
//! directories and the working directories of a space's untracked processes
//! leave this module.
pub(crate) use crate::client::processes::Identity;
use crate::protocol::timing;
use anyhow::{Context, Result, ensure};
use std::{
    collections::HashSet, ffi::OsStr, os::unix::ffi::OsStrExt, path::PathBuf, time::Duration,
};

#[cfg(test)]
pub use attribution::{OwnedProcess, Unattributed};
pub use attribution::{
    Scan, scan, scan_chat, scan_oneshot_runs, scan_workspaces, stays_unattributed,
};
#[cfg(target_os = "linux")]
pub(super) use linux::{process_record_unavailable, read_process_stat, stat_fields};
#[cfg(target_os = "macos")]
pub(super) use macos::{parent, pid_info};
use marks::left_execution;
pub use marks::{owned_by_workspace, started_with};
pub(crate) use platform::executable;

/// Deeper chains are not plausible process trees; stop rather than loop.
const MAX_ANCESTRY: usize = 64;

/// Operations on the client type [`Identity`] that only the `shoal` command uses.
pub(crate) trait IdentityExt {
    /// The unique ID that live processes' parent links can name: one recorded
    /// in this boot.
    fn lineage_root(&self) -> Option<u64>;

    /// Whether the process ran in an earlier boot, so it has ended. One whose
    /// record does not name its boot may run in this one.
    fn of_earlier_boot(&self) -> bool;

    /// This identity as recorded in `boot`.
    #[cfg(test)]
    fn recorded_in(self, boot: &str) -> Self;
}

impl IdentityExt for Identity {
    fn lineage_root(&self) -> Option<u64> {
        let unique = self.unique.as_ref()?;
        (Some(&unique.boot) == platform::boot_session()).then_some(unique.id)
    }

    fn of_earlier_boot(&self) -> bool {
        platform::recorded_boot(self)
            .zip(platform::current_boot())
            .is_some_and(|(then, now)| then != now)
    }

    #[cfg(test)]
    fn recorded_in(self, boot: &str) -> Self {
        platform::recorded_in(self, boot)
    }
}

/// A process's identity and its parent's unique ID, which the kernel keeps
/// after the parent exits and launchd adopts the process, until the process
/// replaces its image.
#[cfg_attr(test, derive(Clone))]
struct Lineage {
    identity: Identity,
    unique: Option<u64>,
    parent: Option<u64>,
    /// Read with the identity, so it names this process's parent.
    parent_pid: u32,
    /// Launchd adopted it when its parent exited, rather than starting it,
    /// or on Linux its parent is the subreaper that adopts orphans.
    adopted: bool,
}

impl Lineage {
    /// Its parent exited and launchd, init or a subreaper adopted it. Linux
    /// cannot tell their own children from adopted ones, so all of them count.
    fn detached(&self) -> bool {
        self.adopted || (platform::INIT_CHILDREN_DETACHED && self.parent_pid == 1)
    }

    /// Count it adopted when its parent is `subreaper`, which takes orphans
    /// in init's place.
    fn note_subreaper(&mut self, subreaper: Option<u32>) {
        self.adopted |= subreaper == Some(self.parent_pid);
    }

    /// Whether `parent`, observed after this process, is the process it was
    /// forked from rather than a later one reusing its PID: that one is born
    /// after this process and has another unique ID.
    fn forked_from(&self, parent: &Lineage) -> bool {
        self.identity.not_older_than(&parent.identity)
            && match (self.parent, parent.unique) {
                (Some(link), Some(unique)) => link == unique,
                _ => true,
            }
    }
}

pub fn capture(pid: u32) -> Result<Option<Identity>> {
    Ok(lineage(pid)?.map(|lineage| lineage.identity))
}

fn lineage(pid: u32) -> Result<Option<Lineage>> {
    ensure!(pid > 1 && pid <= i32::MAX as u32, "invalid process ID");
    platform::lineage(pid)
}

/// This process's ancestors, nearest first, until init or a reparented
/// (daemonized) process ends the chain.
pub fn ancestors() -> impl Iterator<Item = u32> {
    std::iter::successors(Some(std::os::unix::process::parent_id()), |pid| {
        platform::parent(*pid)
    })
    .take_while(|pid| *pid > 1)
    .take(MAX_ANCESTRY)
}

/// State directories of this process's ancestors that started with workspace
/// scope, nearest first. Ancestors keep their initial environment when a
/// descendant clears its own. Unreadable ancestors are skipped, so this is
/// cooperative, not a boundary.
pub fn scoped_ancestor_state_dirs() -> Vec<PathBuf> {
    let token = format!("{}=", crate::env::SCOPE_TOKEN).into_bytes();
    let state = format!("{}=", crate::env::STATE_DIR).into_bytes();
    ancestors()
        .filter_map(|pid| platform::environment(pid).ok())
        .filter(|environment| environment.iter().any(|entry| entry.starts_with(&token)))
        .filter_map(|environment| {
            environment
                .iter()
                .find_map(|entry| entry.strip_prefix(state.as_slice()))
                .map(|dir| PathBuf::from(OsStr::from_bytes(dir)))
        })
        .collect()
}

pub fn alive(identity: &Identity) -> Result<bool> {
    Ok(capture(identity.pid)?.as_ref() == Some(identity))
}

/// The parent of the live process `identity` names, `None` once it has
/// exited.
pub fn parent_of(identity: &Identity) -> Option<u32> {
    if !alive(identity).unwrap_or(false) {
        return None;
    }
    platform::parent(identity.pid)
}

/// The process group of the live process `identity` names, `None` once it
/// has exited.
pub fn group_of(identity: &Identity) -> Option<u32> {
    if !alive(identity).unwrap_or(false) {
        return None;
    }
    let pid = i32::try_from(identity.pid).ok()?;
    // Reading a process group sends no signal.
    let group = unsafe { libc::getpgid(pid) };
    u32::try_from(group).ok()
}

/// Whether the process `identity` names has exited without its parent
/// reaping it yet, so its CPU time has not reached the parent's.
pub fn unreaped(identity: &Identity) -> Result<bool> {
    ensure!(
        identity.pid > 1 && identity.pid <= i32::MAX as u32,
        "invalid process ID"
    );
    platform::unreaped(identity)
}

pub fn signal(identity: &Identity, signal: i32) -> Result<()> {
    ensure!(
        identity.pid != std::process::id(),
        "refusing to signal the daemon itself"
    );
    if alive(identity)? {
        // Signal only the individual PID whose UID and birth time were checked.
        // This is cooperative recovery, not a race-free hostile-process boundary.
        let result = unsafe { libc::kill(identity.pid as i32, signal) };
        if result != 0 {
            let error = std::io::Error::last_os_error();
            ensure!(
                error.raw_os_error() == Some(libc::ESRCH),
                "signal owned process: {error}"
            );
        }
    }
    Ok(())
}

/// Whether `lineage` is detached, or the parent `among` finds for its PID is
/// the process it was forked from and descends from a detached one.
fn descends_from_detached<'a>(
    lineage: &'a Lineage,
    among: impl Fn(u32) -> Option<&'a Lineage>,
) -> bool {
    let mut current = lineage;
    for _ in 0..MAX_ANCESTRY {
        if current.detached() {
            return true;
        }
        match among(current.parent_pid) {
            Some(parent) if current.forked_from(parent) => current = parent,
            _ => return false,
        }
    }
    false
}

/// A live process, with its effective and real user, process group and
/// parent.
struct Listed {
    pid: u32,
    uid: u32,
    real_uid: u32,
    group: u32,
    parent: u32,
}

/// Every live process other than init and the daemon, read from the kernel.
/// Inventories run often, and spawning `ps` for each cost more than all the
/// reads it saves.
fn listed() -> Result<Vec<Listed>> {
    let mut listed = platform::listed()?;
    listed.retain(|process| process.pid > 1 && process.pid != std::process::id());
    Ok(listed)
}
/// Whether the live process `identity` works inside `directory`. One whose
/// working directory cannot be read, or that has exited, does not.
pub fn works_in(identity: &Identity, directory: &std::path::Path) -> Result<bool> {
    let Some(working) = platform::working_directory(identity.pid) else {
        return Ok(false);
    };
    // The PID may have gone to another process since the read.
    Ok(working.starts_with(directory) && alive(identity)?)
}

enum Image {
    Settled,
    /// Replacing its image: the new environment is not published yet. Only
    /// Linux exposes it.
    #[cfg_attr(target_os = "macos", allow(dead_code))]
    Loading,
    /// Exiting or a kernel thread: it runs no more user code.
    Ended,
}

/// How [`Identity`]s compare by age.
pub(crate) trait IdentityAge {
    /// A process born before the wrapper cannot be one of its descendants.
    fn not_older_than(&self, wrapper: &Identity) -> bool;
}

impl IdentityAge for Identity {
    fn not_older_than(&self, wrapper: &Identity) -> bool {
        let Some(own) = self.birth.rsplit_once(':') else {
            return true;
        };
        let Some(other) = wrapper.birth.rsplit_once(':') else {
            return true;
        };
        platform::born_not_before(own, other)
    }
}

/// Wait until no process of process group `group` remains. A leader may
/// exit before its descendants finish their signal handlers.
pub async fn group_exited(group: u32) -> Result<()> {
    while group_alive(group).await? {
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    Ok(())
}

/// Whether a live process of this user remains in process group `group`.
/// Zombies, which can no longer clean up, do not count. It lists processes
/// natively instead of running `ps`, so frequent checks stay cheap.
pub async fn group_alive(group: u32) -> Result<bool> {
    ensure!(
        group > 1 && group <= i32::MAX as u32,
        "invalid process group"
    );
    tokio::task::spawn_blocking(move || {
        // SAFETY: signal 0 only checks whether the group has members it may
        // signal, so a group with none of this user's processes fails.
        if unsafe { libc::killpg(group as i32, 0) } != 0 {
            return Ok(false);
        }
        for pid in platform::group_members(group)? {
            if capture(pid)?.is_some() {
                return Ok(true);
            }
        }
        Ok(false)
    })
    .await
    .context("process group worker failed")?
}

/// The live processes those of `identities` started, directly or through
/// others, followed by the unique IDs of their lineage, so a reused PID
/// never counts and they count after it and those between them exit; none
/// from one whose lineage is unknown. A workspace's process among them, as
/// [`owned_by_workspace`] tells with `wrappers`, is left out with what it
/// started.
pub async fn started_by(identities: &[Identity], wrappers: &[Identity]) -> Result<Vec<Identity>> {
    let roots: HashSet<u64> = identities
        .iter()
        .filter_map(Identity::lineage_root)
        .collect();
    if roots.is_empty() {
        return Ok(Vec::new());
    }
    let rows = same_user_processes().await?;
    let wrappers = wrappers.to_vec();
    tokio::task::spawn_blocking(move || {
        let mut lineages = Vec::new();
        for (pid, _, _) in &rows {
            lineages.extend(lineage(*pid)?);
        }
        descendants_of(roots, lineages, |process| {
            owned_by_workspace(process, &wrappers)
        })
    })
    .await
    .context("process lineage worker failed")?
}

/// A live, identity-verified leader establishes ownership of its group and
/// current descendants (including children that started another session).
/// After it exits, its lineage still establishes ownership of the processes
/// descending from it through live parents, except those below a process
/// whose own environment lacks the marker of `execution`, such as a demo the
/// command started. Without that proof, group members are candidates only and
/// must not be killed.
pub async fn related(
    child: Option<&Identity>,
    group: Option<u32>,
    execution: Option<&str>,
) -> Result<(Vec<Identity>, Vec<Identity>)> {
    let Some(group) = group else {
        return Ok((vec![], vec![]));
    };
    let rows = same_user_processes().await?;
    let live = child.map(alive).transpose()?.unwrap_or(false);
    let mut pids: HashSet<u32> = rows
        .iter()
        .filter(|(_, pgid, _)| *pgid == group)
        .map(|(pid, _, _)| *pid)
        .collect();
    if live {
        pids.insert(child.unwrap().pid);
        pids = descendants(pids, &rows);
    }
    let identities = pids
        .into_iter()
        .map(capture)
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .flatten()
        .collect();
    if live && child.map(alive).transpose()?.unwrap_or(false) {
        return Ok((identities, vec![]));
    }
    let Some(root) = child.and_then(Identity::lineage_root) else {
        return Ok((vec![], identities));
    };
    let execution = execution.map(str::to_owned);
    let lineage = tokio::task::spawn_blocking(move || descended_from(root, &rows, execution))
        .await
        .context("process lineage worker failed")??;
    let candidates = identities
        .into_iter()
        .filter(|identity| !lineage.contains(identity))
        .collect();
    Ok((lineage, candidates))
}

/// Same-user processes other than init and the daemon, as (PID, process
/// group, parent PID).
async fn same_user_processes() -> Result<Vec<(u32, u32, u32)>> {
    tokio::task::spawn_blocking(list_same_user)
        .await
        .context("process listing worker failed")?
}

/// The PIDs of this user's live processes other than init and this one.
pub(super) fn same_user_pids() -> Result<Vec<u32>> {
    Ok(list_same_user()?
        .into_iter()
        .map(|(pid, _, _)| pid)
        .collect())
}

fn list_same_user() -> Result<Vec<(u32, u32, u32)>> {
    let user = unsafe { libc::geteuid() };
    Ok(listed()?
        .into_iter()
        .filter(|process| process.uid == user)
        .map(|process| (process.pid, process.group, process.parent))
        .collect())
}

/// `pids` and the PIDs whose parent links lead to one of them.
fn descendants(mut pids: HashSet<u32>, rows: &[(u32, u32, u32)]) -> HashSet<u32> {
    loop {
        let previous = pids.len();
        for (pid, _, parent) in rows {
            if pids.contains(parent) {
                pids.insert(*pid);
            }
        }
        if pids.len() == previous {
            return pids;
        }
    }
}

/// The live processes whose parent unique IDs lead to the process with
/// unique ID `root`, which may have exited, short of those that left
/// `execution`.
fn descended_from(
    root: u64,
    rows: &[(u32, u32, u32)],
    execution: Option<String>,
) -> Result<Vec<Identity>> {
    let mut lineages = Vec::new();
    for (pid, _, _) in rows {
        lineages.extend(lineage(*pid)?);
    }
    let Some(execution) = execution else {
        return lineage_descendants(root, lineages, |_| Ok(false));
    };
    let deadline = std::time::Instant::now() + timing::PROCESS_SETTLE_TIMEOUT;
    lineage_descendants(root, lineages, |identity| {
        left_execution(identity, &execution, deadline)
    })
}

/// A parent's unique ID is lower than its children's, since the kernel
/// assigns them in order, so one pass in that order finds every descendant.
/// A descendant that `left` names is left out with its own descendants.
fn lineage_descendants(
    root: u64,
    lineages: Vec<Lineage>,
    left: impl FnMut(&Identity) -> Result<bool>,
) -> Result<Vec<Identity>> {
    descendants_of(HashSet::from([root]), lineages, left)
}

/// [`lineage_descendants`] of every one of `roots`.
fn descendants_of(
    roots: HashSet<u64>,
    mut lineages: Vec<Lineage>,
    mut left: impl FnMut(&Identity) -> Result<bool>,
) -> Result<Vec<Identity>> {
    lineages.sort_by_key(|lineage| lineage.unique);
    let mut ancestors = roots;
    let mut found = Vec::new();
    for lineage in lineages {
        let (Some(unique), Some(parent)) = (lineage.unique, lineage.parent) else {
            continue;
        };
        if ancestors.contains(&parent) && !left(&lineage.identity)? {
            ancestors.insert(unique);
            found.push(lineage.identity);
        }
    }
    Ok(found)
}

pub async fn stop_verified(targets: &[Identity]) -> Result<()> {
    for identity in targets {
        signal(identity, libc::SIGTERM)?;
    }
    if targets.is_empty() {
        return Ok(());
    }
    let deadline = tokio::time::Instant::now() + timing::PROCESS_TERM_GRACE;
    while tokio::time::Instant::now() < deadline && targets.iter().any(|p| alive(p).unwrap_or(true))
    {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    for identity in targets {
        signal(identity, libc::SIGKILL)?;
    }
    let deadline = tokio::time::Instant::now() + timing::PROCESS_KILL_WAIT;
    while tokio::time::Instant::now() < deadline && targets.iter().any(|p| alive(p).unwrap_or(true))
    {
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    for identity in targets {
        ensure!(!alive(identity)?, "owned PID {} did not stop", identity.pid);
    }
    Ok(())
}

mod attribution;
mod marks;
mod owners;
pub mod spaces;
#[cfg(test)]
mod tests;

// Each back end implements the same operations on its kernel's process
// records.
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
use linux as platform;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
use macos as platform;
