//! Viewers of simulator demos: a tool, serve-sim unless the demo names
//! another, that streams the demo's device and takes its input on a port,
//! which a demo stream then carries as it carries a web demo's. The first
//! open starts it in the demo's verified worktree, marked as the demo's
//! execution, so stopping, removal and recovery stop it with the demo's
//! other processes. The daemon records its process with the demo, by which
//! it spares the viewer between the demo's runs, across its own restarts too.
use std::{
    collections::{BTreeMap, HashMap},
    ffi::OsString,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{Arc, Mutex},
    time::Duration,
};

use anyhow::{Context as _, Result, bail};
use rusqlite::{Connection, OptionalExtension, params};
use shoal_ids::execution_ids::ExecutionId;
use shoal_leases::{allocation::Allocation, ports::Acquisition};
use shoal_protocol::{demos::Running, scope_tokens::ScopeToken, space_names::SpaceSelector};
use tokio::{
    process::{Child, Command},
    sync::Mutex as AsyncMutex,
    time::Instant,
};

use super::streaming::connect;
use crate::daemon::log;
use crate::{
    config::repo::ConflictPolicy,
    daemon::{manager::Manager, scope::Caller},
    env,
    model::{ExecutionSpace, PortRequest},
    paths::{Paths, StateFiles as _},
    process::identity::{self as process, Identity},
    refusal::{Reason, Refusal},
};

