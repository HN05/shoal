//! Same-user process ownership for cooperative recovery. Never expose process
//! arguments/environment; only the explicit execution marker, ancestor PIDs
//! and scoped ancestors' state directories leave this module.
use crate::protocol::timing;
use crate::tools::Tool;
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    ffi::OsStr,
    os::unix::ffi::OsStrExt,
    path::PathBuf,
    time::Duration,
};

/// Deeper chains are not plausible process trees; stop rather than loop.
const MAX_ANCESTRY: usize = 64;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Identity {
    pub pid: u32,
    pub birth: String,
    /// The kernel's never-reused process ID where the platform reports one,
    /// so the process's descendants stay attributable after it exits.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unique: Option<UniqueId>,
}

/// Unique process IDs start over at each boot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UniqueId {
    pub boot: String,
    pub id: u64,
}

impl Identity {
    /// The unique ID that live processes' parent links can name: one recorded
    /// in this boot.
    fn lineage_root(&self) -> Option<u64> {
        let unique = self.unique.as_ref()?;
        (Some(&unique.boot) == boot_session()).then_some(unique.id)
    }

    /// Whether the process ran in an earlier boot, so it has ended. One whose
    /// record does not name its boot may run in this one.
    pub fn of_earlier_boot(&self) -> bool {
        recorded_boot(self)
            .zip(current_boot())
            .is_some_and(|(then, now)| then != now)
    }

    /// This identity as recorded in `boot`.
    #[cfg(test)]
    pub(crate) fn recorded_in(self, boot: &str) -> Self {
        #[cfg(target_os = "linux")]
        let identity = {
            let start = self.birth.split_once(':').map_or("", |(_, start)| start);
            Self {
                birth: format!("{boot}:{start}"),
                ..self
            }
        };
        #[cfg(target_os = "macos")]
        let identity = {
            let id = self.unique.as_ref().map_or(1, |unique| unique.id);
            Self {
                unique: Some(UniqueId {
                    boot: boot.into(),
                    id,
                }),
                ..self
            }
        };
        identity
    }
}

/// PID and birth name the process; records made before `unique` existed
/// lack it.
impl PartialEq for Identity {
    fn eq(&self, other: &Self) -> bool {
        self.pid == other.pid && self.birth == other.birth
    }
}

impl Eq for Identity {}

impl std::hash::Hash for Identity {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.pid.hash(state);
        self.birth.hash(state);
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
    /// Launchd adopted it when its parent exited, rather than starting it.
    adopted: bool,
}

impl Lineage {
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
    platform_lineage(pid)
}

