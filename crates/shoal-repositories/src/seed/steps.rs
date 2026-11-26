//! Running a seed build's commands in its worktree. Each runs in a process
//! group of its own, which is gone once the command ends.
use std::{
    fmt, fs,
    io::{Read as _, Seek as _, Write as _},
    path::{Path, PathBuf},
    process::{ExitStatus, Stdio},
    time::Duration,
};

use anyhow::{Context, Result, bail};
use shoal_daemon_base::log;
use tokio::{process::Command, sync::watch};

use super::{
    build::{Build, Previous, Step, Toolchains},
    builds::Plan,
};

/// Longest a seed build may run before it is killed and reported.
const BUILD_TIMEOUT: Duration = Duration::from_secs(2 * 60 * 60);

/// What a seed build that went to its end built with, and how its soft
/// steps failed.
pub(super) struct Built {
    pub(super) toolchains: Toolchains,
    pub(super) soft_failures: Vec<StepFailure>,
}

/// A step that ran to its end and exited non-zero.
pub(super) struct StepFailure {
    label: String,
    code: i32,
    log: PathBuf,
}

impl fmt::Display for StepFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self { label, code, log } = self;
        write!(
            f,
            "{label} exited with {code}; output is in {}",
            log.display()
        )
    }
}

/// Run the seed build in its worktree, writing its output to `log`: name
/// each preset's toolchain, reset the outputs `previous` left that this build
/// would not reuse, then run the build's commands in order, until one that
/// is not soft fails, the build times out or `cancel` is set.
pub(super) async fn run_build(
    plan: &Plan,
    previous: Option<&Previous>,
    worktree: &Path,
    log: &Path,
    cancel: watch::Receiver<bool>,
) -> Result<Built> {
    let mut steps = Steps::new(plan, worktree, log, cancel)?;
    let toolchains = identify_toolchains(&mut steps, &plan.build).await?;
    let resets = plan.build.resets(previous, &plan.settings()?, &toolchains);
    let mut soft_failures = Vec::new();
    for step in plan.build.steps(&resets) {
        soft_failures.extend(steps.run(&step).await?);
    }
    Ok(Built {
        toolchains,
        soft_failures,
    })
}

/// Run each preset's toolchain command and keep what it printed.
async fn identify_toolchains(steps: &mut Steps<'_>, build: &Build) -> Result<Toolchains> {
    let mut toolchains = Toolchains::new();
    for (preset, step) in build.toolchain_steps() {
        toolchains.insert(preset, steps.capture(&step).await?);
    }
    Ok(toolchains)
}

/// Runs a seed build's commands one at a time in its worktree, each in the
/// log, until the build's deadline.
struct Steps<'a> {
    plan: &'a Plan,
    worktree: &'a Path,
    log: &'a Path,
    output: fs::File,
    deadline: tokio::time::Instant,
    cancel: watch::Receiver<bool>,
}

impl<'a> Steps<'a> {
    fn new(
        plan: &'a Plan,
        worktree: &'a Path,
        log: &'a Path,
        cancel: watch::Receiver<bool>,
    ) -> Result<Self> {
        Ok(Self {
            plan,
            worktree,
            log,
            output: fs::File::create(log).with_context(|| format!("create {}", log.display()))?,
            deadline: tokio::time::Instant::now() + BUILD_TIMEOUT,
            cancel,
        })
    }

    /// Run `step`, writing its output to the log. Returns how a soft step
    /// failed; any other failure is an error.
    async fn run(&mut self, step: &Step) -> Result<Option<StepFailure>> {
        writeln!(self.output, "$ {}", step.label)?;
        let stdout = self.output.try_clone()?;
        let status = self.wait(step, &stdout).await?;
        match self.failure(step, status) {
            Some(failure) if !step.soft => bail!("{failure}"),
            soft => Ok(soft),
        }
    }

    /// Run `step` and return what it printed to standard output, which the
    /// log gets too.
    async fn capture(&mut self, step: &Step) -> Result<String> {
        writeln!(self.output, "$ {}", step.label)?;
        let mut stdout = tempfile::tempfile()?;
        let waited = self.wait(step, &stdout).await;
        let mut printed = Vec::new();
        stdout.seek(std::io::SeekFrom::Start(0))?;
        stdout.read_to_end(&mut printed)?;
        self.output.write_all(&printed)?;
        if let Some(failure) = self.failure(step, waited?) {
            bail!("{failure}");
        }
        Ok(String::from_utf8_lossy(&printed).into_owned())
    }

    /// How `step` failed when it exited with `status`, or `None` when it
    /// succeeded.
    fn failure(&self, step: &Step, status: ExitStatus) -> Option<StepFailure> {
        (!status.success()).then(|| StepFailure {
            label: step.label.clone(),
            code: shoal_core::execution::exit_code(status),
            log: self.log.to_owned(),
        })
    }

