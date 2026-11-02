//! The subreaper that adopts this user's orphans on Linux. An orphan goes to
//! its nearest ancestor that set `PR_SET_CHILD_SUBREAPER`, such as
//! `systemd --user` or a container's init, and to init only when it has none.
//! `/proc` shows no such flag, so the process orphans one of its own and reads
//! which process adopted it; agents and the daemon run under the same one.
use super::{Identity, proc_lineage, signal};
use crate::protocol::timing;
use anyhow::{Context, Result};
use std::sync::{Mutex, PoisonError};

/// What adopted the last probe. Orphans go to the next subreaper above only
/// once the one they went to exits, and to init once none is left.
static FOUND: Mutex<Option<Adopter>> = Mutex::new(None);

#[derive(Clone)]
enum Adopter {
    Init,
    Subreaper(Identity),
}

/// The PID of the subreaper that adopts this process's orphans, or `None`
/// when init adopts them.
pub(super) async fn current() -> Result<Option<u32>> {
    let found = FOUND.lock().unwrap_or_else(PoisonError::into_inner).clone();
    let adopter = match found {
        Some(adopter) if adopter.adopts()? => adopter,
        _ => {
            let adopter = probe().await?;
            *FOUND.lock().unwrap_or_else(PoisonError::into_inner) = Some(adopter.clone());
            adopter
        }
    };
    Ok(match adopter {
        Adopter::Init => None,
        Adopter::Subreaper(identity) => Some(identity.pid),
    })
}

impl Adopter {
    /// Whether orphans still go to it: init for good, a subreaper while it
    /// lives.
    fn adopts(&self) -> Result<bool> {
        Ok(match self {
            Adopter::Init => true,
            Adopter::Subreaper(identity) => identify(identity.pid)?.as_ref() == Some(identity),
        })
    }
}

/// Orphan a process, read which process adopted it, and stop it.
async fn probe() -> Result<Adopter> {
    let mut command = tokio::process::Command::new("sh");
    command
        .args(["-c", "sleep 60 >/dev/null 2>&1 & echo $!"])
        .current_dir("/");
    let output = crate::subprocess::Run::new(command)
        .timeout(timing::PROCESS_INVENTORY_TIMEOUT)
        .output()
        .await
        .context("orphan a process to find its subreaper")?;
    let orphan = output
        .trim()
        .parse()
        .context("invalid orphaned process ID")?;
    // The shell has exited, and the kernel hands its children to the adopter
    // before then.
    let orphan = proc_lineage(orphan, |_| true)?.context("the orphaned process exited")?;
    signal(&orphan.identity, libc::SIGKILL)?;
    if orphan.parent_pid == 1 {
        return Ok(Adopter::Init);
    }
    let subreaper = identify(orphan.parent_pid)?.context("the subreaper exited")?;
    Ok(Adopter::Subreaper(subreaper))
}

/// The identity of the live process `pid`, whichever user runs it.
fn identify(pid: u32) -> Result<Option<Identity>> {
    Ok(proc_lineage(pid, |_| true)?.map(|lineage| lineage.identity))
}
