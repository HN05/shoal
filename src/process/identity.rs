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
}

/// PID and birth name the process; records made before `unique` existed
/// lack it.
impl PartialEq for Identity {
    fn eq(&self, other: &Self) -> bool {
        self.pid == other.pid && self.birth == other.birth
    }
}

impl Eq for Identity {}

/// A process's identity and its parent's unique ID, which the kernel keeps
/// after the parent exits and launchd adopts the process, until the process
/// replaces its image.
struct Lineage {
    identity: Identity,
    unique: Option<u64>,
    parent: Option<u64>,
    /// Launchd adopted it when its parent exited, rather than starting it.
    adopted: bool,
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

/// Inventory processes whose environment sets `marker` to one of `ids`; each
/// match records the value it carries.
async fn scan_marked(marker: &str, ids: HashSet<String>) -> Result<Scan> {
    if ids.is_empty() {
        return Ok(Scan::default());
    }
    let listing = same_user_parents().await?;
    let marker = format!("{marker}=").into_bytes();
    tokio::task::spawn_blocking(move || {
        let settle_deadline = std::time::Instant::now() + timing::PROCESS_SETTLE_TIMEOUT;
        let mut inventory = Inventory {
            listed: listing.iter().map(|(pid, _)| *pid).collect(),
            observed: HashMap::new(),
        };
        for (pid, parent) in listing {
            if let Some(process) = observe(pid, parent, &marker, &ids, settle_deadline)? {
                inventory.observed.insert(pid, process);
            }
        }
        inventory.attribute()
    })
    .await
    .context("process inventory worker failed")?
}

/// Same-user processes other than init and the daemon, with their parent PIDs.
async fn same_user_parents() -> Result<Vec<(u32, u32)>> {
    let mut command = tokio::process::Command::new(Tool::Ps.program());
    command
        .args(["-ax", "-o", "pid=,uid=,ppid="])
        .env("LC_ALL", "C");
    let output = crate::subprocess::Run::new(command)
        .timeout(timing::PROCESS_INVENTORY_TIMEOUT)
        .output()
        .await
        .context("process inventory failed")?;
    let mut inventory = Vec::new();
    for line in output.lines() {
        let fields: Vec<_> = line.split_whitespace().collect();
        ensure!(fields.len() == 3, "invalid process inventory");
        let pid: u32 = fields[0].parse()?;
        // System daemons may run as negative UIDs such as nobody (-2);
        // they are never ours.
        let Ok(uid) = fields[1].parse::<u32>() else {
            continue;
        };
        if uid == unsafe { libc::geteuid() } && pid > 1 && pid != std::process::id() {
            inventory.push((pid, fields[2].parse()?));
        }
    }
    Ok(inventory)
}

/// One live process and what its own environment says about its owner.
struct Observed {
    identity: Identity,
    parent: u32,
    adopted: bool,
    evidence: Evidence,
}

enum Evidence {
    Marked(String),
    Unmarked,
    Unreadable,
    /// Settled empty, which a platform binary's hidden environment also reads as.
    Hidden,
}

/// Read `pid`'s environment for `marker`, unless it has exited.
fn observe(
    pid: u32,
    parent: u32,
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
    Ok(Some(Observed {
        adopted: lineage.adopted,
        identity: lineage.identity,
        parent,
        evidence,
    }))
}

#[derive(Clone)]
enum Attribution {
    Owned(String),
    Foreign,
    /// Its owner cannot be told; the identity heads the processes it covers.
    Unknown(Identity),
}

/// The inventoried PIDs and the processes among them still alive to observe.
struct Inventory {
    listed: HashSet<u32>,
    observed: HashMap<u32, Observed>,
}

impl Inventory {
    /// Attribute every observed process and collect the scan, checking that
    /// owned processes still live.
    fn attribute(&self) -> Result<Scan> {
        let mut attributions = HashMap::new();
        let mut scan = Scan::default();
        for (pid, process) in &self.observed {
            match self.resolve(*pid, &mut attributions) {
                Attribution::Owned(execution_id) if alive(&process.identity)? => {
                    scan.processes.push(OwnedProcess {
                        execution_id,
                        identity: process.identity.clone(),
                    });
                }
                Attribution::Unknown(head) => scan.unreadable.push(Unattributed {
                    identity: process.identity.clone(),
                    head,
                }),
                _ => {}
            }
        }
        Ok(scan)
    }

