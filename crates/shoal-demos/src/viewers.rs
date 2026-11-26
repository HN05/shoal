//! Viewers of simulator demos. A viewer carries the demo's execution's
//! marker, so stopping, removal and recovery stop it with the demo's other
//! processes.
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    ffi::OsString,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{Arc, Mutex},
    time::Duration,
};

use anyhow::{Context as _, Result};
use rusqlite::{Connection, OptionalExtension, params};
use shoal_core::config::repo::ConflictPolicy;
use shoal_daemon_base::callers::Caller;
use shoal_ids::execution_ids::ExecutionId;
use shoal_protocol::demos::Running;
use tokio::{
    net::TcpStream,
    process::{Child, Command},
    sync::Mutex as AsyncMutex,
    time::Instant,
};

use crate::{
    Demos,
    daemon::{Daemon, Executions, Joining, Ports, Reserved},
    env, log,
    model::{ExecutionSpace, PortRequest},
    paths::{Paths, StateFiles as _},
    process::identity::{self as process, Identity, IdentityExt as _},
    refusal::{Reason, Refusal},
};

/// How often a starting viewer is checked for its port.
pub(crate) const START_POLL: Duration = Duration::from_millis(100);
/// A hang guard for a viewer that never takes a connection.
const START_LIMIT: Duration = Duration::from_secs(60);
/// How many checks, [`START_POLL`] apart, a stopped viewer's group gets to
/// end in.
const GROUP_SETTLE_POLLS: u32 = 50;
/// How many of a viewer's last lines of errors its failure shows, and how
/// many bytes those may take at most.
const ERROR_LINES: usize = 10;
const ERROR_BYTES: u64 = 4096;
/// The default viewer's program, and what installs it.
const SERVE_SIM: (&str, &str) = ("serve-sim", "npm install -g serve-sim");

/// The locks of demos' viewers, by the execution that runs each demo.
#[derive(Default)]
pub(crate) struct Viewers {
    /// Held while a viewer starts, so two opens of one demo start one.
    starts: Locks,
    /// Held from a viewer's spawn until its record commits, and while the
    /// demo's survivors are stopped, so they never include an unrecorded
    /// viewer.
    spawns: Locks,
}

type Locks = Mutex<HashMap<ExecutionId, Arc<AsyncMutex<()>>>>;

/// A viewer the daemon started: the port it serves on, its process, which
/// leads a process group of its own, and whether it serves yet.
pub struct Viewer {
    pub port: u16,
    pub process: Identity,
    pub serving: bool,
}

impl Viewers {
    /// The lock a demo `execution` runs holds while its survivors stop or
    /// its viewer spawns.
    pub(crate) fn spawn_lock(&self, execution: &ExecutionId) -> Arc<AsyncMutex<()>> {
        lock(&self.spawns, execution)
    }

    /// Forget the demo `execution` ran, whose viewer record went with it.
    pub(crate) fn end(&self, execution: &ExecutionId) {
        for locks in [&self.starts, &self.spawns] {
            let mut locks = locks.lock().unwrap_or_else(|error| error.into_inner());
            locks.remove(execution);
        }
    }
}

fn lock(locks: &Locks, execution: &ExecutionId) -> Arc<AsyncMutex<()>> {
    let mut locks = locks.lock().unwrap_or_else(|error| error.into_inner());
    locks.entry(execution.clone()).or_default().clone()
}

impl Demos {
    /// The port the viewer of the simulator demo `demo` serves on, started
    /// on the demo's first open, as `caller` reserves it; refused for a
    /// demo whose viewer is off.
    pub async fn open_viewer(
        &self,
        daemon: &dyn Daemon,
        demo: &Running,
        caller: Option<&Caller>,
    ) -> Result<u16> {
        let name = &demo.launch.name;
        if demo.launch.viewer_command.is_none() {
            let message =
                format!("demo {name} has its viewer turned off; open it on its own machine");
            return Err(Refusal::new(Reason::NotReady, message).into());
        }
        let start = lock(&self.viewers.starts, &demo.execution_id);
        let _starting = start.lock().await;
        if let Some(port) = self.reuse_recorded_viewer(demo).await? {
            return Ok(port);
        }
        // Reserved first, so the environment exports it.
        let port = reserve_viewer_port(daemon, demo, caller).await?;
        refuse_occupied(port, &demo.launch.name).await?;
        let place = self.viewer_place(daemon, demo).await?;
        let starting = (self.spawn_recorded_viewer(daemon, demo, port, &place)).await?;
        let process = starting.process.clone();
        if let Err(error) = starting.serve(port).await {
            self.forget_viewer(&demo.execution_id).await;
            return Err(error);
        }
        self.record_serving(
            &demo.execution_id,
            Viewer {
                port,
                process,
                serving: true,
            },
        )
        .await?;
        Ok(port)
    }