    /// Run `step`, its standard output to `stdout` and its errors to the
    /// log, until it exits, the build's deadline passes or `cancel` is set,
    /// and return how it exited. Whatever it leaves in its process group is
    /// killed before this returns.
    async fn wait(&mut self, step: &Step, stdout: &fs::File) -> Result<ExitStatus> {
        let mut child = step_command(step, self.plan, self.worktree, stdout, &self.output)?
            .spawn()
            .with_context(|| format!("launch {}", step.label))?;
        let group = child.id().context("seed build process ID unavailable")? as i32;
        // Dropping the build, as daemon shutdown does, kills its whole group.
        let mut guard = GroupGuard(Some(group));
        let stopped = tokio::select! {
            exited = tokio::time::timeout_at(self.deadline, exited(group)) => match exited {
                Ok(exited) => Ok(exited?),
                Err(_) => Err(format!(
                    "the seed build timed out after {} minutes in {}; output is in {}",
                    BUILD_TIMEOUT.as_secs() / 60,
                    step.label,
                    self.log.display()
                )),
            },
            _ = self.cancel.wait_for(|cancelled| *cancelled) => {
                Err("seed build cancelled".to_owned())
            }
        };
        // The leader is not reaped yet, so its ID still names this group
        // alone: nothing else can have taken it.
        // SAFETY: killpg takes no pointers; a group without members is ESRCH.
        unsafe { libc::killpg(group, libc::SIGKILL) };
        let status = child.wait().await?;
        group_gone(group).await;
        guard.0 = None;
        if let Err(reason) = stopped {
            bail!(reason);
        }
        Ok(status)
    }
}

/// `step` as its own process group in the seed worktree, with the
/// repository's `[env]` and none of the daemon's Shoal variables or secrets,
/// printing to `stdout` and `stderr`.
fn step_command(
    step: &Step,
    plan: &Plan,
    worktree: &Path,
    stdout: &fs::File,
    stderr: &fs::File,
) -> Result<Command> {
    let (program, args) = step.argv.split_first().context("empty seed command")?;
    let mut command = Command::new(program);
    command.args(args);
    // The daemon's own Shoal variables describe no workspace of the seed's.
    for (name, _) in std::env::vars_os() {
        if name.to_string_lossy().starts_with(shoal_core::env::PREFIX) {
            command.env_remove(name);
        }
    }
    for name in shoal_core::env::inherited_secret_exports() {
        command.env_remove(name);
    }
    if let Some(path) = shoal_core::env::path_without_secret_tools() {
        command.env("PATH", path);
    }
    shoal_core::env::apply_configured(&mut command, &plan.environment);
    command
        .current_dir(worktree)
        .stdin(Stdio::null())
        .stdout(stdout.try_clone()?)
        .stderr(stderr.try_clone()?)
        .process_group(0)
        .kill_on_drop(true);
    Ok(command)
}

/// Kills a seed build's process group when dropped before the build has
/// confirmed the group is empty. Until then a member or the unreaped leader
/// keeps the group ID, so the signal cannot reach another process.
struct GroupGuard(Option<i32>);

impl Drop for GroupGuard {
    fn drop(&mut self) {
        if let Some(group) = self.0 {
            // SAFETY: killpg takes no pointers.
            unsafe { libc::killpg(group, libc::SIGKILL) };
        }
    }
}

/// How long killed members of a seed build's group may take to exit before
/// the wait is logged.
const GROUP_EXIT_WARNING: Duration = Duration::from_secs(30);

/// Wait until no live process remains in `group`, so nothing writes to the
/// seed worktree once this returns; the caller keeps the seed slot meanwhile,
/// so removal waits too. A zombie counts as gone: it runs no code, and in a
/// container without an init process nothing may ever reap it.
async fn group_gone(group: i32) {
    let started = tokio::time::Instant::now();
    let mut warned = false;
    let mut check_failed = false;
    loop {
        match shoal_core::process::identity::group_alive(group as u32).await {
            Ok(false) => return,
            Ok(true) => {
                if !warned && started.elapsed() >= GROUP_EXIT_WARNING {
                    log!("seed build group {group} survives SIGKILL; waiting for it to exit");
                    warned = true;
                }
            }
            // A failed check counts as alive, so nothing writes once this
            // returns.
            Err(error) => {
                if !check_failed {
                    log!(
                        "cannot check seed build group {group}; waiting for it to exit: {error:#}"
                    );
                    check_failed = true;
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// Wait for the process `pid` to exit without reaping it.
async fn exited(pid: i32) -> Result<()> {
    tokio::task::spawn_blocking(move || {
        loop {
            // SAFETY: `info` is a valid, writable siginfo_t for waitid to fill.
            let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
            let flags = libc::WEXITED | libc::WNOWAIT;
            if unsafe { libc::waitid(libc::P_PID, pid as libc::id_t, &mut info, flags) } == 0 {
                return Ok(());
            }
            let error = std::io::Error::last_os_error();
            if error.kind() != std::io::ErrorKind::Interrupted {
                return Err(error).context("wait for setup.seed.command");
            }
        }
    })
    .await?
}