    /// `pid`'s owner by its own environment, or for a hidden one by the
    /// nearest ancestor whose environment tells, as its environment was
    /// inherited from there. A hidden process launchd adopted has no ancestor
    /// left to tell.
    fn resolve(&self, pid: u32, attributions: &mut HashMap<u32, Attribution>) -> Attribution {
        let mut hidden: Vec<&Observed> = Vec::new();
        let mut current = pid;
        let attribution = loop {
            if let Some(attribution) = attributions.get(&current) {
                break attribution.clone();
            }
            let Some(process) = self.observed.get(&current) else {
                break self.unobserved_parent(current, hidden.last().copied());
            };
            match &process.evidence {
                Evidence::Marked(id) => break Attribution::Owned(id.clone()),
                Evidence::Unmarked => break Attribution::Foreign,
                Evidence::Unreadable => break Attribution::Unknown(process.identity.clone()),
                Evidence::Hidden => hidden.push(process),
            }
            if process.parent <= 1 {
                break if process.adopted {
                    Attribution::Unknown(process.identity.clone())
                } else {
                    Attribution::Foreign
                };
            }
            if hidden.len() > MAX_ANCESTRY {
                break Attribution::Unknown(process.identity.clone());
            }
            current = process.parent;
        };
        for process in hidden.into_iter().chain(self.observed.get(&current)) {
            attributions.insert(process.identity.pid, attribution.clone());
        }
        attribution
    }