    /// Spawn `demo`'s viewer on `port` at `place` and record it as starting,
    /// while no stop of the demo's survivors can take it for one.
    async fn spawn_recorded_viewer(
        &self,
        daemon: &dyn Executions,
        demo: &Running,
        port: u16,
        place: &Place,
    ) -> Result<Starting> {
        let spawning = self.viewers.spawn_lock(&demo.execution_id);
        let _spawning = spawning.lock().await;
        if daemon.completing(&demo.execution_id) {
            let message = format!("demo {} is stopping", demo.launch.name);
            return Err(Refusal::new(Reason::NotReady, message).into());
        }
        let starting = Starting::spawn(demo, port, place)?;
        let viewer = Viewer {
            port,
            process: starting.process.clone(),
            serving: false,
        };
        if let Err(error) = self.record_viewer(&demo.execution_id, &viewer).await {
            starting.stop().await;
            return Err(error);
        }
        Ok(starting)
    }

    /// Wait until the viewer `process` an earlier daemon started for `demo`
    /// serves, then record that it does.
    async fn await_recorded_viewer(&self, demo: &Running, viewer: Viewer) -> Result<()> {
        let tool = demo.launch.viewer(None).map(|viewer| viewer.tool);
        let program = tool.as_deref().unwrap_or("the viewer");
        let exited = || Ok((!runs(&viewer.process)).then(|| "exited".to_owned()));
        if let Err(error) = await_port(viewer.port, program, exited).await {
            // Stopped and forgotten, so the next open starts another.
            self.retire_viewer(demo, &viewer).await?;
            return Err(error);
        }
        self.record_serving(&demo.execution_id, viewer).await
    }

    /// Record that `viewer` serves.
    async fn record_serving(&self, execution: &ExecutionId, viewer: Viewer) -> Result<()> {
        let serving = Viewer {
            serving: true,
            ..viewer
        };
        self.record_viewer(execution, &serving).await
    }

    /// The viewer of the demo `execution` runs, while its process runs.
    pub async fn running_viewer(&self, execution: &ExecutionId) -> Result<Option<Viewer>> {
        let viewer = self.recorded_viewer(execution).await?;
        Ok(viewer.filter(|viewer| runs(&viewer.process)))
    }

    async fn recorded_viewer(&self, execution: &ExecutionId) -> Result<Option<Viewer>> {
        let execution = execution.clone();
        self.store.read(move |db| recorded(db, &execution)).await
    }

    /// The port of the viewer recorded for `demo` while it runs, once it
    /// serves. One whose leader exited has what survives of its group
    /// stopped and its record forgotten, so the open starts another.
    async fn reuse_recorded_viewer(&self, demo: &Running) -> Result<Option<u16>> {
        let Some(viewer) = self.recorded_viewer(&demo.execution_id).await? else {
            return Ok(None);
        };
        if !runs(&viewer.process) {
            self.retire_viewer(demo, &viewer).await?;
            return Ok(None);
        }
        let port = viewer.port;
        if !viewer.serving {
            self.await_recorded_viewer(demo, viewer).await?;
        }
        Ok(Some(port))
    }

    /// Stop the viewer recorded for the demo `execution` runs, before the
    /// execution's records go; fails, so they stay, while its process group
    /// is not proven stopped.
    pub async fn retire_execution_viewer(&self, execution: &ExecutionId) -> Result<()> {
        let Some(viewer) = self.recorded_viewer(execution).await? else {
            return Ok(());
        };
        (stop_recorded(execution, &viewer.process).await).context("its demo's viewer")
    }

    /// The exit code the execution `execution` ends with, its wrapper having
    /// reported `exit_code`: none while its demo's viewer is not proven
    /// stopped, which keeps the execution's records, its end unproven, until
    /// recovery forgets it once the viewer is.
    pub async fn reported_end(
        &self,
        execution: &ExecutionId,
        exit_code: Option<i32>,
    ) -> Option<i32> {
        match self.retire_execution_viewer(execution).await {
            Ok(()) => exit_code,
            Err(error) => {
                log!("execution {execution} kept: {error:#}");
                None
            }
        }
    }

