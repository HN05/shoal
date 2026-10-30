//! Hold the operating system's sleep inhibitor while agents have work, when
//! `keep_awake` opts in, so an idle timeout or a closed laptop lid does not
//! suspend it. The inhibitor lasts only as long as a pipe the daemon holds
//! open, so it ends with the daemon however that stops.
use std::{process::Stdio, sync::Arc, time::Duration};

use anyhow::{Context, Result, anyhow};
use rusqlite::Connection;
use tokio::{
    io::AsyncReadExt,
    process::{Child, ChildStderr, ChildStdin, Command},
};

use super::{
    links::wait::watches_open_pr,
    log,
    notifications::NotificationKind,
    workspace::{Manager, TrackedAgents, agent_state, delegated},
};
use crate::{config::Config, state::AgentState};

/// Rechecks without a store change, such as after a failed lookup.
const SWEEP_INTERVAL: Duration = Duration::from_secs(30);
/// Lets a burst of state changes settle into one pass.
const SETTLE: Duration = Duration::from_millis(500);
/// How long an ended inhibitor's remaining stderr may take to arrive.
const REMAINDER_GUARD: Duration = Duration::from_secs(1);

/// The platform's inhibitor, which holds sleep off while the command it is
/// given, `cat` reading the daemon's pipe, runs.
#[cfg(target_os = "macos")]
const INHIBITOR: &[&str] = &["caffeinate", "-i", "-s"];
#[cfg(not(target_os = "macos"))]
const INHIBITOR: &[&str] = &[
    "systemd-inhibit",
    "--what=sleep:idle:handle-lid-switch",
    "--who=Shoal",
    "--why=Agents have work",
    "--mode=block",
];

enum Hold {
    Released,
    Held(Box<Inhibitor>),
    /// The inhibitor could not start or ended on its own under this config.
    /// It is retried once agents run out of work or another config is in
    /// effect, since a reload may fix what failed.
    Failed(Arc<Config>),
}

impl Hold {
    /// Wait for a held inhibitor to end on its own.
    async fn ended(&mut self) -> anyhow::Error {
        match self {
            Self::Held(inhibitor) => inhibitor.ended().await,
            Self::Released | Self::Failed(_) => std::future::pending().await,
        }
    }
}

pub(super) async fn run(manager: Arc<Manager>) {
    let mut changed = manager.store.watch_changes();
    let mut reloaded = manager.config_state.watch();
    let mut hold = Hold::Released;
    loop {
        let config = manager.config();
        if let Some(wanted) = wanted(&manager, &config).await {
            hold = update(&manager, hold, wanted, &config, INHIBITOR).await;
        }
        let ended = tokio::select! {
            _ = changed.changed() => None,
            _ = reloaded.changed() => None,
            error = hold.ended() => Some(error),
            () = tokio::time::sleep(SWEEP_INTERVAL) => None,
        };
        if let Some(error) = ended {
            hold = failed(&manager, error, config).await;
        }
        tokio::time::sleep(SETTLE).await;
        changed.borrow_and_update();
        reloaded.borrow_and_update();
    }
}

/// Whether to hold the inhibitor under `config`, or `None` when the lookup
/// failed and the current hold should stay.
async fn wanted(manager: &Manager, config: &Config) -> Option<bool> {
    if !config.machine.keep_awake {
        return Some(false);
    }
    match manager.agents_have_work().await {
        Ok(work) => Some(work),
        Err(error) => {
            log!("keep awake: {error:#}");
            None
        }
    }
}

impl Manager {
    async fn agents_have_work(&self) -> Result<bool> {
        let agents = self.tracked_agents().await;
        self.store
            .run(move |db| agents_have_work(db, &agents))
            .await
    }
}

