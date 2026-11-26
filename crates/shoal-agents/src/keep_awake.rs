//! Holding the operating system's sleep inhibitor while agents have work that
//! `keep_awake` holds the machine for. `docs/design/power.md` says why the
//! inhibitor lasts only as long as a pipe the daemon holds open.
use shoal_ids::space_ids::SpaceId;
use std::{process::Stdio, sync::Arc, time::Duration};

use anyhow::{Context, Result, anyhow};
use rusqlite::Connection;
use shoal_core::config::{Config, machine::KeepAwake};
use shoal_daemon_base::alerts::AlertKind;
use tokio::{
    io::AsyncReadExt,
    process::{Child, ChildStderr, ChildStdin, Command},
};

use crate::{
    Agents,
    away::Presence,
    daemon::{Daemon, Records},
    log,
    state::AgentState,
    turn_state,
};

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

pub async fn run(agents: Arc<Agents>, daemon: Arc<dyn Daemon>) {
    let mut changed = agents.store.watch_changes();
    let mut reloaded = agents.config.watch();
    let mut presence = agents.presence();
    let mut hold = Hold::Released;
    loop {
        let config = agents.config.current();
        let held_for = held_for(config.machine.keep_awake, *presence.borrow_and_update());
        if let Some(wanted) = wanted(&agents, held_for).await {
            hold = update(&*daemon, hold, wanted, &config, INHIBITOR).await;
        }
        let ended = tokio::select! {
            _ = changed.changed() => None,
            _ = reloaded.changed() => None,
            _ = presence.changed() => None,
            error = hold.ended() => Some(error),
            () = tokio::time::sleep(SWEEP_INTERVAL) => None,
        };
        if let Some(error) = ended {
            hold = failed(&*daemon, error, config).await;
        }
        tokio::time::sleep(SETTLE).await;
        changed.borrow_and_update();
        reloaded.borrow_and_update();
    }
}

/// The work that holds the machine awake.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Work {
    /// None does.
    None,
    /// Handing spaces over to other machines, whatever agents do.
    HandingOver,
    /// An agent working on a turn.
    Turns,
    /// An agent working on a turn, or idle while an open PR whose activity
    /// Shoal delivers to it as its next turn is watched.
    TurnsAndWatches,
}

/// The work that holds the machine awake under `level` while the user is
/// where `presence` says.
fn held_for(level: KeepAwake, presence: Presence) -> Work {
    match level {
        KeepAwake::Off => Work::None,
        KeepAwake::Handover if presence.handing_over => Work::HandingOver,
        KeepAwake::Handover => Work::None,
        KeepAwake::Finish if presence.away => Work::Turns,
        KeepAwake::Finish | KeepAwake::On => Work::TurnsAndWatches,
    }
}

/// Whether agents have the work that holds the inhibitor, `held_for`, or
/// `None` when the lookup failed and the current hold should stay.
async fn wanted(agents: &Agents, held_for: Work) -> Option<bool> {
    match held_for {
        Work::None => return Some(false),
        Work::HandingOver => return Some(true),
        Work::Turns | Work::TurnsAndWatches => {}
    }
    match agents.agents_have_work(held_for).await {
        Ok(work) => Some(work),
        Err(error) => {
            log!("keep awake: {error:#}");
            None
        }
    }
}

impl Agents {
    /// Whether agents have the `work` that holds the machine awake.
    pub async fn agents_have_work(&self, work: Work) -> Result<bool> {
        let running = self.running_agent_workspaces().await;
        let records = self.records.clone();
        self.store
            .run(move |db| agents_have_work(db, &*records, &running, work))
            .await
    }
}

/// Whether an agent has `work`, the agents of the spaces `running` its own.
fn agents_have_work(
    db: &Connection,
    records: &dyn Records,
    running: &[SpaceId],
    work: Work,
) -> Result<bool> {
    for (id, state) in turn_state::all(db)? {
        let work = match (state, work) {
            (AgentState::Working, _) => true,
            (AgentState::Idle, Work::TurnsAndWatches) => {
                records.receives_open_pr_activity(db, &id, running)?
            }
            _ => false,
        };
        if work {
            return Ok(true);
        }
    }
    Ok(false)
}

async fn update(
    daemon: &dyn Daemon,
    hold: Hold,
    wanted: bool,
    config: &Arc<Config>,
    inhibitor: &[&str],
) -> Hold {
    match (hold, wanted) {
        (Hold::Failed(failed), true) if Arc::ptr_eq(&failed, config) => Hold::Failed(failed),
        (Hold::Released | Hold::Failed(_), true) => match Inhibitor::start(inhibitor) {
            Ok(inhibitor) => Hold::Held(Box::new(inhibitor)),
            Err(error) => failed(daemon, error, config.clone()).await,
        },
        (hold @ Hold::Held(_), true) => hold,
        (_, false) => Hold::Released,
    }
}

async fn failed(daemon: &dyn Daemon, error: anyhow::Error, config: Arc<Config>) -> Hold {
    let message = format!("Shoal could not keep this machine awake: {error:#}");
    log!("keep awake: {error:#}");
    daemon
        .notify(None, AlertKind::KeepAwakeFailed, message)
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
        match shoal_subprocess::diagnostic(&self.said).trim() {
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
                let room = shoal_subprocess::MAX_DIAGNOSTIC_CHARS.saturating_sub(self.said.len());
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