/// This process's ancestors, nearest first, until init or a reparented
/// (daemonized) process ends the chain.
pub fn ancestors() -> impl Iterator<Item = u32> {
    std::iter::successors(Some(std::os::unix::process::parent_id()), |pid| {
        parent(*pid)
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
        .filter_map(|pid| environment(pid).ok())
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
    platform_unreaped(identity)
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
    scan_marked(crate::env::EXECUTION_ID, ids).await
}

/// Live same-user processes carrying the workspace's identity: its tracked
/// executions, hooks and processes started with its exported environment.
/// Unreadable environments cannot be attributed and are left out.
pub async fn scan_workspace(id: &str) -> Result<Vec<Identity>> {
    let scan = scan_marked(crate::env::WORKSPACE_ID, HashSet::from([id.to_owned()])).await?;
    Ok(scan.processes.into_iter().map(|p| p.identity).collect())
}

/// `scan_workspace` for each of the workspaces `ids`, pairing every process
/// with the workspace it carries.
pub async fn scan_workspaces(ids: HashSet<String>) -> Result<Vec<(String, Identity)>> {
    let scan = scan_marked(crate::env::WORKSPACE_ID, ids).await?;
    Ok(scan
        .processes
        .into_iter()
        .map(|p| (p.execution_id, p.identity))
        .collect())
}

/// Inventory processes whose environment sets `marker` to one of `ids`; each
/// match records the value it carries.
async fn scan_marked(marker: &str, ids: HashSet<String>) -> Result<Scan> {
    if ids.is_empty() {
        return Ok(Scan::default());
    }
    let listing = list_processes().await?;
    let marker = format!("{marker}=").into_bytes();
    tokio::task::spawn_blocking(move || {
        let settle_deadline = std::time::Instant::now() + timing::PROCESS_SETTLE_TIMEOUT;
        let mut inventory = Inventory {
            observed: HashMap::new(),
        };
        for pid in listing.own {
            if let Some(process) = observe(pid, &marker, &ids, settle_deadline)? {
                inventory.observed.insert(pid, process);
            }
        }
        for pid in listing.elevated {
            if let Some(process) = observe_elevated(pid)? {
                inventory.observed.insert(pid, process);
            }
        }
        inventory.attribute()
    })
    .await
    .context("process inventory worker failed")?
}

/// The processes an inventory observes, other than init and the daemon.
struct Listing {
    /// Processes running as this user.
    own: Vec<u32>,
    /// Setuid programs this user started, such as `login`, which run as
    /// another user.
    elevated: Vec<u32>,
}

async fn list_processes() -> Result<Listing> {
    let mut command = tokio::process::Command::new(Tool::Ps.program());
    command
        .args(["-ax", "-o", "pid=,uid=,ruid="])
        .env("LC_ALL", "C");
    let output = crate::subprocess::Run::new(command)
        .timeout(timing::PROCESS_INVENTORY_TIMEOUT)
        .output()
        .await
        .context("process inventory failed")?;
    let user = unsafe { libc::geteuid() };
    let mut listing = Listing {
        own: Vec::new(),
        elevated: Vec::new(),
    };
    for line in output.lines() {
        let fields: Vec<_> = line.split_whitespace().collect();
        ensure!(fields.len() == 3, "invalid process inventory");
        let pid: u32 = fields[0].parse()?;
        if pid <= 1 || pid == std::process::id() {
            continue;
        }
        // System daemons may run as negative UIDs such as nobody (-2);
        // they are never ours.
        let uid = fields[1].parse::<u32>().ok();
        let real = fields[2].parse::<u32>().ok();
        if uid == Some(user) {
            listing.own.push(pid);
        } else if real == Some(user) {
            listing.elevated.push(pid);
        }
    }
    Ok(listing)
}

/// One live process and what its own environment says about its owner.
struct Observed {
    lineage: Lineage,
    evidence: Evidence,
}

enum Evidence {
    Marked(String),
    Unmarked,
    Unreadable,
    /// Settled empty, which a platform binary's hidden environment also reads as.
    Hidden,
    /// A setuid program this user started, whose environment came from the
    /// parent that started it. It runs as another user, so it is never
    /// reported or stopped itself.
    Elevated,
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
    let marker = format!("{}=", crate::env::EXECUTION_ID).into_bytes();
    let ids = HashSet::from([execution.to_owned()]);
    tokio::task::spawn_blocking(move || {
        loop {
            let evidence = observe(identity.pid, &marker, &ids, deadline)?
                .filter(|observed| observed.lineage.identity == identity)
                .map(|observed| observed.evidence);
            match evidence {
                None | Some(Evidence::Unmarked) => return Ok(false),
                Some(Evidence::Marked(_)) => return Ok(true),
                _ if std::time::Instant::now() >= deadline => return Ok(true),
                _ => std::thread::sleep(Duration::from_millis(10)),
            }
        }
    })
    .await
    .context("process settle worker failed")?
}

/// Whether `identity` was started with `variable` set. An environment that
/// cannot be read counts as without it.
pub fn started_with(identity: &Identity, variable: &str) -> bool {
    let prefix = format!("{variable}=");
    let deadline = std::time::Instant::now() + timing::PROCESS_SETTLE_TIMEOUT;
    matches!(
        visibility(identity, deadline),
        Ok(Visibility::Environment(environment))
            if environment.iter().any(|entry| entry.starts_with(prefix.as_bytes()))
    )
}

/// Read `pid`'s environment for `marker`, unless it has exited.
fn observe(
    pid: u32,
    marker: &[u8],
    ids: &HashSet<String>,
    deadline: std::time::Instant,
) -> Result<Option<Observed>> {
    let Some(lineage) = lineage(pid)? else {
        return Ok(None);
    };
    let evidence = match visibility(&lineage.identity, deadline)? {
        Visibility::Environment(environment)
            if environment.is_empty() && hides_environment(pid) =>
        {
            Evidence::Hidden
        }
        Visibility::Environment(environment) => environment
            .iter()
            .find_map(|entry| entry.strip_prefix(marker))
            .and_then(|id| std::str::from_utf8(id).ok())
            .filter(|id| ids.contains(*id))
            .map_or(Evidence::Unmarked, |id| Evidence::Marked(id.into())),
        Visibility::Unreadable => Evidence::Unreadable,
        Visibility::Gone => return Ok(None),
    };
    Ok(Some(Observed { lineage, evidence }))
}

/// Observe `pid`, a setuid program this user started, unless it has exited.
fn observe_elevated(pid: u32) -> Result<Option<Observed>> {
    Ok(elevated_lineage(pid)?.map(|lineage| Observed {
        lineage,
        evidence: Evidence::Elevated,
    }))
}

#[derive(Clone)]
enum Attribution {
    Owned(String),
    Foreign,
    /// Its owner cannot be told; the identity heads the processes it covers.
    Unknown(Identity),
}

/// The inventoried processes still alive to observe.
struct Inventory {
    observed: HashMap<u32, Observed>,
}

/// Signalling is refused only for a live process of another user.
fn another_users(pid: u32) -> bool {
    let result = unsafe { libc::kill(pid as i32, 0) };
    result != 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

impl Inventory {
    /// Attribute every observed process and collect the scan, checking that
    /// owned processes still live.
    fn attribute(&self) -> Result<Scan> {
        let mut attributions = HashMap::new();
        let mut scan = Scan::default();
        for (pid, process) in &self.observed {
            if matches!(process.evidence, Evidence::Elevated) {
                continue;
            }
            let identity = &process.lineage.identity;
            match self.resolve(*pid, &mut attributions) {
                Attribution::Owned(execution_id) if alive(identity)? => {
                    scan.processes.push(OwnedProcess {
                        execution_id,
                        identity: identity.clone(),
                    });
                }
                Attribution::Unknown(head) => scan.unreadable.push(Unattributed {
                    identity: identity.clone(),
                    head,
                }),
                _ => {}
            }
        }
        Ok(scan)
    }

    /// `pid`'s owner by its own environment, or for a hidden one by the
    /// nearest ancestor whose environment tells, as its environment was
    /// inherited from there through any setuid programs between them. A
    /// hidden process launchd adopted has no ancestor left to tell.
    fn resolve(&self, pid: u32, attributions: &mut HashMap<u32, Attribution>) -> Attribution {
        let mut hidden: Vec<&Observed> = Vec::new();
        let mut current = pid;
        let attribution = loop {
            let observed = self.observed.get(&current);
            if let (Some(child), Some(parent)) = (hidden.last(), observed)
                && !child.lineage.forked_from(&parent.lineage)
            {
                // The parent exited and its PID was reused.
                break Attribution::Unknown(child.lineage.identity.clone());
            }
            if let Some(attribution) = attributions.get(&current) {
                break attribution.clone();
            }
            let Some(process) = observed else {
                break self.unobserved_parent(current, hidden.last().copied());
            };
            let lineage = &process.lineage;
            match &process.evidence {
                Evidence::Marked(id) => break Attribution::Owned(id.clone()),
                Evidence::Unmarked => break Attribution::Foreign,
                Evidence::Unreadable => break Attribution::Unknown(lineage.identity.clone()),
                Evidence::Hidden | Evidence::Elevated => hidden.push(process),
            }
            if lineage.parent_pid <= 1 {
                break if lineage.adopted {
                    Attribution::Unknown(lineage.identity.clone())
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
                Attribution::Unknown(child.lineage.identity.clone())
            }
            _ => Attribution::Foreign,
        }
    }
}

enum Visibility {
    Environment(Vec<Vec<u8>>),
    Unreadable,
    Gone,
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

/// macOS refuses the arguments of a process whose exec has not published them
/// yet, often right after reading them as empty; Linux reads them as empty
/// instead.
const REFUSED_WHILE_LOADING: bool = cfg!(target_os = "macos");

/// Linux reads an empty environment while a process replaces its image and
/// after an exiting process releases its memory; neither is evidence about
/// ownership. Wait for loading images, then confirm a settled empty read.
fn visibility(identity: &Identity, deadline: std::time::Instant) -> Result<Visibility> {
    visibility_with(identity, deadline, environment)
}

/// `visibility`, reading environments with `environment`.
fn visibility_with(
    identity: &Identity,
    deadline: std::time::Instant,
    mut environment: impl FnMut(u32) -> Result<Vec<Vec<u8>>>,
) -> Result<Visibility> {
    loop {
        let read = environment(identity.pid);
        if !alive(identity)? {
            return Ok(Visibility::Gone);
        }
        match read {
            Ok(environment) if !environment.is_empty() => {
                return Ok(Visibility::Environment(environment));
            }
            // Without CAP_SYS_PTRACE, Linux refuses an exiting process's
            // environment. A refusal also hides env_end, so never wait on it.
            Err(_) => match image(identity.pid)? {
                Image::Ended => return Ok(Visibility::Gone),
                _ if REFUSED_WHILE_LOADING && std::time::Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(1));
                    continue;
                }
                _ => return Ok(Visibility::Unreadable),
            },
            Ok(_) => {}
        }
        match image(identity.pid)? {
            Image::Ended => return Ok(Visibility::Gone),
            Image::Loading if std::time::Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(1));
            }
            Image::Loading => return Ok(Visibility::Unreadable),
            // The image may have settled after the empty read.
            Image::Settled => match environment(identity.pid) {
                Ok(environment) => return Ok(Visibility::Environment(environment)),
                Err(_) if REFUSED_WHILE_LOADING && std::time::Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(1));
                }
                _ if alive(identity)? => return Ok(Visibility::Unreadable),
                _ => return Ok(Visibility::Gone),
            },
        }
    }
}