/// How often a starting viewer is checked for its port.
const START_POLL: Duration = Duration::from_millis(100);
/// A hang guard for a viewer that never takes a connection.
const START_LIMIT: Duration = Duration::from_secs(60);
/// How many of a viewer's last lines of errors its failure shows.
const ERROR_LINES: usize = 10;
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
pub(crate) struct Viewer {
    pub(crate) port: u16,
    pub(crate) process: Identity,
    pub(crate) serving: bool,
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

impl Manager {
    /// The port the viewer of the simulator demo `demo` serves on, started
    /// on the demo's first open; refused for a demo whose viewer is off.
    pub(crate) async fn open_viewer(&self, demo: &Running, caller: Option<&Caller>) -> Result<u16> {
        let name = &demo.launch.name;
        if demo.launch.viewer_command.is_none() {
            let message =
                format!("demo {name} has its viewer turned off; open it on its own machine");
            return Err(Refusal::new(Reason::NotReady, message).into());
        }
        let start = lock(&self.viewers.starts, &demo.execution_id);
        let _starting = start.lock().await;
        if let Some(viewer) = self.running_viewer(&demo.execution_id).await? {
            let port = viewer.port;
            if !viewer.serving {
                self.await_recorded_viewer(demo, viewer).await?;
            }
            return Ok(port);
        }
        // Reserved first, so the environment exports it.
        let port = self.reserve_viewer_port(demo, caller).await?;
        let place = self.viewer_place(demo).await?;
        let starting = self.spawn_recorded_viewer(demo, port, &place).await?;
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
        demo: &Running,
        port: u16,
        place: &Place,
    ) -> Result<Starting> {
        let spawning = self.viewers.spawn_lock(&demo.execution_id);
        let _spawning = spawning.lock().await;
        if self.executions.completing(&demo.execution_id).is_some() {
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
            stop_recorded(&viewer.process).await;
            self.forget_viewer(&demo.execution_id).await;
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
    pub(crate) async fn running_viewer(&self, execution: &ExecutionId) -> Result<Option<Viewer>> {
        let execution = execution.clone();
        let viewer = self.store.read(move |db| recorded(db, &execution)).await?;
        Ok(viewer.filter(|viewer| runs(&viewer.process)))
    }

    /// Where `demo`'s viewer runs: the demo's verified worktree, with the
    /// environment the demo's execution gives its commands, scope included.
    async fn viewer_place(&self, demo: &Running) -> Result<Place> {
        let home = self.execution_home(&demo.workspace_id).await?;
        let (home, configured, ports) = self.joining_environment(home).await?;
        let space = home.plan_space();
        let execution = demo.execution_id.clone();
        let token = self
            .store
            .read(move |db| scope_token(db, &execution))
            .await?;
        let execution = env::execution_environment(&space, &self.paths, &ports, &token);
        let paths = self.paths.clone();
        Ok(Place {
            space,
            paths,
            configured,
            execution,
        })
    }

    /// The port reserved for `demo`'s viewer, under the demo's name, as
    /// `caller` reserves it; refused while it waits for approval.
    async fn reserve_viewer_port(&self, demo: &Running, caller: Option<&Caller>) -> Result<u16> {
        let name = &demo.launch.name;
        let request = PortRequest {
            port: None,
            env_var: None,
            reason: Some(format!("The viewer of demo {name}")),
            on_conflict: Some(ConflictPolicy::Auto),
        };
        let workspace = SpaceSelector::id(&demo.workspace_id);
        let acquired = (self.leases)
            .acquire_port(self, &workspace, name.clone(), request, caller)
            .await?;
        match acquired {
            Acquisition::Allocation(Allocation::Granted(reservation)) => Ok(reservation.port),
            Acquisition::Allocation(Allocation::Busy(message)) => bail!(message),
            Acquisition::Allocation(Allocation::Approval(request)) => {
                let message = format!(
                    "the viewer of demo {name} waits for approval {} of its port; open the demo again once it is approved",
                    request.number
                );
                Err(Refusal::new(Reason::NotReady, message).into())
            }
            Acquisition::Suggested(_) => {
                bail!("no port could be reserved for the viewer of demo {name}")
            }
        }
    }

    async fn record_viewer(&self, execution: &ExecutionId, viewer: &Viewer) -> Result<()> {
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
}

/// The viewer recorded for the demo `execution` runs.
pub(crate) fn recorded(db: &Connection, execution: &ExecutionId) -> Result<Option<Viewer>> {
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

/// The scope token of the execution `execution`, which its commands act
/// under.
fn scope_token(db: &Connection, execution: &ExecutionId) -> Result<ScopeToken> {
    let token: Option<ScopeToken> = db
        .query_row(
            "SELECT scope_token FROM executions WHERE id=?1",
            [execution],
            |row| row.get(0),
        )
        .optional()?
        .flatten();
    token.context("the demo's execution has no scope token")
}

/// Kill the viewer `process` and its process group, which it leads while
/// its identity verifies: a viewer serves only its demo, so nothing in its
/// group has work to save.
pub(crate) async fn stop_recorded(process: &Identity) {
    if runs(process) {
        // SAFETY: the group the live, verified viewer leads.
        unsafe {
            libc::kill(-(process.pid as i32), libc::SIGKILL);
        }
    }
    if let Err(error) = process::stop_verified(std::slice::from_ref(process)).await {
        log!("viewer {} not stopped: {error:#}", process.pid);
    }
}

/// Whether `process` runs: alive under its recorded identity and not
/// exited awaiting its parent.
fn runs(process: &Identity) -> bool {
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

/// Kill the process group of `child`, which this daemon never waited for,
/// so its PID still names that group, and reap it meanwhile.
fn abandon(mut child: Child) {
    if let Some(pid) = child.id() {
        // SAFETY: the group of the unreaped child this daemon started.
        unsafe {
            libc::kill(-(pid as i32), libc::SIGKILL);
        }
    }
    tokio::spawn(async move { child.wait().await });
}

/// Stop the process group `child` leads. Only while it is unreaped does
/// its PID still name that group.
async fn stop_unreaped(child: &mut Child) {
    if let (Ok(None), Some(pid)) = (child.try_wait(), child.id()) {
        // SAFETY: the group of the unreaped child this daemon started.
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
/// colon.
fn printed(log: &Path) -> String {
    let errors = std::fs::read_to_string(log).unwrap_or_default();
    let lines: Vec<_> = errors.lines().collect();
    let last = &lines[lines.len().saturating_sub(ERROR_LINES)..];
    if last.is_empty() {
        return String::new();
    }
    format!(": {}", last.join("\n"))
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
