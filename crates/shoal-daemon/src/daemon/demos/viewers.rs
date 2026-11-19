//! Viewers of simulator demos: a tool, serve-sim unless the demo names
//! another, that streams the demo's device and takes its input on a port,
//! which a demo stream then carries as it carries a web demo's. The first
//! open starts it, marked as the demo's execution, so stopping, removal and
//! recovery stop it with the demo's other processes; it outlives the demo's
//! runs and ends with the demo.
use std::{
    collections::{HashMap, VecDeque},
    process::Stdio,
    sync::{Arc, Mutex},
    time::Duration,
};

use anyhow::{Context as _, Result, bail};
use shoal_ids::execution_ids::ExecutionId;
use shoal_leases::{allocation::Allocation, ports::Acquisition};
use shoal_protocol::{demos::Running, space_names::SpaceSelector};
use tokio::{
    io::{AsyncBufReadExt as _, BufReader},
    process::{Child, ChildStderr, Command},
    sync::Mutex as AsyncMutex,
    time::Instant,
};

use super::streaming::connect;
use crate::{
    config::repo::ConflictPolicy,
    daemon::manager::Manager,
    env,
    model::PortRequest,
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

/// The viewers of running demos, by the execution that runs each demo.
#[derive(Default)]
pub(crate) struct Viewers {
    demos: Mutex<HashMap<ExecutionId, Slot>>,
}

/// A demo's viewer, locked while it starts.
type Slot = Arc<AsyncMutex<Option<Viewer>>>;

/// A viewer serving on `port`: the process this daemon started, or none
/// for one an earlier daemon started.
struct Viewer {
    port: u16,
    process: Option<Process>,
}

/// A viewer process, the leader of its own process group, whose group is
/// killed when it is dropped.
struct Process {
    child: Child,
    group: u32,
    errors: Arc<Mutex<VecDeque<String>>>,
}

impl Viewers {
    fn slot(&self, execution: &ExecutionId) -> Slot {
        let mut demos = self.demos.lock().unwrap_or_else(|error| error.into_inner());
        demos.entry(execution.clone()).or_default().clone()
    }

    /// The port the viewer of the demo `execution` runs serves on, while
    /// it runs; none while it starts.
    pub(crate) fn port(&self, execution: &ExecutionId) -> Option<u16> {
        let slot = self.existing(execution)?;
        let mut viewer = slot.try_lock().ok()?;
        let viewer = viewer.as_mut()?;
        viewer.runs().then_some(viewer.port)
    }

    /// The process group of the viewer this daemon started for the demo
    /// `execution` runs, which outlives the demo's runs.
    pub(crate) fn group(&self, execution: &ExecutionId) -> Option<u32> {
        let slot = self.existing(execution)?;
        let viewer = slot.try_lock().ok()?;
        Some(viewer.as_ref()?.process.as_ref()?.group)
    }

    /// Forget the viewer of the demo `execution` ran, stopping it.
    pub(crate) fn end(&self, execution: &ExecutionId) {
        let mut demos = self.demos.lock().unwrap_or_else(|error| error.into_inner());
        demos.remove(execution);
    }

    fn existing(&self, execution: &ExecutionId) -> Option<Slot> {
        let demos = self.demos.lock().unwrap_or_else(|error| error.into_inner());
        demos.get(execution).cloned()
    }
}

impl Viewer {
    /// Its port while the process this daemon started runs.
    fn serving(&mut self) -> Option<u16> {
        let process = self.process.as_mut()?;
        matches!(process.child.try_wait(), Ok(None)).then_some(self.port)
    }

    /// Whether it runs, as far as this daemon knows: one an earlier daemon
    /// started is taken to until a connection to it fails.
    fn runs(&mut self) -> bool {
        self.process.is_none() || self.serving().is_some()
    }
}

impl Manager {
    /// The port the viewer of the simulator demo `demo` serves on, started
    /// on the demo's first open; refused for a demo whose viewer is off.
    pub(crate) async fn open_viewer(&self, demo: &Running) -> Result<u16> {
        let name = &demo.launch.name;
        if demo.launch.viewer.is_none() {
            let message =
                format!("demo {name} has its viewer turned off; open it on its own machine");
            return Err(Refusal::new(Reason::NotReady, message).into());
        }
        let slot = self.viewers.slot(&demo.execution_id);
        let mut viewer = slot.lock().await;
        if let Some(port) = viewer.as_mut().and_then(Viewer::serving) {
            return Ok(port);
        }
        let port = self.viewer_port(demo).await?;
        // A viewer an earlier daemon started still serves there.
        let process = match connect(port).await {
            Ok(_) => None,
            Err(_) => Some(self.start_viewer(demo, port).await?),
        };
        *viewer = Some(Viewer { port, process });
        Ok(port)
    }

    /// The port reserved for `demo`'s viewer, under the demo's name.
    async fn viewer_port(&self, demo: &Running) -> Result<u16> {
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

    /// Start `demo`'s viewer on `port` in its workspace and wait until the
    /// port takes a connection.
    async fn start_viewer(&self, demo: &Running, port: u16) -> Result<Process> {
        let space = self.space(&SpaceSelector::id(&demo.workspace_id)).await?;
        let directory = self.space_directory(&space).await?;
        let command = (demo.launch)
            .viewer_command(port)
            .context("the demo has no viewer")?;
        let mut process = Process::spawn(&command, &directory, demo)?;
        process.await_port(&command[0], port).await?;
        Ok(process)
    }
}

impl Process {
    /// Start `command` in `directory` as `demo`'s viewer, in a process group
    /// of its own, marked as the demo's execution, keeping its errors.
    fn spawn(command: &[String], directory: &std::path::Path, demo: &Running) -> Result<Self> {
        let (program, args) = command.split_first().context("the viewer has no program")?;
        let child = Command::new(program)
            .args(args)
            .current_dir(directory)
            .envs(&demo.launch.environment)
            .env_remove(env::SCOPE_TOKEN)
            .env(env::EXECUTION_ID, &demo.execution_id)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .process_group(0)
            .kill_on_drop(true)
            .spawn()
            .map_err(|error| not_started(program, error))?;
        let mut process = Self {
            group: child.id().context("the viewer has no PID")?,
            errors: Default::default(),
            child,
        };
        if let Some(stderr) = process.child.stderr.take() {
            tokio::spawn(keep_errors(stderr, process.errors.clone()));
        }
        Ok(process)
    }

    /// Wait until `port` takes a connection, failing once the viewer
    /// `program` exits or [`START_LIMIT`] passes.
    async fn await_port(&mut self, program: &str, port: u16) -> Result<()> {
        let deadline = Instant::now() + START_LIMIT;
        loop {
            if connect(port).await.is_ok() {
                return Ok(());
            }
            if let Some(status) = self.child.try_wait()? {
                let message = format!("viewer {program} exited with {status}{}", self.errors());
                return Err(Refusal::new(Reason::NotReady, message).into());
            }
            if Instant::now() >= deadline {
                let message = format!(
                    "viewer {program} took no connection on port {port} within {}s{}",
                    START_LIMIT.as_secs(),
                    self.errors()
                );
                return Err(Refusal::new(Reason::NotReady, message).into());
            }
            tokio::time::sleep(START_POLL).await;
        }
    }

    /// The last lines of errors it printed, after a colon.
    fn errors(&self) -> String {
        let errors = self
            .errors
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if errors.is_empty() {
            return String::new();
        }
        let lines: Vec<_> = errors.iter().map(String::as_str).collect();
        format!(": {}", lines.join("\n"))
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        // SAFETY: the group of the viewer this daemon started, which leads it.
        unsafe {
            libc::kill(-(self.group as i32), libc::SIGKILL);
        }
    }
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