/// macOS refuses an exiting process's arguments, and its records soon after;
/// in between they flag the exit.
#[cfg(target_os = "macos")]
fn image(pid: u32) -> Result<Image> {
    Ok(bsd_info(pid, false)?.map_or(Image::Ended, |info| bsd_image(&info.bsd)))
}

#[cfg(target_os = "macos")]
fn bsd_image(bsd: &libc::proc_bsdinfo) -> Image {
    // PROC_FLAG_INEXIT in sys/proc_info.h.
    const EXITING: u32 = 0x4;
    // BSD SZOMB = 5.
    if bsd.pbi_status == 5 || bsd.pbi_flags & EXITING != 0 {
        Image::Ended
    } else {
        Image::Settled
    }
}

/// macOS 27 hides the environments of its own platform binaries, such as
/// shells, from other processes; one that cannot be checked counts as hidden.
#[cfg(target_os = "macos")]
fn hides_environment(pid: u32) -> bool {
    const CS_PLATFORM_BINARY: u32 = 0x0400_0000;
    super::code_signing::status_flags(pid as i32)
        .is_none_or(|flags| flags & CS_PLATFORM_BINARY != 0)
}

#[cfg(target_os = "linux")]
fn hides_environment(_pid: u32) -> bool {
    false
}

#[cfg(target_os = "linux")]
fn boot_session() -> Option<&'static String> {
    None
}

