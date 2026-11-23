//! Tracked executions that outlive the process starting them, with their
//! output in a log.
use shoal_ids::execution_ids::ExecutionId;
use shoal_protocol::executions::TrackedAgent;
use std::{
    ffi::OsString,
    io::Write,
    path::{Path, PathBuf},
    process::Stdio,
};

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::Child,
    time::timeout,
};

use crate::{
    env,
    model::ExecutionSpace,
    paths::Paths,
    protocol::timing,
    workers::{Worker, detached_command, internal_command},
};

/// Start `command` in `space`, a workspace or a chat, as a tracked
/// execution that outlives this process: a new session running this
/// binary's detached wrapper as `wrapper` says, with the command's output in
/// `log`, and `agent` naming the shortcut for the exit notification. Returns
/// once the daemon has recorded the launch.
pub async fn launch_detached(
    paths: &Paths,
    space: &ExecutionSpace,
    log: PathBuf,
    command: Vec<OsString>,
    wrapper: &Wrapper<'_>,
    agent: Option<&TrackedAgent>,
) -> Result<DetachedExecution> {
    let launch = DetachedCommand::prepare(paths, space, log, command, wrapper, agent)?;
    launch.start().await
}

/// How the wrapper of a detached execution starts.
#[derive(Default)]
pub struct Wrapper<'a> {
    /// Inherited variables removed from its environment.
    pub clear: &'a [&'a str],
    /// Variables added to its environment.
    pub env: &'a [(&'a str, String)],
}

/// A detached execution this process launched, whose wrapper it can still
/// stop.
pub struct DetachedExecution {
    launch: DetachedLaunch,
    wrapper: Child,
}

impl DetachedExecution {
    /// Leave the execution running and return its launch. The wrapper is
    /// this process's child, so a task waits on it to reap it once it exits:
    /// dropping it leaves that to Tokio's orphan queue, which has left the
    /// daemon's wrappers zombies for days.
    pub fn detach(mut self) -> DetachedLaunch {
        tokio::spawn(async move {
            if let Err(error) = self.wrapper.wait().await {
                eprintln!("warning: could not reap the execution wrapper: {error}");
            }
        });
        self.launch
    }

    /// Interrupt the execution as Ctrl-C interrupts a command, which stops
    /// it without saving it for `shoal resume`, and wait until its wrapper
    /// reported the end and exited. The wrapper is this process's child, not
    /// yet reaped, so its ID names no other process.
    pub async fn interrupt(mut self) -> Result<()> {
        let pid = self
            .wrapper
            .id()
            .context("the execution wrapper was reaped")?;
        let pid = libc::pid_t::try_from(pid).context("wrapper process ID out of range")?;
        // SAFETY: kill takes no pointers.
        if unsafe { libc::kill(pid, libc::SIGINT) } == -1 {
            return Err(std::io::Error::last_os_error()).context("interrupt the execution wrapper");
        }
        self.wrapper
            .wait()
            .await
            .context("wait for the execution wrapper")?;
        Ok(())
    }
}

/// What a detached wrapper reports on stdout once the daemon has recorded the
/// command's process group; the CLI that spawned it returns after reading it.
#[derive(Debug, Serialize, Deserialize)]
pub struct DetachedLaunch {
    pub execution_id: ExecutionId,
    pub pid: u32,
    pub log: PathBuf,
}

/// Tell the spawning CLI that the launch is recorded, then point stdout at the
/// log (the wrapper's stderr) so nothing else ever writes to the closed pipe.
pub fn report_launch(launch: &DetachedLaunch) -> Result<()> {
    let mut stdout = std::io::stdout().lock();
    serde_json::to_writer(&mut stdout, launch)?;
    stdout.write_all(b"\n")?;
    stdout.flush()?;
    // SAFETY: duplicating one open descriptor onto another in this process.
    ensure!(
        unsafe { libc::dup2(libc::STDERR_FILENO, libc::STDOUT_FILENO) } != -1,
        "redirect wrapper output to the log"
    );
    Ok(())
}

struct DetachedCommand {
    log: PathBuf,
    child: Child,
}

impl DetachedCommand {
    fn prepare(
        paths: &Paths,
        space: &ExecutionSpace,
        log: PathBuf,
        command: Vec<OsString>,
        wrapper: &Wrapper<'_>,
        agent: Option<&TrackedAgent>,
    ) -> Result<Self> {
        ensure!(space.path.is_dir(), "{} directory is missing", space.kind);
        let file = prepare_detached_log(&log, space, &command)?;
        let command = internal_command(
            paths,
            false,
            Worker::Detached {
                workspace: &space.id,
                log: &log,
                agent,
                command: &command,
            },
        )?;
        let child = spawn_detached_wrapper(&command, file, wrapper)?;
        Ok(Self { log, child })
    }

    async fn start(mut self) -> Result<DetachedExecution> {
        let stdout = self
            .child
            .stdout
            .take()
            .context("wrapper stdout unavailable")?;
        let report = timeout(
            timing::DETACHED_LAUNCH_TIMEOUT,
            BufReader::new(stdout).lines().next_line(),
        )
        .await
        .context("detached launch was not recorded in time")??;
        match report {
            Some(line) => Ok(DetachedExecution {
                launch: serde_json::from_str(&line).context("unexpected detached launch report")?,
                wrapper: self.child,
            }),
            None => Err(detached_launch_failure(&self.log, &mut self.child).await),
        }
    }
}

fn prepare_detached_log(
    log: &Path,
    space: &ExecutionSpace,
    command: &[OsString],
) -> Result<std::fs::File> {
    if let Some(parent) = log.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log)
        .with_context(|| format!("open {}", log.display()))?;
    writeln!(
        file,
        "shoal: starting {} in {}",
        command
            .iter()
            .map(|arg| arg.to_string_lossy())
            .collect::<Vec<_>>()
            .join(" "),
        space.path.display()
    )?;
    Ok(file)
}

fn spawn_detached_wrapper(
    argv: &[OsString],
    file: std::fs::File,
    wrapper: &Wrapper<'_>,
) -> Result<Child> {
    let mut command = detached_command(argv);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::from(file))
        .env_remove(env::SHELL_DIRECTIVE);
    // A launch from inside another launcher's session must not inherit its
    // attachment; the caller names what to drop and what to set.
    for name in wrapper.clear {
        command.env_remove(name);
    }
    command.envs(wrapper.env.iter().map(|(name, value)| (*name, value)));
    if let Some(entry) = crate::env::queue_entry() {
        command.env(env::QUEUE_ENTRY, entry);
    }
    command.spawn().context("launch detached execution wrapper")
}

async fn detached_launch_failure(log: &Path, child: &mut Child) -> anyhow::Error {
    let status = match timeout(timing::DETACHED_EXIT_TIMEOUT, child.wait()).await {
        Ok(Ok(status)) => format!("wrapper exited with {status}"),
        _ => "wrapper still running".into(),
    };
    anyhow::anyhow!(
        "detached launch failed ({status}); see {}\n{}",
        log.display(),
        log_tail(log)
    )
}

/// The last lines of a log, for an error message.
fn log_tail(log: &Path) -> String {
    let text = std::fs::read_to_string(log).unwrap_or_default();
    let lines: Vec<_> = text.lines().rev().take(10).collect();
    lines.into_iter().rev().collect::<Vec<_>>().join("\n")
}

#[cfg(test)]
mod tests;
