//! Processes running with workspace scope, recorded in the state directory by
//! their execution wrapper. Parent links and birth identities stay readable
//! where other processes' environments do not (macOS 27 hides those of its
//! own platform binaries, such as shells), so a descendant that cleared its
//! scope token still finds a recorded ancestor.
use super::identity::{self, Identity};
use crate::fsutil::{self, Permissions, ReplaceOptions};
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

const DIRECTORY: &str = "scoped-processes";

/// The records of one tracked command: its wrapper's while the command runs,
/// so the command can run nothing before an ancestor is recorded, and the
/// command's own while it lives, so its descendants stay scoped when the
/// wrapper hands it over. Dropping removes the records whose process ended.
pub struct Records {
    directory: PathBuf,
    wrapper: Identity,
    command: Option<Identity>,
}

impl Records {
    /// Record this process, the wrapper, before it spawns its command.
    pub fn wrapper(state: &Path) -> Result<Self> {
        let wrapper =
            identity::capture(std::process::id())?.context("cannot identify execution wrapper")?;
        let directory = state.join(DIRECTORY);
        std::fs::create_dir_all(&directory)
            .with_context(|| format!("create {}", directory.display()))?;
        write(&directory, &wrapper)?;
        Ok(Self {
            directory,
            wrapper,
            command: None,
        })
    }

    /// Record the spawned command, unless it has already exited.
    pub fn command(&mut self, pid: u32) -> Result<()> {
        if let Some(command) = identity::capture(pid)? {
            write(&self.directory, &command)?;
            self.command = Some(command);
        }
        Ok(())
    }
}

impl Drop for Records {
    fn drop(&mut self) {
        remove(&self.directory, &self.wrapper);
        if let Some(command) = &self.command
            && !identity::alive(command).unwrap_or(true)
        {
            remove(&self.directory, command);
        }
    }
}

/// True when an ancestor of this process is recorded in `state`.
pub fn recorded_ancestor(state: &Path) -> bool {
    any_recorded(state, identity::ancestors())
}

fn any_recorded(state: &Path, mut pids: impl Iterator<Item = u32>) -> bool {
    let directory = state.join(DIRECTORY);
    pids.any(|pid| {
        let Ok(Some(birth)) = fsutil::read_optional(&record(&directory, pid)) else {
            return false;
        };
        identity::capture(pid).is_ok_and(|live| live.is_some_and(|live| live.birth == birth))
    })
}

/// A reused PID replaces its ended predecessor's record.
fn write(directory: &Path, process: &Identity) -> Result<()> {
    let path = record(directory, process.pid);
    fsutil::replace_atomically(
        &path,
        process.birth.as_bytes(),
        ReplaceOptions {
            permissions: Permissions::Temporary,
            sync: false,
        },
    )
    .with_context(|| format!("record workspace scope in {}", path.display()))
}

/// Remove `process`'s record unless a later process with its PID replaced it.
fn remove(directory: &Path, process: &Identity) {
    let path = record(directory, process.pid);
    if fsutil::read_optional(&path).is_ok_and(|birth| birth.as_deref() == Some(&process.birth)) {
        let _ = std::fs::remove_file(path);
    }
}

fn record(directory: &Path, pid: u32) -> PathBuf {
    directory.join(pid.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_recorded_wrapper_scopes_its_descendants_until_dropped() -> Result<()> {
        let state = tempfile::tempdir()?;
        let records = Records::wrapper(state.path())?;
        assert!(any_recorded(state.path(), [std::process::id()].into_iter()));
        drop(records);
        assert!(!any_recorded(
            state.path(),
            [std::process::id()].into_iter()
        ));
        Ok(())
    }

    #[test]
    fn a_record_of_an_ended_process_does_not_match_its_reused_pid() -> Result<()> {
        let state = tempfile::tempdir()?;
        let directory = state.path().join(DIRECTORY);
        std::fs::create_dir_all(&directory)?;
        let mut ended = identity::capture(std::process::id())?.context("identify this process")?;
        ended.birth.push('0');
        write(&directory, &ended)?;
        assert!(!any_recorded(
            state.path(),
            [std::process::id()].into_iter()
        ));
        Ok(())
    }

    #[test]
    fn a_living_command_keeps_its_record_after_its_wrapper_drops() -> Result<()> {
        let state = tempfile::tempdir()?;
        // `cat` lives until its stdin closes, after the assertion.
        let mut command = std::process::Command::new("cat")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .spawn()?;
        let mut records = Records::wrapper(state.path())?;
        records.command(command.id())?;
        drop(records);
        let kept = any_recorded(state.path(), [command.id()].into_iter());
        drop(command.stdin.take());
        command.wait()?;
        assert!(kept);
        Ok(())
    }
}