    /// Stop `demo`'s recorded `viewer` and forget it once nothing of its
    /// process group runs; while something does, the record stays, so the
    /// next open tries again.
    async fn retire_viewer(&self, demo: &Running, viewer: &Viewer) -> Result<()> {
        if let Err(error) = stop_recorded(&demo.execution_id, &viewer.process).await {
            let message = format!("the earlier viewer of demo {}: {error:#}", demo.launch.name);
            return Err(Refusal::new(Reason::NotReady, message).into());
        }
        self.forget_viewer(&demo.execution_id).await;
        Ok(())
    }

    /// Where `demo`'s viewer runs: the demo's verified worktree, with the
    /// environment the demo's execution gives its commands, scope included.
    async fn viewer_place(&self, daemon: &dyn Executions, demo: &Running) -> Result<Place> {
        let Joining {
            space,
            configured,
            ports,
        } = daemon.joining(&demo.workspace_id).await?;
        let token = daemon.scope_token(&demo.execution_id).await?;
        let execution = env::execution_environment(&space, &self.paths, &ports, &token);
        let paths = self.paths.clone();
        Ok(Place {
            space,
            paths,
            configured,
            execution,
        })
    }

    pub(crate) async fn record_viewer(
        &self,
        execution: &ExecutionId,
        viewer: &Viewer,
    ) -> Result<()> {
        let execution = execution.clone();
        let (port, serving) = (viewer.port, viewer.serving);
        let process = serde_json::to_string(&viewer.process)?;
        self.store
            .write(move |tx| {
                tx.execute(
                    "INSERT OR REPLACE INTO demo_viewers(execution_id,port,process,serving)
                     VALUES (?1,?2,?3,?4)",
                    params![execution, port, process, serving],
                )?;
                Ok(())
            })
            .await
    }

    /// Forget the viewer of the demo `execution` runs, which did not start.
    async fn forget_viewer(&self, execution: &ExecutionId) {
        let execution = execution.clone();
        let forgotten = (self.store).write(move |tx| {
            tx.execute(
                "DELETE FROM demo_viewers WHERE execution_id=?1",
                [execution],
            )?;
            Ok(())
        });
        if let Err(error) = forgotten.await {
            log!("viewer record not forgotten: {error:#}");
        }
    }

    /// The lock the demo `execution` runs holds while its survivors stop or
    /// its viewer spawns.
    pub fn viewer_spawn_lock(&self, execution: &ExecutionId) -> Arc<AsyncMutex<()>> {
        self.viewers.spawn_lock(execution)
    }

    /// Forget the viewer locks of the demo `execution` ran, which ended.
    pub fn end_viewer(&self, execution: &ExecutionId) {
        self.viewers.end(execution);
    }
}

/// The port reserved for `demo`'s viewer, under the demo's name, as
/// `caller` reserves it; refused while it waits for approval.
async fn reserve_viewer_port(
    daemon: &dyn Ports,
    demo: &Running,
    caller: Option<&Caller>,
) -> Result<u16> {
    let name = &demo.launch.name;
    let request = PortRequest {
        port: None,
        env_var: None,
        reason: Some(format!("The viewer of demo {name}")),
        on_conflict: Some(ConflictPolicy::Auto),
    };
    let reserved = (daemon.reserve_port(&demo.workspace_id, name.clone(), request, caller)).await?;
    match reserved {
        Reserved::Port(port) => Ok(port),
        Reserved::Busy(message) => anyhow::bail!(message),
        Reserved::Approval(request) => {
            let message = format!(
                "the viewer of demo {name} waits for approval {} of its port; open the demo again once it is approved",
                request.number
            );
            Err(Refusal::new(Reason::NotReady, message).into())
        }
        Reserved::Unreserved => {
            anyhow::bail!("no port could be reserved for the viewer of demo {name}")
        }
    }
}

