//! Viewers of simulator demos: a tool, serve-sim unless the demo names
//! another, that streams the demo's device and takes its input on a port,
//! which a demo stream then carries as it carries a web demo's. The first
//! open starts it in the demo's verified worktree, marked as the demo's
//! execution, so stopping, removal and recovery stop it with the demo's
//! other processes. The daemon records its process with the demo, by which
//! it spares the viewer between the demo's runs, across its own restarts too.
use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    ffi::OsString,
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
    io::{AsyncBufReadExt as _, BufReader},
    process::{Child, ChildStderr, Command},
    sync::Mutex as AsyncMutex,
    time::Instant,
};

use super::streaming::connect;
use crate::daemon::log;
use crate::{
    config::repo::ConflictPolicy,
    daemon::manager::Manager,
    env,
    model::{ExecutionSpace, PortRequest},
    paths::Paths,
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

/// The starts of demos' viewers, by the execution that runs each demo, so
/// two opens of one demo start one viewer.
#[derive(Default)]
pub(crate) struct Viewers {
    starts: Mutex<HashMap<ExecutionId, Arc<AsyncMutex<()>>>>,
}

/// A viewer the daemon started: the port it serves on, its process, which
/// leads a process group of its own, and whether it serves yet.
pub(crate) struct Viewer {
    pub(crate) port: u16,
    pub(crate) process: Identity,
    pub(crate) serving: bool,
}

impl Viewers {
    fn start_lock(&self, execution: &ExecutionId) -> Arc<AsyncMutex<()>> {
        let mut starts = self
            .starts
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        starts.entry(execution.clone()).or_default().clone()
    }

    /// Forget the demo `execution` ran, whose viewer record went with it.
    pub(crate) fn end(&self, execution: &ExecutionId) {
        let mut starts = self
            .starts
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        starts.remove(execution);
    }
}

impl Manager {
    /// The port the viewer of the simulator demo `demo` serves on, started
    /// on the demo's first open; refused for a demo whose viewer is off.
    pub(crate) async fn open_viewer(&self, demo: &Running) -> Result<u16> {
        let name = &demo.launch.name;
        if demo.launch.viewer_command.is_none() {
            let message =
                format!("demo {name} has its viewer turned off; open it on its own machine");
            return Err(Refusal::new(Reason::NotReady, message).into());
        }
        let start = self.viewers.start_lock(&demo.execution_id);
        let _starting = start.lock().await;
        if let Some(viewer) = self.running_viewer(&demo.execution_id).await? {
            return Ok(viewer.port);
        }
        let place = self.viewer_place(demo).await?;
        let port = self.reserve_viewer_port(demo).await?;
        let starting = Starting::spawn(demo, port, &place)?;
        // Recorded before it serves, so a run ending meanwhile spares it.
        let mut viewer = Viewer {
            port,
            process: starting.process.clone(),
            serving: false,
        };
        if let Err(error) = self.record_viewer(&demo.execution_id, &viewer).await {
            starting.stop().await;
            return Err(error);
        }
        if let Err(error) = starting.serve(port).await {
            self.forget_viewer(&demo.execution_id).await;
            return Err(error);
        }
        viewer.serving = true;
        self.record_viewer(&demo.execution_id, &viewer).await?;
        Ok(port)
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
        let space = self.execution_home(&demo.workspace_id).await?.plan_space();
        let workspace = SpaceSelector::id(&demo.workspace_id);
        let ports = self.leases.list_ports(self, Some(&workspace)).await?;
        let execution = demo.execution_id.clone();
        let token = self
            .store
            .read(move |db| scope_token(db, &execution))
            .await?;
        let environment = env::execution_environment(&space, &self.paths, &ports, &token);
        let paths = self.paths.clone();
        Ok(Place {
            space,
            paths,
            environment,
        })
    }

    /// The port reserved for `demo`'s viewer, under the demo's name.
    async fn reserve_viewer_port(&self, demo: &Running) -> Result<u16> {
        let name = &demo.launch.name;
        let request = PortRequest {
            port: None,
            env_var: None,
            reason: Some(format!("The viewer of demo {name}")),
            on_conflict: Some(ConflictPolicy::Auto),
        };
        let workspace = SpaceSelector::id(&demo.workspace_id);
        let acquired = (self.leases)
            .acquire_port(self, &workspace, name.clone(), request, None)
            .await?;
        match acquired {
            Acquisition::Allocation(Allocation::Granted(reservation)) => Ok(reservation.port),
            Acquisition::Allocation(Allocation::Busy(message)) => bail!(message),
            Acquisition::Allocation(Allocation::Approval(_)) | Acquisition::Suggested(_) => {
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

/// Whether `process` runs: alive under its recorded identity and not
/// exited awaiting its parent.
fn runs(process: &Identity) -> bool {
    process::alive(process).unwrap_or(false) && !process::unreaped(process).unwrap_or(true)
}

/// Where a viewer runs: the demo's space, and the environment its
/// execution gives its commands.
struct Place {
    space: ExecutionSpace,
    paths: Paths,
    environment: BTreeMap<String, OsString>,
}

/// A viewer this daemon started that does not serve yet.
struct Starting {
    child: Child,
    process: Identity,
    program: String,
    errors: Arc<Mutex<VecDeque<String>>>,
}

impl Starting {
    /// Start `demo`'s viewer on `port` at `place`, leading a process group
    /// of its own and marked as the demo's execution, keeping the errors it
    /// prints.
    fn spawn(demo: &Running, port: u16, place: &Place) -> Result<Self> {
        let command = (demo.launch)
            .filled_viewer_command(port)
            .context("the demo has no viewer")?;
        let (program, args) = command.split_first().context("the viewer has no program")?;
        let mut viewer = Command::new(program);
        env::apply_space_identity(&mut viewer, &place.space, &place.paths);
        let mut child = viewer
            .args(args)
            .current_dir(&place.space.path)
            .envs(&place.environment)
            .envs(&demo.launch.environment)
            .env(env::EXECUTION_ID, &demo.execution_id)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .process_group(0)
            .spawn()
            .map_err(|error| not_started(program, error))?;
        let errors = Arc::new(Mutex::new(VecDeque::new()));
        if let Some(stderr) = child.stderr.take() {
            tokio::spawn(keep_errors(stderr, errors.clone()));
        }
        let pid = child.id().context("the viewer exited at once")?;
        let process = process::capture(pid)?.context("the viewer exited at once")?;
        Ok(Self {
            child,
            process,
            program: program.clone(),
            errors,
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
        let (program, deadline) = (&self.program, Instant::now() + START_LIMIT);
        loop {
            if connect(port).await.is_ok() {
                return Ok(());
            }
            if let Some(status) = self.child.try_wait()? {
                let printed = printed(&self.errors);
                let message = format!("viewer {program} exited with {status}{printed}");
                return Err(Refusal::new(Reason::NotReady, message).into());
            }
            if Instant::now() >= deadline {
                let message = format!(
                    "viewer {program} took no connection on port {port} within {}s{}",
                    START_LIMIT.as_secs(),
                    printed(&self.errors)
                );
                return Err(Refusal::new(Reason::NotReady, message).into());
            }
            tokio::time::sleep(START_POLL).await;
        }
    }

    async fn stop(mut self) {
        stop(&mut self.child).await;
    }
}

/// Stop the process group `child` leads. Only while it is unreaped does
/// its PID still name that group.
async fn stop(child: &mut Child) {
    if let (Ok(None), Some(pid)) = (child.try_wait(), child.id()) {
        // SAFETY: the group of the unreaped child this daemon started.
        unsafe {
            libc::kill(-(pid as i32), libc::SIGKILL);
        }
    }
    let _ = child.wait().await;
}

/// The last lines of `errors`, after a colon.
fn printed(errors: &Mutex<VecDeque<String>>) -> String {
    let errors = errors.lock().unwrap_or_else(|error| error.into_inner());
    if errors.is_empty() {
        return String::new();
    }
    let lines: Vec<_> = errors.iter().map(String::as_str).collect();
    format!(": {}", lines.join("\n"))
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

/// Keep the last [`ERROR_LINES`] lines `stderr` carries in `errors`.
async fn keep_errors(stderr: ChildStderr, errors: Arc<Mutex<VecDeque<String>>>) {
    let mut lines = BufReader::new(stderr).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let mut errors = errors.lock().unwrap_or_else(|error| error.into_inner());
        if errors.len() == ERROR_LINES {
            errors.pop_front();
        }
        errors.push_back(line);
    }
}

#[cfg(test)]
mod tests;
