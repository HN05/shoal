//! Hold the operating system's sleep inhibitor while agents have work, when
//! `keep_awake` opts in, so an idle timeout or a closed laptop lid does not
//! suspend it. The inhibitor lasts only as long as a pipe the daemon holds
//! open, so it ends with the daemon however that stops.
use std::{process::Stdio, sync::Arc, time::Duration};

use anyhow::{Context, Result, anyhow};
use rusqlite::Connection;
use tokio::{
    io::AsyncReadExt,
    process::{Child, ChildStdin, Command},
};

use super::{log, notifications::NotificationKind, workspace::Manager};
use crate::state::AgentState;

/// Rechecks without a store change, such as after a failed lookup.
const SWEEP_INTERVAL: Duration = Duration::from_secs(30);
/// Lets a burst of state changes settle into one pass.
const SETTLE: Duration = Duration::from_millis(500);

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
    /// The inhibitor could not start or ended on its own. It is not retried
    /// until agents run out of work or the config reloads.
    Failed,
}

impl Hold {
    /// Wait for a held inhibitor to end on its own.
    async fn ended(&mut self) -> anyhow::Error {
        match self {
            Self::Held(inhibitor) => inhibitor.ended().await,
            Self::Released | Self::Failed => std::future::pending().await,
        }
    }
}

enum Wake {
    Changed,
    Reloaded,
    Ended(anyhow::Error),
}

pub(super) async fn run(manager: Arc<Manager>) {
    let mut changed = manager.store.watch_changes();
    let mut config = manager.watch_config();
    let mut hold = Hold::Released;
    loop {
        if let Some(wanted) = wanted(&manager).await {
            hold = update(&manager, hold, wanted).await;
        }
        let wake = tokio::select! {
            _ = changed.changed() => Wake::Changed,
            _ = config.changed() => Wake::Reloaded,
            error = hold.ended() => Wake::Ended(error),
            () = tokio::time::sleep(SWEEP_INTERVAL) => Wake::Changed,
        };
        match wake {
            Wake::Changed => {}
            // A reload may fix what made the inhibitor fail.
            Wake::Reloaded if matches!(hold, Hold::Failed) => hold = Hold::Released,
            Wake::Reloaded => {}
            Wake::Ended(error) => hold = failed(&manager, error).await,
        }
        tokio::time::sleep(SETTLE).await;
        changed.borrow_and_update();
        config.borrow_and_update();
    }
}

/// Whether to hold the inhibitor, or `None` when the lookup failed and the
/// current hold should stay.
async fn wanted(manager: &Manager) -> Option<bool> {
    if !manager.config().keep_awake {
        return Some(false);
    }
    match manager.store.run(agents_have_work).await {
        Ok(work) => Some(work),
        Err(error) => {
            log!("keep awake: {error:#}");
            None
        }
    }
}

/// Whether an agent is working on a turn.
fn agents_have_work(db: &mut Connection) -> Result<bool> {
    Ok(db.query_row(
        "SELECT EXISTS(SELECT 1 FROM workspace_agent_state WHERE state=?1)",
        [AgentState::Working],
        |row| row.get(0),
    )?)
}

async fn update(manager: &Manager, hold: Hold, wanted: bool) -> Hold {
    match (hold, wanted) {
        (Hold::Released, true) => match Inhibitor::start(INHIBITOR) {
            Ok(inhibitor) => Hold::Held(Box::new(inhibitor)),
            Err(error) => failed(manager, error).await,
        },
        (hold @ (Hold::Held(_) | Hold::Failed), true) => hold,
        (_, false) => Hold::Released,
    }
}

async fn failed(manager: &Manager, error: anyhow::Error) -> Hold {
    let message = format!("Shoal could not keep this machine awake: {error:#}");
    log!("keep awake: {error:#}");
    manager
        .notify(None, NotificationKind::KeepAwakeFailed, message)
        .await;
    Hold::Failed
}

/// A running inhibitor, which ends once dropped: its input closes and its
/// process is killed.
struct Inhibitor {
    program: String,
    child: Child,
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
        let input = child.stdin.take();
        Ok(Self {
            program: (*program).into(),
            child,
            _input: input,
        })
    }

    /// Wait for the inhibitor to end on its own, with what it said.
    async fn ended(&mut self) -> anyhow::Error {
        let status = match self.child.wait().await {
            Ok(status) => status,
            Err(error) => return anyhow!(error).context(format!("wait for {}", self.program)),
        };
        let mut output = String::new();
        if let Some(mut stderr) = self.child.stderr.take() {
            let _ = stderr.read_to_string(&mut output).await;
        }
        match output.trim() {
            "" => anyhow!("{} ended ({status})", self.program),
            output => anyhow!("{} ended ({status}): {output}", self.program),
        }
    }
}

#[cfg(test)]
mod tests;