/// The boot `identity` was recorded in, which starts its birth.
#[cfg(target_os = "linux")]
fn recorded_boot(identity: &Identity) -> Option<&str> {
    identity.birth.split_once(':').map(|(boot, _)| boot)
}

#[cfg(target_os = "linux")]
fn current_boot() -> Option<&'static str> {
    static BOOT: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    BOOT.get_or_init(|| {
        let boot = std::fs::read_to_string("/proc/sys/kernel/random/boot_id").ok()?;
        Some(boot.trim().to_owned())
    })
    .as_deref()
}

/// The boot `identity` was recorded in, which its unique ID names.
#[cfg(target_os = "macos")]
fn recorded_boot(identity: &Identity) -> Option<&str> {
    identity.unique.as_ref().map(|unique| unique.boot.as_str())
}

#[cfg(target_os = "macos")]
fn current_boot() -> Option<&'static str> {
    boot_session().map(String::as_str)
}

#[cfg(target_os = "macos")]
fn platform_lineage(pid: u32) -> Result<Option<Lineage>> {
    let Some(info) = bsd_info(pid, false)? else {
        return Ok(None);
    };
    let bsd = &info.bsd;
    // BSD SZOMB = 5. Zombies cannot execute or hold resources.
    if bsd.pbi_uid != unsafe { libc::geteuid() } || bsd.pbi_status == 5 {
        return Ok(None);
    }
    Ok(Some(Lineage {
        identity: Identity {
            pid,
            birth: format!("{}:{}", bsd.pbi_start_tvsec, bsd.pbi_start_tvusec),
            unique: boot_session().map(|boot| UniqueId {
                boot: boot.clone(),
                id: info.unique.unique,
            }),
        },
        unique: Some(info.unique.unique),
        parent: Some(info.unique.parent_unique),
        parent_pid: bsd.pbi_ppid,
        adopted: bsd.pbi_ppid == 1 && !started_by_launchd(&info.unique, launchd_version()),
    }))
}