/// The viewer recorded for the demo `execution` runs.
fn recorded(db: &Connection, execution: &ExecutionId) -> Result<Option<Viewer>> {
    let row: Option<(u16, String, bool)> = db
        .query_row(
            "SELECT port,process,serving FROM demo_viewers WHERE execution_id=?1",
            [execution],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    row.map(|(port, process, serving)| {
        let process = serde_json::from_str(&process).context("invalid viewer record")?;
        Ok(Viewer {
            port,
            process,
            serving,
        })
    })
    .transpose()
}

/// Kill the viewer `process` of the demo `execution` runs and its process
/// group: the whole group while the leader verifies, and otherwise each
/// member that carries the execution's marker, verified one by one. A
/// viewer serves only its demo, so nothing in its group has work to save.
/// Fails while a live process remains in the group, such as one whose
/// environment cannot be read, which unknown ownership leaves running.
async fn stop_recorded(execution: &ExecutionId, process: &Identity) -> Result<()> {
    // Its group ended with that boot; its number may name another now.
    if process.of_earlier_boot() {
        return Ok(());
    }
    if runs(process) {
        // SAFETY: the group the live, verified viewer leads.
        unsafe {
            libc::kill(-(process.pid as i32), libc::SIGKILL);
        }
    }
    let mut targets = group_members(execution, process.pid).await;
    targets.push(process.clone());
    targets.retain(runs);
    process::stop_verified(&targets).await?;
    group_ended(process.pid).await
}

/// Wait a few seconds at most until no live process remains in process
/// group `group`.
async fn group_ended(group: u32) -> Result<()> {
    for _ in 0..GROUP_SETTLE_POLLS {
        if !process::group_alive(group).await.unwrap_or(true) {
            return Ok(());
        }
        tokio::time::sleep(START_POLL).await;
    }
    anyhow::bail!(
        "processes of its process group {group} still run; stop them, then open the demo again"
    )
}

/// The live processes of process group `group` that carry the marker of
/// `execution`.
async fn group_members(execution: &ExecutionId, group: u32) -> Vec<Identity> {
    match process::scan(HashSet::from([execution.clone()])).await {
        Ok(scan) => (scan.processes.into_iter())
            .map(|owned| owned.identity)
            .filter(|member| process::group_of(member) == Some(group))
            .collect(),
        Err(error) => {
            log!("viewer group {group} not scanned: {error:#}");
            Vec::new()
        }
    }
}

/// A connection to `port` on loopback: IPv4's, else IPv6's.
pub async fn connect(port: u16) -> Result<TcpStream> {
    match TcpStream::connect(("127.0.0.1", port)).await {
        Ok(connection) => Ok(connection),
        Err(v4) => TcpStream::connect(("::1", port))
            .await
            .map_err(|_| v4.into()),
    }
}

/// Refuse a viewer `port` another process already listens on, which a
/// new viewer's readiness would take for its own.
async fn refuse_occupied(port: u16, demo: &str) -> Result<()> {
    if connect(port).await.is_err() {
        return Ok(());
    }
    let message = format!(
        "port {port}, reserved for the viewer of demo {demo}, is in use by another process; stop it, or `shoal release port {demo}` so the next open reserves another"
    );
    Err(Refusal::new(Reason::NotReady, message).into())
}

/// Whether `process` runs: alive under its recorded identity and not
/// exited awaiting its parent.
pub(crate) fn runs(process: &Identity) -> bool {
    process::alive(process).unwrap_or(false) && !process::unreaped(process).unwrap_or(true)
}

/// Where a viewer runs: the demo's space, its configured environment, and
/// what its execution exports to its commands, scope included.
struct Place {
    space: ExecutionSpace,
    paths: Paths,
    configured: BTreeMap<String, OsString>,
    execution: BTreeMap<String, OsString>,
}

/// A viewer this daemon started that does not serve yet.
struct Starting {
    child: Child,
    process: Identity,
    program: String,
    /// The file it writes its errors to.
    log: PathBuf,
}

impl Starting {
    /// Start `demo`'s viewer on `port` at `place`, leading a process group
    /// of its own and marked as the demo's execution, its errors written to
    /// a file that needs no reader, so it may write them after the daemon
    /// goes.
    fn spawn(demo: &Running, port: u16, place: &Place) -> Result<Self> {
        let command = (demo.launch)
            .filled_viewer_command(port)
            .context("the demo has no viewer")?;
        let (program, args) = command.split_first().context("the viewer has no program")?;
        let log = place.paths.viewer_log(&place.space.id, &demo.launch.name);
        let errors = create_log(&log)?;
        let mut viewer = Command::new(program);
        env::apply_space_identity(&mut viewer, &place.space, &place.paths);
        env::apply_configured(&mut viewer, &place.configured);
        let child = viewer
            .args(args)
            .current_dir(&place.space.path)
            .envs(&place.execution)
            .envs(&demo.launch.environment)
            .env(env::EXECUTION_ID, &demo.execution_id)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(errors)
            .process_group(0)
            .spawn()
            .map_err(|error| not_started(program, error))?;
        let pid = child.id().context("the viewer exited at once")?;
        let process = match process::capture(pid) {
            Ok(Some(process)) => process,
            // Exited already, as one that exits while it starts.
            _ => {
                abandon(child);
                let message = format!("viewer {program} exited{}", printed(&log));
                return Err(Refusal::new(Reason::NotReady, message).into());
            }
        };
        Ok(Self {
            child,
            process,
            program: program.clone(),
            log,
        })
    }

    /// Wait until `port` takes a connection, then leave the viewer running,
    /// its exit reaped here; a daemon that goes leaves it running. A viewer
    /// that exits first or takes no connection within [`START_LIMIT`] is
    /// stopped.
    async fn serve(mut self, port: u16) -> Result<()> {
        match self.await_port(port).await {
            Ok(()) => {
                tokio::spawn(async move { self.child.wait().await });
                Ok(())
            }
            Err(error) => {
                self.stop().await;
                Err(error)
            }
        }
    }

    async fn await_port(&mut self, port: u16) -> Result<()> {
        let (child, process, log) = (&mut self.child, &self.process, &self.log);
        let exited = || {
            if !process::unreaped(process).unwrap_or(false) {
                return Ok(None);
            }
            // Until it is reaped, its ID still names the group it leads, so
            // what it started there goes with it.
            // SAFETY: the group of the unreaped child this daemon started.
            unsafe {
                libc::kill(-(process.pid as i32), libc::SIGKILL);
            }
            let status = child.try_wait()?;
            let status = status.map_or_else(
                || "exited".to_owned(),
                |status| format!("exited with {status}"),
            );
            Ok(Some(format!("{status}{}", printed(log))))
        };
        await_port(port, &self.program, exited).await
    }

    async fn stop(mut self) {
        stop_unreaped(&mut self.child).await;
    }
}

/// Wait until `port` takes a connection, failing once the viewer `program`
/// has `exited`, which says how, or [`START_LIMIT`] passes.
async fn await_port(
    port: u16,
    program: &str,
    mut exited: impl FnMut() -> Result<Option<String>>,
) -> Result<()> {
    let deadline = Instant::now() + START_LIMIT;
    loop {
        if connect(port).await.is_ok() {
            return Ok(());
        }
        if let Some(how) = exited()? {
            let message = format!("viewer {program} {how}");
            return Err(Refusal::new(Reason::NotReady, message).into());
        }
        if Instant::now() >= deadline {
            let message = format!(
                "viewer {program} took no connection on port {port} within {}s",
                START_LIMIT.as_secs()
            );
            return Err(Refusal::new(Reason::NotReady, message).into());
        }
        tokio::time::sleep(START_POLL).await;
    }
}

/// Kill the process group of `child` and reap it meanwhile.
fn abandon(mut child: Child) {
    tokio::spawn(async move { stop_unreaped(&mut child).await });
}

/// Kill the process group `child` leads, then reap it. Until this daemon
/// reaps it, exited or not, its PID still names that group.
async fn stop_unreaped(child: &mut Child) {
    if let Some(pid) = child.id() {
        // SAFETY: the group of the child this daemon started and has not
        // reaped.
        unsafe {
            libc::kill(-(pid as i32), libc::SIGKILL);
        }
    }
    let _ = child.wait().await;
}

/// The file at `log`, emptied, for a viewer to write its errors to.
fn create_log(log: &Path) -> Result<std::fs::File> {
    if let Some(directory) = log.parent() {
        std::fs::create_dir_all(directory)?;
    }
    std::fs::File::create(log).with_context(|| format!("create {}", log.display()))
}

/// The last [`ERROR_LINES`] lines of the viewer's errors at `log`, after a
/// colon, from at most its last [`ERROR_BYTES`].
fn printed(log: &Path) -> String {
    let errors = tail(log).unwrap_or_default();
    let lines: Vec<_> = errors.lines().collect();
    let last = &lines[lines.len().saturating_sub(ERROR_LINES)..];
    if last.is_empty() {
        return String::new();
    }
    format!(": {}", last.join("\n"))
}

/// The last [`ERROR_BYTES`] of `log`.
fn tail(log: &Path) -> Result<String> {
    use std::io::{Read as _, Seek as _, SeekFrom};
    let mut file = std::fs::File::open(log)?;
    let length = file.metadata()?.len();
    file.seek(SeekFrom::Start(length.saturating_sub(ERROR_BYTES)))?;
    let mut bytes = Vec::new();
    file.take(ERROR_BYTES).read_to_end(&mut bytes)?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// Why the viewer `program` did not start, naming how to install a missing
/// serve-sim.
fn not_started(program: &str, error: std::io::Error) -> anyhow::Error {
    let message = match error.kind() {
        std::io::ErrorKind::NotFound if program == SERVE_SIM.0 => format!(
            "{program} is not installed; install it with `{}`",
            SERVE_SIM.1
        ),
        std::io::ErrorKind::NotFound => format!("viewer {program} is not installed"),
        _ => format!("viewer {program} did not start: {error}"),
    };
    Refusal::new(Reason::NotReady, message).into()
}

#[cfg(test)]
mod tests;
