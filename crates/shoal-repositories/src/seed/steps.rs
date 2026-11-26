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
use shoal_core::process::usage::tree_memory;
use shoal_daemon_base::log;
use std::os::unix::process::ExitStatusExt as _;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use tokio::{process::Command, sync::watch};

use super::{
    build::{Build, Previous, Step, Toolchains},
    builds::Plan,
};
use crate::daemon::SeedAdmission;

/// Longest a seed build may run before it is killed and reported.
const BUILD_TIMEOUT: Duration = Duration::from_secs(2 * 60 * 60);

/// How often a running step's process tree is read.
const SAMPLE_INTERVAL: Duration = Duration::from_secs(1);

/// Protection stopped the seed build, which builds again once it fits.
#[derive(Debug)]
pub(crate) struct SeedStopped;

impl fmt::Display for SeedStopped {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("protection stopped the seed build")
    }
}

impl std::error::Error for SeedStopped {}

/// What a running seed build holds, which its admission hears, and the
/// most it held.
pub(super) struct BuildMemory<'a> {
    admission: &'a dyn SeedAdmission,
    peak: AtomicU64,
    /// A step ended other than by itself: a signal, its deadline, a cancel
    /// or protection's stop.
    cut_short: AtomicBool,
    /// The kernel's OOM killer killed a step.
    oom_killed: AtomicBool,
}

impl<'a> BuildMemory<'a> {
    pub(super) fn new(admission: &'a dyn SeedAdmission) -> Self {
        Self {
            admission,
            peak: AtomicU64::new(0),
            cut_short: AtomicBool::new(false),
            oom_killed: AtomicBool::new(false),
        }
    }

    /// The most the build held.
    pub(super) fn peak(&self) -> u64 {
        self.peak.load(Ordering::Relaxed)
    }

    /// Whether a step ended other than by itself.
    pub(super) fn cut_short(&self) -> bool {
        self.cut_short.load(Ordering::Relaxed)
    }

    /// Whether the kernel's OOM killer killed a step.
    pub(super) fn oom_killed(&self) -> bool {
        self.oom_killed.load(Ordering::Relaxed)
    }

    /// Record how a step ended: with `status`, `stopped` early, or with an
    /// OOM kill its control group counted between its start and its end,
    /// `kills`. The kill may hit a process below the step, such as `rustc`
    /// under a `cargo` that then exits by itself.
    fn step_ended(&self, status: ExitStatus, stopped: bool, kills: (Option<u64>, Option<u64>)) {
        let oom = matches!(kills, (Some(before), Some(after)) if after > before);
        if oom && !stopped {
            self.oom_killed.store(true, Ordering::Relaxed);
        } else if stopped || status.signal().is_some() {
            self.cut_short.store(true, Ordering::Relaxed);
        }
    }

    /// Read what the step `pid` and the processes below it hold, every
    /// [`SAMPLE_INTERVAL`] until dropped.
    async fn follow(&self, pid: u32) -> ! {
        let mut interval = tokio::time::interval(SAMPLE_INTERVAL);
        loop {
            interval.tick().await;
            let read = tokio::task::spawn_blocking(move || tree_memory(pid)).await;
            if let Ok(Ok(memory)) = read {
                self.peak.fetch_max(memory, Ordering::Relaxed);
                self.admission.using(memory);
            }
        }
    }
}

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
/// is not soft fails, the build times out, `cancel` is set or protection
/// stops it, telling `memory` what it holds.
pub(super) async fn run_build(
    plan: &Plan,
    previous: Option<&Previous>,
    (worktree, log): (&Path, &Path),
    cancel: watch::Receiver<bool>,
    memory: &BuildMemory<'_>,
) -> Result<Built> {
    let mut steps = Steps::new(plan, worktree, log, cancel, memory)?;
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
    /// Set once protection stopped the build.
    stop: watch::Receiver<bool>,
    memory: &'a BuildMemory<'a>,
}

impl<'a> Steps<'a> {
    fn new(
        plan: &'a Plan,
        worktree: &'a Path,
        log: &'a Path,
        cancel: watch::Receiver<bool>,
        memory: &'a BuildMemory<'a>,
    ) -> Result<Self> {
        Ok(Self {
            plan,
            worktree,
            log,
            output: fs::File::create(log).with_context(|| format!("create {}", log.display()))?,
            deadline: tokio::time::Instant::now() + BUILD_TIMEOUT,
            cancel,
            stop: memory.admission.stopped(),
            memory,
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
        self.memory.admission.runs_in(group as u32);
        // Read before the step is reaped, whose PID then names no group.
        let control_group = shoal_core::process::oom::group_of(group as u32)
            .ok()
            .flatten();
        let kills_before = oom_kills(control_group.as_deref());
        let stopped = tokio::select! {
            exited = tokio::time::timeout_at(self.deadline, exited(group)) => match exited {
                Ok(exited) => Ok(exited?),
                Err(_) => Err(anyhow::anyhow!(
                    "the seed build timed out after {} minutes in {}; output is in {}",
                    BUILD_TIMEOUT.as_secs() / 60,
                    step.label,
                    self.log.display()
                )),
            },
            _ = self.cancel.wait_for(|cancelled| *cancelled) => {
                Err(anyhow::anyhow!("seed build cancelled"))
            }
            _ = self.stop.wait_for(|stopped| *stopped) => Err(SeedStopped.into()),
            never = self.memory.follow(group as u32) => never,
        };
        // The leader is not reaped yet, so its ID still names this group
        // alone: nothing else can have taken it.
        // SAFETY: killpg takes no pointers; a group without members is ESRCH.
        unsafe { libc::killpg(group, libc::SIGKILL) };
        let status = child.wait().await?;
        let kills = (kills_before, oom_kills(control_group.as_deref()));
        self.memory.step_ended(status, stopped.is_err(), kills);
        group_gone(group).await;
        guard.0 = None;
        stopped?;
        Ok(status)
    }
}

/// How many processes the kernel's OOM killer killed in `group`, a step's
/// control group and the daemon's own, where it can be read.
fn oom_kills(group: Option<&std::path::Path>) -> Option<u64> {
    shoal_core::process::control_group::oom_kills(group?)
        .ok()
        .flatten()
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

#[cfg(test)]
mod tests;