/// The lineage of `pid`, a setuid program this user started, from the
/// records macOS shows for any process, since it refuses the full identity
/// of another user's process.
#[cfg(target_os = "macos")]
fn elevated_lineage(pid: u32) -> Result<Option<Lineage>> {
    let read = || pid_info::<UniqueIdentifiers>(pid, PROC_PIDUNIQIDENTIFIERINFO, 0);
    let Some(unique) = read()? else {
        return Ok(None);
    };
    let (Some(short), Some(birth)) = (
        pid_info::<libc::proc_bsdshortinfo>(pid, libc::PROC_PIDT_SHORTBSDINFO, 0)?,
        start_time(pid)?,
    ) else {
        return Ok(None);
    };
    // The process may have exited between the reads and left its PID to
    // another; unique IDs are never reused.
    let same = read()?.is_some_and(|again| again.unique == unique.unique);
    // BSD SZOMB = 5. Zombies cannot execute or hold resources.
    if !same || short.pbsi_status == 5 {
        return Ok(None);
    }
    Ok(Some(Lineage {
        identity: Identity {
            pid,
            birth,
            unique: boot_session().map(|boot| UniqueId {
                boot: boot.clone(),
                id: unique.unique,
            }),
        },
        unique: Some(unique.unique),
        parent: Some(unique.parent_unique),
        parent_pid: short.pbsi_ppid,
        adopted: short.pbsi_ppid == 1 && !started_by_launchd(&unique, launchd_version()),
    }))
}

/// When `pid` started, in the form its identity records. `p_starttime` opens
/// `kinfo_proc`, which libc does not bind and macOS shows for any process.
#[cfg(target_os = "macos")]
fn start_time(pid: u32) -> Result<Option<String>> {
    // `sizeof(struct kinfo_proc)` on 64-bit macOS.
    const KINFO_PROC_SIZE: usize = 648;
    let mut record = [0_u8; KINFO_PROC_SIZE];
    let mut len = record.len();
    let mut mib = [
        libc::CTL_KERN,
        libc::KERN_PROC,
        libc::KERN_PROC_PID,
        pid as i32,
    ];
    let result = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            mib.len() as u32,
            record.as_mut_ptr().cast(),
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if result != 0 {
        return Err(std::io::Error::last_os_error()).context("inspect process start");
    }
    // An exited process has no record.
    if len == 0 {
        return Ok(None);
    }
    ensure!(len == KINFO_PROC_SIZE, "incomplete process record");
    let seconds = i64::from_ne_bytes(record[..8].try_into()?);
    let micros = i32::from_ne_bytes(record[8..12].try_into()?);
    Ok(Some(format!("{seconds}:{micros}")))
}

/// This boot's session UUID, which names the boot unique process IDs belong to.
#[cfg(target_os = "macos")]
fn boot_session() -> Option<&'static String> {
    static BOOT: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    BOOT.get_or_init(|| {
        let mut bytes = [0_u8; 64];
        let mut len = bytes.len();
        let result = unsafe {
            libc::sysctlbyname(
                c"kern.bootsessionuuid".as_ptr(),
                bytes.as_mut_ptr().cast(),
                &mut len,
                std::ptr::null_mut(),
                0,
            )
        };
        if result != 0 {
            return None;
        }
        let session = std::ffi::CStr::from_bytes_until_nul(bytes.get(..len)?).ok()?;
        Some(session.to_string_lossy().into_owned())
    })
    .as_ref()
}

/// Exec resets an adopted process's parent unique ID to launchd's, but not
/// the version of the PID it was forked from, so only launchd's version read
/// and matching shows that launchd started it.
#[cfg(target_os = "macos")]
fn started_by_launchd(process: &UniqueIdentifiers, launchd: Option<u32>) -> bool {
    // Launchd is the first process the kernel starts.
    const LAUNCHD_UNIQUE: u64 = 1;
    process.parent_unique == LAUNCHD_UNIQUE && launchd == Some(process.original_parent_version)
}

/// Launchd's PID version, which it keeps for the whole boot.
#[cfg(target_os = "macos")]
fn launchd_version() -> Option<u32> {
    static LAUNCHD_VERSION: std::sync::OnceLock<Option<u32>> = std::sync::OnceLock::new();
    *LAUNCHD_VERSION.get_or_init(|| {
        let launchd = pid_info::<UniqueIdentifiers>(1, PROC_PIDUNIQIDENTIFIERINFO, 0);
        Some(launchd.ok()??.id_version as u32)
    })
}

