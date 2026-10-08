//! Same-user process ownership for cooperative recovery. Never expose process
//! arguments/environment; only the explicit execution marker and scoped
//! ancestors' state directories leave this module.
use crate::protocol::timing;
use crate::tools::Tool;
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet, ffi::OsStr, os::unix::ffi::OsStrExt, path::PathBuf, time::Duration,
};

/// Deeper chains are not plausible process trees; stop rather than loop.
const MAX_ANCESTRY: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Identity {
    pub pid: u32,
    pub birth: String,
}

pub fn capture(pid: u32) -> Result<Option<Identity>> {
    ensure!(pid > 1 && pid <= i32::MAX as u32, "invalid process ID");
    platform_identity(pid)
}

/// State directories of this process's ancestors that started with workspace
/// scope, nearest first. Ancestors keep their initial environment when a
/// descendant clears its own; only reparenting (daemonizing) leaves the chain.
/// Unreadable ancestors are skipped, so this is cooperative, not a boundary.
pub fn scoped_ancestor_state_dirs() -> Vec<PathBuf> {
    let token = format!("{}=", crate::env::SCOPE_TOKEN).into_bytes();
    let state = format!("{}=", crate::env::STATE_DIR).into_bytes();
    let mut dirs = Vec::new();
    let mut pid = std::os::unix::process::parent_id();
    for _ in 0..MAX_ANCESTRY {
        if pid <= 1 {
            break;
        }
        if let Ok(environment) = environment(pid)
            && environment.iter().any(|entry| entry.starts_with(&token))
            && let Some(dir) = environment
                .iter()
                .find_map(|entry| entry.strip_prefix(state.as_slice()))
        {
            dirs.push(PathBuf::from(OsStr::from_bytes(dir)));
        }
        let Some(next) = parent(pid) else {
            break;
        };
        pid = next;
    }
    dirs
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
    pub unreadable: Vec<Identity>,
}

pub async fn scan(ids: HashSet<String>) -> Result<Scan> {
    if ids.is_empty() {
        return Ok(Scan::default());
    }
    let mut command = tokio::process::Command::new(Tool::Ps.program());
    command.args(["-ax", "-o", "pid=,uid="]).env("LC_ALL", "C");
    let output = crate::subprocess::Run::new(command)
        .timeout(timing::PROCESS_INVENTORY_TIMEOUT)
        .output()
        .await
        .context("process inventory failed")?;
    let marker = format!("{}=", crate::env::EXECUTION_ID).into_bytes();
    tokio::task::spawn_blocking(move || {
        let mut scan = Scan::default();
        let settle_deadline = std::time::Instant::now() + timing::PROCESS_SETTLE_TIMEOUT;
        for line in output.lines() {
            let fields: Vec<_> = line.split_whitespace().collect();
            ensure!(fields.len() == 2, "invalid process inventory");
            let pid: u32 = fields[0].parse()?;
            // System daemons may run as negative UIDs such as nobody (-2);
            // they are never ours.
            let Ok(uid) = fields[1].parse::<u32>() else {
                continue;
            };
            if uid != unsafe { libc::geteuid() } || pid <= 1 || pid == std::process::id() {
                continue;
            }
            let Some(identity) = capture(pid)? else {
                continue;
            };
            match visibility(&identity, settle_deadline)? {
                Visibility::Environment(environment) => {
                    if let Some(id) = environment
                        .iter()
                        .find_map(|entry| entry.strip_prefix(marker.as_slice()))
                        .and_then(|id| std::str::from_utf8(id).ok())
                        .filter(|id| ids.contains(*id))
                        && alive(&identity)?
                    {
                        scan.processes.push(OwnedProcess {
                            execution_id: id.into(),
                            identity,
                        });
                    }
                }
                Visibility::Unreadable => scan.unreadable.push(identity),
                Visibility::Gone => {}
            }
        }
        Ok(scan)
    })
    .await
    .context("process inventory worker failed")?
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

/// Linux reads an empty environment while a process replaces its image and
/// after an exiting process releases its memory; neither is evidence about
/// ownership. Wait for loading images, then confirm a settled empty read.
fn visibility(identity: &Identity, deadline: std::time::Instant) -> Result<Visibility> {
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
            Err(_) => {
                return Ok(match image(identity.pid)? {
                    Image::Ended => Visibility::Gone,
                    _ => Visibility::Unreadable,
                });
            }
            Ok(_) => {}
        }
        match image(identity.pid)? {
            Image::Ended => return Ok(Visibility::Gone),
            Image::Loading if std::time::Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(1));
            }
            Image::Loading => return Ok(Visibility::Unreadable),
            // The image may have settled after the empty read.
            Image::Settled => {
                return Ok(match environment(identity.pid) {
                    Ok(environment) => Visibility::Environment(environment),
                    _ if alive(identity)? => Visibility::Unreadable,
                    _ => Visibility::Gone,
                });
            }
        }
    }
}

