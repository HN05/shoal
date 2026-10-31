//! One-shot agent runs: an AI tool's command started in the background,
//! without a session or terminal, in a directory its caller chose. It runs
//! as a process group of its own, which is killed when the run times out, is
//! cancelled or is dropped, and its output goes to files in a run directory.
use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use anyhow::{Context, Result};
use tokio::{process::Command, sync::watch};

use crate::{agent::ModelChoice, config::Effective};

/// An agent's command for one run.
pub(crate) struct AgentCommand {
    pub argv: Vec<OsString>,
    /// Variables added to the daemon's own, without its Shoal variables.
    pub environment: BTreeMap<String, OsString>,
    pub remove: BTreeSet<String>,
}

/// The one-shot command of `agent` running with `model` and `args`, the
/// arguments of one of its templates: its launcher's words before `{args}`,
/// the arguments that select the model and effort, then `args`, with its
/// launch environment over `configured`.
pub(crate) fn agent_command(
    settings: &Effective,
    agent: &str,
    model: &ModelChoice,
    args: Vec<OsString>,
    configured: &BTreeMap<String, OsString>,
    home: &Path,
) -> Result<AgentCommand> {
    let tool = settings
        .tools
        .get(agent)
        .with_context(|| format!("no AI tool runs {agent}"))?;
    let launcher = settings
        .launchers
        .get(agent)
        .with_context(|| format!("{agent} has no launcher"))?;
    let selection = tool
        .launch
        .arguments
        .model_args(model)
        .map_err(|template| anyhow::anyhow!("{agent} has no {template} for {model:?}"))?;
    let launch = tool.launch.environment_with(model);
    let mut environment = configured.clone();
    launch.layer(&mut environment, home);
    Ok(AgentCommand {
        argv: crate::ai::oneshot_command(launcher, selection.into_iter().chain(args).collect()),
        environment,
        remove: launch.remove,
    })
}

/// The run directories of one-shot runs whose processes may still be running:
/// from a run's start until what it left is proven stopped.
#[derive(Default)]
pub(crate) struct Runs(std::sync::Mutex<HashSet<PathBuf>>);

impl Runs {
    /// Stop what every run that has not ended cleanly left, as the daemon
    /// stops after aborting them; a run whose processes are proven stopped
    /// is forgotten.
    pub(crate) async fn stop_all(&self) {
        let unfinished: Vec<PathBuf> = self.0.lock().unwrap().iter().cloned().collect();
        for files in unfinished {
            match stop_leftovers(&files).await {
                Ok(()) => {
                    self.0.lock().unwrap().remove(&files);
                }
                Err(error) => crate::daemon::log!(
                    "stopping what the run in {} left: {error:#}",
                    files.display()
                ),
            }
        }
    }

    fn start(&self, files: &Path) {
        self.0.lock().unwrap().insert(files.to_owned());
    }

    fn end(&self, files: &Path) {
        self.0.lock().unwrap().remove(files);
    }
}

/// A run whose processes in other process groups could not be proven
/// stopped, so its tracking must outlive it.
#[derive(Debug)]
pub(crate) struct LeftRunning;

impl std::fmt::Display for LeftRunning {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("the run's processes could not be proven stopped")
    }
}

/// One run of a command.
pub(crate) struct Run<'a> {
    /// Where the run is tracked until what it left is stopped.
    pub runs: &'a Runs,
    pub command: &'a AgentCommand,
    /// Where the command runs.
    pub directory: &'a Path,
    /// Where its standard output and error go, which also marks its
    /// processes; see [`stop_leftovers`].
    pub files: &'a Path,
    pub timeout: Duration,
}

/// How a run ended.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Ended {
    Exited { success: bool },
    TimedOut,
    Cancelled,
}

/// Run until the command exits, the run's timeout passes or `cancel` turns
/// true; then kill whatever is left of its process group, and stop what it
/// started in other groups, which still carries its marker. An error means
/// it could not be started or waited for, or, as [`LeftRunning`], that what
/// it left could not be stopped, so the run stays tracked.
pub(crate) async fn run(run: &Run<'_>, cancel: &mut watch::Receiver<bool>) -> Result<Ended> {
    run.runs.start(run.files);
    let ended = run_group(run, cancel).await;
    stop_leftovers(run.files)
        .await
        .map_err(|error| error.context(LeftRunning))?;
    run.runs.end(run.files);
    ended
}

/// [`run`] for the command's own process group.
async fn run_group(run: &Run<'_>, cancel: &mut watch::Receiver<bool>) -> Result<Ended> {
    let mut child = command(run)?.spawn().context("start the agent")?;
    let group = child.id().context("agent process ID unavailable")? as i32;
    // Dropping the run, as a daemon stop does, kills its whole group.
    let mut guard = GroupGuard(Some(group));
    // The leader stays unreaped until its group is killed, so its ID names
    // this group alone when the signal is sent.
    let ended = tokio::select! {
        exited = tokio::time::timeout(run.timeout, exited_unreaped(group)) => match exited {
            Ok(exited) => exited.map(|()| None).map_err(anyhow::Error::from),
            Err(_) => Ok(Some(Ended::TimedOut)),
        },
        _ = cancel.wait_for(|cancelled| *cancelled) => Ok(Some(Ended::Cancelled)),
    };
    // SAFETY: killpg takes no pointers; a group without members is ESRCH.
    unsafe { libc::killpg(group, libc::SIGKILL) };
    let status = child.wait().await;
    // Once reaped, the ID may name another group.
    guard.0 = None;
    match ended? {
        Some(stopped) => Ok(stopped),
        None => Ok(Ended::Exited {
            success: status?.success(),
        }),
    }
}