#[cfg(target_os = "macos")]
fn platform_unreaped(identity: &Identity) -> Result<bool> {
    // SZOMB in sys/proc.h.
    const ZOMBIE: u32 = 5;
    let Some(info) = bsd_info(identity.pid, true)? else {
        return Ok(false);
    };
    let bsd = &info.bsd;
    Ok(bsd.pbi_status == ZOMBIE
        && bsd.pbi_uid == unsafe { libc::geteuid() }
        && format!("{}:{}", bsd.pbi_start_tvsec, bsd.pbi_start_tvusec) == identity.birth)
}

#[cfg(target_os = "macos")]
pub(super) fn parent(pid: u32) -> Option<u32> {
    bsd_info(pid, false)
        .ok()
        .flatten()
        .map(|info| info.bsd.pbi_ppid)
}

/// `proc_bsdinfowithuniqid` and `proc_uniqidentifierinfo` from XNU's private
/// `proc_info` flavors, which libc does not bind: one read gives the birth and
/// unique IDs together.
#[cfg(target_os = "macos")]
#[repr(C)]
struct BsdInfo {
    bsd: libc::proc_bsdinfo,
    unique: UniqueIdentifiers,
}

#[cfg(target_os = "macos")]
#[repr(C)]
#[allow(dead_code, reason = "unread fields fix the kernel's layout")]
struct UniqueIdentifiers {
    uuid: [u8; 16],
    unique: u64,
    parent_unique: u64,
    id_version: i32,
    original_parent_version: u32,
    reserved: [u64; 2],
}

#[cfg(target_os = "macos")]
const PROC_PIDUNIQIDENTIFIERINFO: i32 = 17;
#[cfg(target_os = "macos")]
const PROC_PIDT_BSDINFOWITHUNIQID: i32 = 18;

#[cfg(target_os = "macos")]
fn bsd_info(pid: u32, include_zombies: bool) -> Result<Option<BsdInfo>> {
    // XNU only searches its zombie list when this argument is nonzero.
    pid_info(pid, PROC_PIDT_BSDINFOWITHUNIQID, u64::from(include_zombies))
}

/// `pid`'s `proc_pidinfo` record of `flavor`, a plain C struct, or `None`
/// when it has exited or is another user's.
#[cfg(target_os = "macos")]
fn pid_info<T>(pid: u32, flavor: i32, argument: u64) -> Result<Option<T>> {
    let mut info = std::mem::MaybeUninit::<T>::zeroed();
    let size = std::mem::size_of::<T>();
    let result = unsafe {
        libc::proc_pidinfo(
            pid as i32,
            flavor,
            argument,
            info.as_mut_ptr().cast(),
            size as i32,
        )
    };
    if result == 0 {
        let error = std::io::Error::last_os_error();
        if process_identity_unavailable(error.raw_os_error()) {
            return Ok(None);
        }
        return Err(error).context("inspect process identity");
    }
    ensure!(result as usize == size, "incomplete process identity");
    Ok(Some(unsafe { info.assume_init() }))
}

#[cfg(any(target_os = "macos", test))]
fn process_identity_unavailable(errno: Option<i32>) -> bool {
    // EPERM means proc_pidinfo could not inspect another user's process. It
    // cannot be an execution Shoal started, so treat it as a different PID.
    matches!(errno, Some(libc::ESRCH | libc::ENOENT | libc::EPERM))
}