    /// A parent left out of the inventory belongs to another user or is the
    /// daemon, neither of which can carry a marker it did not start with. A
    /// listed one that cannot be observed has exited, or exits after the
    /// inventory, and leaves `child` without an ancestor to tell.
    fn unobserved_parent(&self, parent: u32, child: Option<&Observed>) -> Attribution {
        let exists = parent == std::process::id()
            || unsafe { libc::kill(parent as i32, 0) } == 0
            || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM);
        match child {
            Some(child) if self.listed.contains(&parent) || !exists => {
                Attribution::Unknown(child.identity.clone())
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

// Only Linux exposes transient images; macOS reports every image settled.
#[cfg_attr(target_os = "macos", allow(dead_code))]
enum Image {
    Settled,
    /// Replacing its image: the new environment is not published yet.
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

#[cfg(target_os = "macos")]
fn image(_pid: u32) -> Result<Image> {
    Ok(Image::Settled)
}

/// macOS 27 hides the environments of its own platform binaries, such as
/// shells, from other processes; one that cannot be checked counts as hidden.
#[cfg(target_os = "macos")]
fn hides_environment(pid: u32) -> bool {
    const CS_OPS_STATUS: u32 = 0;
    const CS_PLATFORM_BINARY: u32 = 0x0400_0000;
    unsafe extern "C" {
        fn csops(pid: libc::pid_t, ops: u32, status: *mut u32, size: usize) -> libc::c_int;
    }
    let mut status = 0_u32;
    let result = unsafe { csops(pid as i32, CS_OPS_STATUS, &mut status, size_of::<u32>()) };
    result != 0 || status & CS_PLATFORM_BINARY != 0
}

#[cfg(target_os = "linux")]
fn hides_environment(_pid: u32) -> bool {
    false
}

#[cfg(target_os = "linux")]
fn boot_session() -> Option<&'static String> {
    None
}

#[cfg(target_os = "macos")]
fn platform_lineage(pid: u32) -> Result<Option<Lineage>> {
    let Some(info) = bsd_info(pid)? else {
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
        adopted: bsd.pbi_ppid == 1 && !started_by_launchd(&info.unique),
    }))
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
/// the version of the PID it was forked from.
#[cfg(target_os = "macos")]
fn started_by_launchd(process: &UniqueIdentifiers) -> bool {
    // Launchd is the first process the kernel starts.
    const LAUNCHD_UNIQUE: u64 = 1;
    static LAUNCHD_VERSION: std::sync::OnceLock<Option<u32>> = std::sync::OnceLock::new();
    let version = LAUNCHD_VERSION.get_or_init(|| {
        let mut launchd = std::mem::MaybeUninit::<UniqueIdentifiers>::zeroed();
        let size = std::mem::size_of::<UniqueIdentifiers>();
        let result = unsafe {
            libc::proc_pidinfo(
                1,
                PROC_PIDUNIQIDENTIFIERINFO,
                0,
                launchd.as_mut_ptr().cast(),
                size as i32,
            )
        };
        (result as usize == size).then(|| unsafe { launchd.assume_init() }.id_version as u32)
    });
    process.parent_unique == LAUNCHD_UNIQUE
        && version.is_none_or(|version| version == process.original_parent_version)
}

#[cfg(target_os = "macos")]
fn parent(pid: u32) -> Option<u32> {
    bsd_info(pid).ok().flatten().map(|info| info.bsd.pbi_ppid)
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
fn bsd_info(pid: u32) -> Result<Option<BsdInfo>> {
    let mut info = std::mem::MaybeUninit::<BsdInfo>::zeroed();
    let size = std::mem::size_of::<BsdInfo>();
    let result = unsafe {
        libc::proc_pidinfo(
            pid as i32,
            PROC_PIDT_BSDINFOWITHUNIQID,
            0,
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
    use std::os::unix::fs::MetadataExt;
    let path = format!("/proc/{pid}");
    let metadata = match std::fs::metadata(&path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    if metadata.uid() != unsafe { libc::geteuid() } {
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
        // Linux shows every environment, so nothing depends on how a process
        // came to init.
        adopted: false,
    }))
}

#[cfg(target_os = "linux")]
fn parent(pid: u32) -> Option<u32> {
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
fn stat_fields(stat: &str) -> Result<Vec<&str>> {
    Ok(stat
        .rsplit_once(')')
        .context("invalid process stat")?
        .1
        .split_whitespace()
        .collect())
}

#[cfg(target_os = "linux")]
fn read_process_stat(path: &str) -> Result<Option<String>> {
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
/// descending from it through live parents. Without that proof, group members
/// are candidates only and must not be killed.
pub async fn related(
    child: Option<&Identity>,
    group: Option<u32>,
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
        pids.extend(descendants(child.unwrap().pid, &rows));
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
    let lineage = descended_from(root, &rows)?;
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

/// `root` and the PIDs whose parent links lead to it.
fn descendants(root: u32, rows: &[(u32, u32, u32)]) -> HashSet<u32> {
    let mut pids = HashSet::from([root]);
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
/// unique ID `root`, which may have exited.
fn descended_from(root: u64, rows: &[(u32, u32, u32)]) -> Result<Vec<Identity>> {
    let mut lineages = Vec::new();
    for (pid, _, _) in rows {
        lineages.extend(lineage(*pid)?);
    }
    Ok(lineage_descendants(root, lineages))
}

/// A parent's unique ID is lower than its children's, since the kernel
/// assigns them in order, so one pass in that order finds every descendant.
fn lineage_descendants(root: u64, mut lineages: Vec<Lineage>) -> Vec<Identity> {
    lineages.sort_by_key(|lineage| lineage.unique);
    let mut ancestors = HashSet::from([root]);
    let mut found = Vec::new();
    for lineage in lineages {
        let (Some(unique), Some(parent)) = (lineage.unique, lineage.parent) else {
            continue;
        };
        if ancestors.contains(&parent) {
            ancestors.insert(unique);
            found.push(lineage.identity);
        }
    }
    found
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
mod tests {
    use super::*;

    #[test]
    fn process_identity_unavailable_includes_inaccessible_processes() {
        assert!(process_identity_unavailable(Some(libc::ESRCH)));
        assert!(process_identity_unavailable(Some(libc::ENOENT)));
        assert!(process_identity_unavailable(Some(libc::EPERM)));
        assert!(!process_identity_unavailable(Some(libc::EACCES)));
        assert!(!process_identity_unavailable(None));
    }

    #[test]
    fn lineage_follows_unique_ids_through_exited_parents_only_to_the_root() {
        let process = |pid, unique, parent| Lineage {
            identity: Identity {
                pid,
                birth: pid.to_string(),
                unique: None,
            },
            unique: Some(unique),
            parent: Some(parent),
            adopted: false,
        };
        // 10 has exited; 11 descends from it and 13 from 11, while 12 is
        // another branch whose parent 9 is no descendant of 10.
        let found = lineage_descendants(
            10,
            vec![process(13, 13, 11), process(12, 12, 9), process(11, 11, 10)],
        );
        let pids: Vec<_> = found.iter().map(|identity| identity.pid).collect();
        assert_eq!(pids, [11, 13]);
    }

    #[test]
    fn a_unique_id_recorded_in_another_boot_roots_no_lineage() -> Result<()> {
        let this = capture(std::process::id())?.context("identify this process")?;
        assert_eq!(
            this.lineage_root(),
            this.unique.as_ref().map(|unique| unique.id)
        );
        let earlier = Identity {
            unique: Some(UniqueId {
                boot: "an earlier boot".into(),
                id: 1,
            }),
            ..this
        };
        assert_eq!(earlier.lineage_root(), None);
        Ok(())
    }

    #[test]
    fn identity_equality_ignores_a_missing_unique_id() {
        let recorded = Identity {
            pid: 7,
            birth: "1:2".into(),
            unique: None,
        };
        let captured = Identity {
            unique: Some(UniqueId {
                boot: "boot".into(),
                id: 42,
            }),
            ..recorded.clone()
        };
        assert_eq!(recorded, captured);
        let stored: Identity = serde_json::from_str(r#"{"pid":7,"birth":"1:2"}"#).unwrap();
        assert_eq!(stored.unique, None);
    }

    /// A command that started a background job and exited: launchd adopts
    /// the job, whose recorded lineage still leads to the command.
    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn an_exited_commands_background_job_is_owned_through_its_lineage() -> Result<()> {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
        let mut command = tokio::process::Command::new("sh")
            .args(["-c", "sleep 30 & echo $!; read _"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .process_group(0)
            .kill_on_drop(true)
            .spawn()?;
        let identity = capture(command.id().context("command PID")?)?.context("command")?;
        let mut stdout = tokio::io::BufReader::new(command.stdout.take().context("stdout")?);
        let mut line = String::new();
        stdout.read_line(&mut line).await?;
        let job = capture(line.trim().parse()?)?.context("background job")?;
        // Replacing its image after launchd adopts it would end its lineage.
        while executable(job.pid).as_deref() != Some(std::path::Path::new("/bin/sleep")) {
            tokio::task::yield_now().await;
        }
        command.stdin.take().context("stdin")?.shutdown().await?;
        command.wait().await?;
        let (owned, candidates) = related(Some(&identity), Some(identity.pid)).await?;
        signal(&job, libc::SIGKILL)?;
        assert!(owned.contains(&job));
        assert!(!candidates.contains(&job));
        Ok(())
    }

    fn observed(pid: u32, parent: u32, adopted: bool, evidence: Evidence) -> (u32, Observed) {
        let identity = Identity {
            pid,
            birth: pid.to_string(),
            unique: None,
        };
        (
            pid,
            Observed {
                identity,
                parent,
                adopted,
                evidence,
            },
        )
    }

    #[test]
    fn a_hidden_environment_takes_the_attribution_of_the_nearest_ancestor_that_tells() {
        use Evidence::{Hidden, Marked, Unmarked};
        let inventory = Inventory {
            listed: HashSet::from([10, 11, 12, 20, 21, 30, 31, 32, 40, 41]),
            observed: HashMap::from([
                // A shell and its job under a marked agent.
                observed(10, 9, false, Marked("execution".into())),
                observed(11, 10, false, Hidden),
                observed(12, 11, false, Hidden),
                // A shell under an unmarked terminal.
                observed(20, 1, false, Unmarked),
                observed(21, 20, false, Hidden),
                // A job launchd adopted after its shell exited, and its child.
                observed(31, 1, true, Hidden),
                observed(32, 31, false, Hidden),
                // A job launchd started.
                observed(30, 1, false, Hidden),
                // A shell whose listed parent exited before it was observed.
                observed(41, 40, false, Hidden),
            ]),
        };
        let mut attributions = HashMap::new();
        let mut resolve = |pid| match inventory.resolve(pid, &mut attributions) {
            Attribution::Owned(id) => format!("owned by {id}"),
            Attribution::Foreign => "foreign".into(),
            Attribution::Unknown(head) => format!("unknown under {}", head.pid),
        };
        assert_eq!(resolve(12), "owned by execution");
        assert_eq!(resolve(11), "owned by execution");
        assert_eq!(resolve(21), "foreign");
        assert_eq!(resolve(32), "unknown under 31");
        assert_eq!(resolve(30), "foreign");
        assert_eq!(resolve(41), "unknown under 41");
    }

    #[cfg(target_os = "macos")]
    fn executable(pid: u32) -> Option<PathBuf> {
        let mut path = vec![0_u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
        let len =
            unsafe { libc::proc_pidpath(pid as i32, path.as_mut_ptr().cast(), path.len() as u32) };
        path.truncate(usize::try_from(len).ok()?);
        Some(PathBuf::from(OsStr::from_bytes(&path)))
    }

    /// A shell's background job that launchd adopted, on a macOS that hides
    /// a platform binary's environment.
    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn an_adopted_job_with_a_hidden_environment_is_unreadable() -> Result<()> {
        let output = tokio::process::Command::new("sh")
            .args(["-c", "sleep 30 </dev/null >/dev/null 2>&1 & echo $!"])
            .output()
            .await?;
        let pid = std::str::from_utf8(&output.stdout)?.trim().parse()?;
        let job = capture(pid)?.context("background job")?;
        let deadline = std::time::Instant::now() + timing::PROCESS_SETTLE_TIMEOUT;
        let hidden = matches!(
            visibility(&job, deadline)?,
            Visibility::Environment(environment) if environment.is_empty()
        ) && hides_environment(pid);
        let scan = scan(HashSet::from(["unrelated-execution".into()])).await;
        signal(&job, libc::SIGKILL)?;
        let unreadable = scan?.unreadable.iter().any(|u| u.identity == job);
        assert_eq!(unreadable, hidden);
        Ok(())
    }

    #[test]
    #[ignore = "helper child launched by ownership test"]
    fn marked_child() {
        std::thread::sleep(Duration::from_secs(30));
    }
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn stat_read_after_process_exit_is_not_an_inventory_failure() {
        use std::os::fd::AsRawFd;

        let mut child = tokio::process::Command::new("sleep")
            .arg("30")
            .env_clear()
            .env("SHOAL_TEST_PROCESS", "1")
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let pid = child.id().unwrap();
        let stat = std::fs::File::open(format!("/proc/{pid}/stat")).unwrap();
        assert!(capture(pid).unwrap().is_some());
        child.kill().await.unwrap();

        // Keep the proc inode open across exit to deterministically exercise
        // the race between opening stat and reading it during an inventory.
        let path = format!("/proc/self/fd/{}", stat.as_raw_fd());
        assert_eq!(
            std::fs::read_to_string(&path).unwrap_err().raw_os_error(),
            Some(libc::ESRCH)
        );
        assert!(read_process_stat(&path).unwrap().is_none());
        assert!(capture(pid).unwrap().is_none());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn empty_environment_waits_only_for_a_loading_image() {
        let stat = |flags: u64, env_end: u64| {
            let mut fields = vec![0; 50];
            fields[6] = flags;
            fields[48] = env_end;
            let fields: Vec<_> = fields.iter().map(u64::to_string).collect();
            format!("7 (odd) name) S {}", fields[1..].join(" "))
        };
        assert!(matches!(stat_image(&stat(0, 0)).unwrap(), Image::Loading));
        assert!(matches!(
            stat_image(&stat(0, 4096)).unwrap(),
            Image::Settled
        ));
        assert!(matches!(stat_image(&stat(0x4, 0)).unwrap(), Image::Ended));
        assert!(matches!(
            stat_image(&stat(0x20_0000, 0)).unwrap(),
            Image::Ended
        ));
    }

    #[tokio::test]
    async fn spawned_process_environment_is_visible_until_it_exits() {
        let mut child = tokio::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "process::identity::tests::marked_child",
                "--ignored",
            ])
            .stdout(std::process::Stdio::null())
            .env_clear()
            .env("SHOAL_TEST_PROCESS", "1")
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let identity = capture(child.id().unwrap()).unwrap().unwrap();
        let deadline = std::time::Instant::now() + timing::PROCESS_SETTLE_TIMEOUT;
        // Spawning returns once exec is committed, possibly before the new
        // environment is published.
        let Visibility::Environment(environment) = visibility(&identity, deadline).unwrap() else {
            panic!("spawned process environment is not visible");
        };
        assert!(
            environment
                .iter()
                .any(|entry| entry == b"SHOAL_TEST_PROCESS=1")
        );
        child.kill().await.unwrap();
        assert!(matches!(
            visibility(&identity, deadline).unwrap(),
            Visibility::Gone
        ));
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn refusal_after_an_empty_read_waits_for_the_published_environment() {
        let mut child = tokio::process::Command::new("sleep")
            .arg("30")
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let identity = capture(child.id().unwrap()).unwrap().unwrap();
        // The order macOS reports while a spawned process execs.
        let mut reads = [
            Ok(vec![]),
            Err(anyhow::anyhow!("cannot inspect process environment")),
            Ok(vec![b"SHOAL_TEST_PROCESS=1".to_vec()]),
        ]
        .into_iter();
        let deadline = std::time::Instant::now() + timing::PROCESS_SETTLE_TIMEOUT;
        let visibility = visibility_with(&identity, deadline, |_| reads.next().unwrap()).unwrap();
        let Visibility::Environment(environment) = visibility else {
            panic!("published environment is not visible");
        };
        assert_eq!(environment, [b"SHOAL_TEST_PROCESS=1".to_vec()]);
        child.kill().await.unwrap();
    }

    #[tokio::test]
    async fn settled_empty_environment_is_readable_and_has_no_execution_marker() {
        let mut child = tokio::process::Command::new("cat")
            .env_clear()
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let identity = capture(child.id().unwrap()).unwrap().unwrap();
        let deadline = std::time::Instant::now() + timing::PROCESS_SETTLE_TIMEOUT;
        let Visibility::Environment(environment) = visibility(&identity, deadline).unwrap() else {
            panic!("settled empty environment must be readable");
        };
        assert!(environment.is_empty());
        let scan = scan(HashSet::from(["unrelated-execution".into()]))
            .await
            .unwrap();
        assert!(scan.unreadable.iter().all(|u| u.identity != identity));
        assert!(scan.processes.iter().all(|p| p.identity != identity));
        child.kill().await.unwrap();
        child.wait().await.unwrap();
    }

    #[tokio::test]
    async fn discovers_marked_processes_and_rejects_changed_identity() {
        let id = uuid::Uuid::new_v4().to_string();
        let mut child = tokio::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "process::identity::tests::marked_child",
                "--ignored",
            ])
            .stdout(std::process::Stdio::null())
            .env(crate::env::EXECUTION_ID, &id)
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let identity = capture(child.id().unwrap()).unwrap().unwrap();
        let inventory = scan(HashSet::from([id.clone()])).await.unwrap();
        assert!(
            inventory
                .processes
                .iter()
                .any(|p| p.identity == identity && p.execution_id == id)
        );

        let changed = Identity {
            birth: "different-process".into(),
            ..identity.clone()
        };
        signal(&changed, libc::SIGKILL).unwrap();
        assert!(alive(&identity).unwrap());
        signal(&identity, libc::SIGKILL).unwrap();
        child.wait().await.unwrap();
        assert!(!alive(&identity).unwrap());
    }
}