/// Where a run in `files` writes its standard output.
pub(crate) fn stdout(files: &Path) -> PathBuf {
    files.join("stdout")
}

/// Where a run in `files` writes its standard error.
pub(crate) fn stderr(files: &Path) -> PathBuf {
    files.join("stderr")
}

/// Stop the processes the run in `files` left, as a stopped daemon's run or
/// in another process group, wherever they work: each verified by identity
/// and carrying the run's marker, or started by one that did. Anyone else's
/// process, such as a shell in the run's directory, is left alone.
pub(crate) async fn stop_leftovers(files: &Path) -> Result<()> {
    let run = files.to_string_lossy().into_owned();
    let left = crate::process::identity::scan_oneshot_runs([run].into()).await?;
    if !left.is_empty() {
        crate::daemon::log!(
            "stopping {} agent processes the run in {} left",
            left.len(),
            files.display()
        );
    }
    crate::process::identity::stop_verified(&left).await
}

/// How often a running command is checked for having exited.
const EXIT_POLL: Duration = Duration::from_millis(100);

/// Wait for the child `pid` to exit without reaping it, so its ID still
/// names its process group until the caller reaps it. It polls rather than
/// blocking a thread, so a paused clock can drive the timeout in tests.
async fn exited_unreaped(pid: i32) -> std::io::Result<()> {
    while !has_exited(pid)? {
        tokio::time::sleep(EXIT_POLL).await;
    }
    Ok(())
}

/// Whether the child `pid` has exited, leaving it unreaped.
fn has_exited(pid: i32) -> std::io::Result<bool> {
    loop {
        // SAFETY: `info` is a valid, writable siginfo_t for waitid to fill.
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        let flags = libc::WEXITED | libc::WNOWAIT | libc::WNOHANG;
        if unsafe { libc::waitid(libc::P_PID, pid as libc::id_t, &mut info, flags) } == 0 {
            // SAFETY: waitid filled `info`; without an exited child it zeroes the PID.
            return Ok(unsafe { info.si_pid() } != 0);
        }
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

/// The command as its own process group in the run's directory, with the
/// configured variables and none of the daemon's Shoal variables or secrets.
fn command(run: &Run) -> Result<Command> {
    let (program, args) = run
        .command
        .argv
        .split_first()
        .context("empty agent command")?;
    let mut command = Command::new(program);
    command.args(args);
    for (name, _) in std::env::vars_os() {
        if name.to_string_lossy().starts_with(crate::env::PREFIX) {
            command.env_remove(name);
        }
    }
    for name in crate::env::inherited_secret_exports() {
        command.env_remove(name);
    }
    if let Some(path) = crate::env::path_without_secret_tools() {
        command.env("PATH", path);
    }
    crate::env::apply_configured(&mut command, &run.command.environment);
    for name in &run.command.remove {
        command.env_remove(name);
    }
    command.env(crate::env::ONESHOT_RUN, run.files);
    command
        .current_dir(run.directory)
        .stdin(Stdio::null())
        .stdout(fs::File::create(stdout(run.files))?)
        .stderr(fs::File::create(stderr(run.files))?)
        .process_group(0)
        .kill_on_drop(true);
    Ok(command)
}

/// The file's bytes, unless it is missing, as from a command that wrote
/// none, or larger than `limit`.
pub(crate) fn read_bounded(path: &Path, limit: u64) -> std::io::Result<Option<Vec<u8>>> {
    match fs::metadata(path) {
        Ok(metadata) if metadata.len() > limit => Ok(None),
        Ok(_) => fs::read(path).map(Some),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

/// The last `limit` bytes of the file.
pub(crate) fn read_tail(path: &Path, limit: u64) -> std::io::Result<Vec<u8>> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = fs::File::open(path)?;
    let length = file.metadata()?.len();
    file.seek(SeekFrom::Start(length.saturating_sub(limit)))?;
    let mut tail = Vec::new();
    file.take(limit).read_to_end(&mut tail)?;
    Ok(tail)
}

/// Kills a run's process group when dropped before its leader is reaped,
/// while the ID still names that group alone.
struct GroupGuard(Option<i32>);

impl Drop for GroupGuard {
    fn drop(&mut self) {
        if let Some(group) = self.0 {
            // SAFETY: killpg takes no pointers.
            unsafe { libc::killpg(group, libc::SIGKILL) };
        }
    }
}

#[cfg(test)]
mod tests;