#[cfg(target_os = "macos")]
fn environment(pid: u32) -> Result<Vec<Vec<u8>>> {
    let capacity = unsafe { libc::sysconf(libc::_SC_ARG_MAX) };
    ensure!(
        (1..=16 * 1024 * 1024).contains(&capacity),
        "invalid process argument limit"
    );
    let mut bytes = vec![0_u8; capacity as usize];
    let mut len = bytes.len();
    let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid as i32];
    let result = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            3,
            bytes.as_mut_ptr().cast(),
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    ensure!(result == 0, "cannot inspect process environment");
    bytes.truncate(len);
    ensure!(bytes.len() >= 4, "incomplete process arguments");
    let argc = i32::from_ne_bytes(bytes[..4].try_into()?);
    ensure!(argc > 0, "process arguments unavailable");
    let mut offset = 4;
    while offset < bytes.len() && bytes[offset] != 0 {
        offset += 1;
    }
    while offset < bytes.len() && bytes[offset] == 0 {
        offset += 1;
    }
    for _ in 0..argc {
        while offset < bytes.len() && bytes[offset] != 0 {
            offset += 1;
        }
        offset += 1;
    }
    ensure!(offset <= bytes.len(), "incomplete process arguments");
    while offset < bytes.len() && bytes[offset] == 0 {
        offset += 1;
    }
    Ok(bytes[offset..]
        .split(|b| *b == 0)
        .take_while(|entry| !entry.is_empty())
        .map(<[u8]>::to_vec)
        .collect())
}

#[cfg(target_os = "linux")]
fn platform_lineage(pid: u32) -> Result<Option<Lineage>> {
    proc_lineage(pid, |owner| owner == unsafe { libc::geteuid() })
}

/// The lineage of `pid`, a setuid program this user started, whose records
/// another user owns.
#[cfg(target_os = "linux")]
fn elevated_lineage(pid: u32) -> Result<Option<Lineage>> {
    proc_lineage(pid, |owner| owner != unsafe { libc::geteuid() })
}

/// The lineage of `pid` when `owned` accepts the owner of its records.
#[cfg(target_os = "linux")]
fn proc_lineage(pid: u32, owned: impl Fn(u32) -> bool) -> Result<Option<Lineage>> {
    use std::os::unix::fs::MetadataExt;
    let path = format!("/proc/{pid}");
    let metadata = match std::fs::metadata(&path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    if !owned(metadata.uid()) {
        return Ok(None);
    }
    let Some(stat) = read_process_stat(&format!("{path}/stat"))? else {
        return Ok(None);
    };
    let fields = stat_fields(&stat)?;
    ensure!(fields.len() > 19, "incomplete process stat");
    if fields[0] == "Z" || fields[0] == "X" {
        return Ok(None);
    }
    let boot = std::fs::read_to_string("/proc/sys/kernel/random/boot_id")?;
    // Linux keeps no record of an exited parent, so lineage ends with it.
    Ok(Some(Lineage {
        identity: Identity {
            pid,
            birth: format!("{}:{}", boot.trim(), fields[19]),
            unique: None,
        },
        unique: None,
        parent: None,
        parent_pid: fields[1].parse()?,
        // Linux shows every environment, so nothing depends on how a process
        // came to init.
        adopted: false,
    }))
}

#[cfg(target_os = "linux")]
fn platform_unreaped(identity: &Identity) -> Result<bool> {
    let Some(stat) = read_process_stat(&format!("/proc/{}/stat", identity.pid))? else {
        return Ok(false);
    };
    let fields = stat_fields(&stat)?;
    ensure!(fields.len() > 19, "incomplete process stat");
    let boot = std::fs::read_to_string("/proc/sys/kernel/random/boot_id")?;
    Ok(fields[0] == "Z" && format!("{}:{}", boot.trim(), fields[19]) == identity.birth)
}

#[cfg(target_os = "linux")]
pub(super) fn parent(pid: u32) -> Option<u32> {
    let stat = read_process_stat(&format!("/proc/{pid}/stat")).ok()??;
    stat_fields(&stat).ok()?.get(1)?.parse().ok()
}

#[cfg(target_os = "linux")]
fn image(pid: u32) -> Result<Image> {
    match read_process_stat(&format!("/proc/{pid}/stat"))? {
        Some(stat) => stat_image(&stat),
        None => Ok(Image::Ended),
    }
}

#[cfg(target_os = "linux")]
fn stat_image(stat: &str) -> Result<Image> {
    const PF_EXITING: u64 = 0x4;
    const PF_KTHREAD: u64 = 0x0020_0000;
    let fields = stat_fields(stat)?;
    // Field 9 is flags and field 51 env_end, which stays 0 until exec
    // publishes the new environment.
    ensure!(fields.len() > 48, "incomplete process stat");
    if fields[6].parse::<u64>()? & (PF_EXITING | PF_KTHREAD) != 0 {
        return Ok(Image::Ended);
    }
    Ok(if fields[48] == "0" {
        Image::Loading
    } else {
        Image::Settled
    })
}

/// Fields after the parenthesized command name, starting with the state.
#[cfg(target_os = "linux")]
pub(super) fn stat_fields(stat: &str) -> Result<Vec<&str>> {
    Ok(stat
        .rsplit_once(')')
        .context("invalid process stat")?
        .1
        .split_whitespace()
        .collect())
}

#[cfg(target_os = "linux")]
pub(super) fn read_process_stat(path: &str) -> Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(stat) => Ok(Some(stat)),
        // Linux returns ESRCH if the process exits between open and read.
        Err(error) if matches!(error.raw_os_error(), Some(libc::ENOENT | libc::ESRCH)) => Ok(None),
        Err(error) => Err(error.into()),
    }
}