#[cfg(target_os = "macos")]
fn image(_pid: u32) -> Result<Image> {
    Ok(Image::Settled)
}

#[cfg(target_os = "macos")]
fn platform_identity(pid: u32) -> Result<Option<Identity>> {
    let Some(info) = bsd_info(pid)? else {
        return Ok(None);
    };
    // BSD SZOMB = 5. Zombies cannot execute or hold resources.
    if info.pbi_uid != unsafe { libc::geteuid() } || info.pbi_status == 5 {
        return Ok(None);
    }
    Ok(Some(Identity {
        pid,
        birth: format!("{}:{}", info.pbi_start_tvsec, info.pbi_start_tvusec),
    }))
}

#[cfg(target_os = "macos")]
fn parent(pid: u32) -> Option<u32> {
    bsd_info(pid).ok().flatten().map(|info| info.pbi_ppid)
}

#[cfg(target_os = "macos")]
fn bsd_info(pid: u32) -> Result<Option<libc::proc_bsdinfo>> {
    let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::zeroed();
    let size = std::mem::size_of::<libc::proc_bsdinfo>();
    let result = unsafe {
        libc::proc_pidinfo(
            pid as i32,
            libc::PROC_PIDTBSDINFO,
            0,
            info.as_mut_ptr().cast(),
            size as i32,
        )
    };
    if result == 0 {
        let error = std::io::Error::last_os_error();
        if matches!(error.raw_os_error(), Some(libc::ESRCH | libc::ENOENT)) {
            return Ok(None);
        }
        return Err(error).context("inspect process identity");
    }
    ensure!(result as usize == size, "incomplete process identity");
    Ok(Some(unsafe { info.assume_init() }))
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
fn platform_identity(pid: u32) -> Result<Option<Identity>> {
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
    Ok(Some(Identity {
        pid,
        birth: format!("{}:{}", boot.trim(), fields[19]),
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
/// Without that proof, group members are candidates only and must not be killed.
pub async fn related(
    child: Option<&Identity>,
    group: Option<u32>,
) -> Result<(Vec<Identity>, Vec<Identity>)> {
    let Some(group) = group else {
        return Ok((vec![], vec![]));
    };
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
    let live = child.map(alive).transpose()?.unwrap_or(false);
    let mut pids: HashSet<u32> = rows
        .iter()
        .filter(|(_, pgid, _)| *pgid == group)
        .map(|(pid, _, _)| *pid)
        .collect();
    if live {
        pids.insert(child.unwrap().pid);
        loop {
            let previous = pids.len();
            for (pid, _, parent) in &rows {
                if pids.contains(parent) {
                    pids.insert(*pid);
                }
            }
            if pids.len() == previous {
                break;
            }
        }
    }
    let identities = pids
        .into_iter()
        .map(capture)
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .flatten()
        .collect();
    if live && child.map(alive).transpose()?.unwrap_or(false) {
        Ok((identities, vec![]))
    } else {
        Ok((vec![], identities))
    }
}

pub async fn stop_verified(targets: &[Identity]) -> Result<()> {
    for identity in targets {
        signal(identity, libc::SIGTERM)?;
    }
    if targets.is_empty() {
        return Ok(());
    }
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while tokio::time::Instant::now() < deadline && targets.iter().any(|p| alive(p).unwrap_or(true))
    {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    for identity in targets {
        signal(identity, libc::SIGKILL)?;
    }
    let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
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
        assert!(!scan.unreadable.contains(&identity));
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
