//! Same-user process ownership for cooperative recovery. Never expose process
//! arguments/environment; only the explicit execution marker leaves this module.
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{collections::HashSet, time::Duration};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Identity {
    pub pid: u32,
    pub birth: String,
}

pub fn capture(pid: u32) -> Result<Option<Identity>> {
    ensure!(pid > 1 && pid <= i32::MAX as u32, "invalid process ID");
    platform_identity(pid)
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
    let mut command = tokio::process::Command::new("ps");
    command.args(["-ax", "-o", "pid=,uid="]).env("LC_ALL", "C");
    let output = tokio::time::timeout(Duration::from_secs(10), crate::subprocess::output(command))
        .await
        .context("process inventory timed out")??;
    let marker = format!("{}=", crate::env::EXECUTION_ID).into_bytes();
    tokio::task::spawn_blocking(move || {
        let mut scan = Scan::default();
        for line in output.lines() {
            let fields: Vec<_> = line.split_whitespace().collect();
            ensure!(fields.len() == 2, "invalid process inventory");
            let pid: u32 = fields[0].parse()?;
            let uid: u32 = fields[1].parse()?;
            if uid != unsafe { libc::geteuid() } || pid <= 1 || pid == std::process::id() {
                continue;
            }
            let Some(identity) = capture(pid)? else {
                continue;
            };
            match environment(pid) {
                Ok(environment) if !environment.is_empty() => {
                    if let Some(id) = environment
                        .iter()
                        .find_map(|entry| entry.strip_prefix(marker.as_slice()))
                        .and_then(|id| std::str::from_utf8(id).ok())
                        .filter(|id| ids.contains(*id))
                    {
                        if alive(&identity)? {
                            scan.processes.push(OwnedProcess {
                                execution_id: id.into(),
                                identity,
                            });
                        }
                    }
                }
                Ok(_) | Err(_) if alive(&identity)? => scan.unreadable.push(identity),
                Ok(_) | Err(_) => {}
            }
        }
        Ok(scan)
    })
    .await
    .context("process inventory worker failed")?
}

#[cfg(target_os = "macos")]
fn platform_identity(pid: u32) -> Result<Option<Identity>> {
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
    let info = unsafe { info.assume_init() };
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
    let stat = match std::fs::read_to_string(format!("{path}/stat")) {
        Ok(stat) => stat,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let fields: Vec<_> = stat
        .rsplit_once(')')
        .context("invalid process stat")?
        .1
        .split_whitespace()
        .collect();
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
    let mut command = tokio::process::Command::new("ps");
    command
        .args(["-ax", "-o", "pid=,uid=,pgid=,ppid="])
        .env("LC_ALL", "C");
    let output =
        tokio::time::timeout(Duration::from_secs(10), crate::subprocess::output(command)).await??;
    let mut rows = Vec::new();
    for line in output.lines() {
        let fields: Vec<u32> = line
            .split_whitespace()
            .map(str::parse)
            .collect::<std::result::Result<_, _>>()?;
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
    #[tokio::test]
    async fn discovers_marked_processes_and_rejects_changed_identity() {
        let id = uuid::Uuid::new_v4().to_string();
        let mut child = tokio::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "process_identity::tests::marked_child",
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