/// Whether an agent is working on a turn, or has finished one while an open
/// PR whose activity Shoal delivers to it as its next turn is watched.
fn agents_have_work(db: &Connection, agents: &TrackedAgents) -> Result<bool> {
    for (id, state) in agent_state::all(db)? {
        let work = match state {
            AgentState::Working => true,
            AgentState::Idle => receives_open_pr_activity(db, &id, agents)?,
            AgentState::Waiting => false,
        };
        if work {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Whether the workspace, or a child workspace whose activity its agent
/// receives, watches an open PR.
fn receives_open_pr_activity(db: &Connection, id: &str, agents: &TrackedAgents) -> Result<bool> {
    let children = delegated(db, id, agents)?.into_iter().map(|child| child.id);
    for workspace in std::iter::once(id.to_owned()).chain(children) {
        if watches_open_pr(db, &workspace)? {
            return Ok(true);
        }
    }
    Ok(false)
}

async fn update(
    manager: &Manager,
    hold: Hold,
    wanted: bool,
    config: &Arc<Config>,
    inhibitor: &[&str],
) -> Hold {
    match (hold, wanted) {
        (Hold::Failed(failed), true) if Arc::ptr_eq(&failed, config) => Hold::Failed(failed),
        (Hold::Released | Hold::Failed(_), true) => match Inhibitor::start(inhibitor) {
            Ok(inhibitor) => Hold::Held(Box::new(inhibitor)),
            Err(error) => failed(manager, error, config.clone()).await,
        },
        (hold @ Hold::Held(_), true) => hold,
        (_, false) => Hold::Released,
    }
}

async fn failed(manager: &Manager, error: anyhow::Error, config: Arc<Config>) -> Hold {
    let message = format!("Shoal could not keep this machine awake: {error:#}");
    log!("keep awake: {error:#}");
    manager
        .notify(None, NotificationKind::KeepAwakeFailed, message)
        .await;
    Hold::Failed(config)
}

/// A running inhibitor, which ends once dropped: its input closes and its
/// process is killed.
struct Inhibitor {
    program: String,
    child: Child,
    /// Drained while the inhibitor runs, so it never blocks on a full pipe.
    stderr: Option<ChildStderr>,
    /// The start of what it wrote to stderr, kept for the failure message.
    said: Vec<u8>,
    /// Closing it lets `cat`, and so the inhibitor, finish.
    _input: Option<ChildStdin>,
}

impl Inhibitor {
    fn start(argv: &[&str]) -> Result<Self> {
        let (program, args) = argv.split_first().context("empty inhibitor command")?;
        let mut child = Command::new(program)
            .args(args)
            .arg("cat")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .with_context(|| format!("start {program}"))?;
        Ok(Self {
            program: (*program).into(),
            stderr: child.stderr.take(),
            _input: child.stdin.take(),
            child,
            said: Vec::new(),
        })
    }

    /// Wait for the inhibitor to end on its own, with what it said. Dropping
    /// the wait keeps what was read for the next one.
    async fn ended(&mut self) -> anyhow::Error {
        let status = loop {
            let mut chunk = [0; 1024];
            tokio::select! {
                status = self.child.wait() => break status,
                read = read_some(&mut self.stderr, &mut chunk) => self.keep(read, &chunk),
            }
        };
        // `cat` shares the pipe and can outlive the inhibitor, so the rest is
        // read only while it comes promptly.
        let _ = tokio::time::timeout(REMAINDER_GUARD, self.read_remainder()).await;
        let status = match status {
            Ok(status) => status,
            Err(error) => return anyhow!(error).context(format!("wait for {}", self.program)),
        };
        match crate::subprocess::diagnostic(&self.said).trim() {
            "" => anyhow!("{} ended ({status})", self.program),
            said => anyhow!("{} ended ({status}): {said}", self.program),
        }
    }

    async fn read_remainder(&mut self) {
        let mut chunk = [0; 1024];
        while self.stderr.is_some() {
            let read = read_some(&mut self.stderr, &mut chunk).await;
            self.keep(read, &chunk);
        }
    }

    /// Keep the start of a read from stderr, closing it at its end.
    fn keep(&mut self, read: std::io::Result<usize>, chunk: &[u8]) {
        match read {
            Ok(0) | Err(_) => self.stderr = None,
            Ok(read) => {
                let room = crate::subprocess::MAX_DIAGNOSTIC_CHARS.saturating_sub(self.said.len());
                self.said.extend_from_slice(&chunk[..read.min(room)]);
            }
        }
    }
}

/// Read what the pipe has, or wait forever once it is closed.
async fn read_some(pipe: &mut Option<ChildStderr>, chunk: &mut [u8]) -> std::io::Result<usize> {
    match pipe {
        Some(pipe) => pipe.read(chunk).await,
        None => std::future::pending().await,
    }
}

#[cfg(test)]
mod tests;