#[cfg(target_os = "linux")]
fn environment(pid: u32) -> Result<Vec<Vec<u8>>> {
    Ok(std::fs::read(format!("/proc/{pid}/environ"))?
        .split(|b| *b == 0)
        .filter(|e| !e.is_empty())
        .map(<[u8]>::to_vec)
        .collect())
}

impl Identity {
    /// A process born before the wrapper cannot be one of its descendants.
    pub fn not_older_than(&self, wrapper: &Identity) -> bool {
        let Some((own_prefix, own_end)) = self.birth.rsplit_once(':') else {
            return true;
        };
        let Some((other_prefix, other_end)) = wrapper.birth.rsplit_once(':') else {
            return true;
        };
        #[cfg(target_os = "macos")]
        {
            match (
                own_prefix.parse::<u64>(),
                own_end.parse::<u64>(),
                other_prefix.parse::<u64>(),
                other_end.parse::<u64>(),
            ) {
                (Ok(a), Ok(b), Ok(c), Ok(d)) => (a, b) >= (c, d),
                _ => true,
            }
        }
        #[cfg(target_os = "linux")]
        {
            own_prefix == other_prefix
                && own_end
                    .parse::<u64>()
                    .ok()
                    .zip(other_end.parse::<u64>().ok())
                    .is_none_or(|(a, b)| a >= b)
        }
    }
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
    let mut command = tokio::process::Command::new(Tool::Ps.program());
    command
        .args(["-ax", "-o", "pid=,uid=,pgid=,ppid="])
        .env("LC_ALL", "C");
    let output = crate::subprocess::Run::new(command)
        .timeout(timing::PROCESS_INVENTORY_TIMEOUT)
        .output()
        .await?;
    let mut rows = Vec::new();
    for line in output.lines() {
        // Negative UIDs (nobody) cannot parse and are never ours; see `scan`.
        let Ok(fields) = line
            .split_whitespace()
            .map(str::parse)
            .collect::<std::result::Result<Vec<u32>, _>>()
        else {
            continue;
        };
        ensure!(fields.len() == 4, "invalid process ancestry inventory");
        if fields[1] == unsafe { libc::geteuid() }
            && fields[0] > 1
            && fields[0] != std::process::id()
        {
            rows.push((fields[0], fields[2], fields[3]));
        }
    }
    Ok(rows)
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
    let marker = format!("{}=", crate::env::EXECUTION_ID).into_bytes();
    let ids = HashSet::from([execution]);
    let deadline = std::time::Instant::now() + timing::PROCESS_SETTLE_TIMEOUT;
    lineage_descendants(root, lineages, |identity| {
        left_execution(identity, &marker, &ids, deadline)
    })
}

/// Whether the process's own environment, read rather than hidden, lacks
/// the execution's marker: a process started without it, such as a demo,
/// no longer belongs to the execution, nor do its descendants.
fn left_execution(
    identity: &Identity,
    marker: &[u8],
    ids: &HashSet<String>,
    deadline: std::time::Instant,
) -> Result<bool> {
    Ok(
        observe(identity.pid, marker, ids, deadline)?.is_some_and(|observed| {
            observed.lineage.identity == *identity
                && matches!(observed.evidence, Evidence::Unmarked)
        }),
    )
}

/// A parent's unique ID is lower than its children's, since the kernel
/// assigns them in order, so one pass in that order finds every descendant.
/// A descendant that `left` names is left out with its own descendants.
fn lineage_descendants(
    root: u64,
    mut lineages: Vec<Lineage>,
    mut left: impl FnMut(&Identity) -> Result<bool>,
) -> Result<Vec<Identity>> {
    lineages.sort_by_key(|lineage| lineage.unique);
    let mut ancestors = HashSet::from([root]);
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

#[cfg(test)]
mod tests;
