//! What a workspace saved for `shoal resume` when its agents and commands
//! stopped, which the daemon reads from its state, and how a resume
//! restores it.
use crate::execution_ids::ExecutionId;
use std::path::PathBuf;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::{
    agent::Agent,
    connection::{Daemon, request},
    protocol::RecoveryMethod,
    space_ids::SpaceId,
    space_names::SpaceSelector,
    time::UnixTime,
};

/// What the existing workspace `workspace` saved.
pub async fn read(daemon: &impl Daemon, workspace: SpaceSelector) -> Result<Saved> {
    request(daemon, RecoveryMethod::Saved { workspace }).await
}

/// The workspaces this caller sees that saved stopped agents or commands,
/// which are unfinished work; last sessions alone are not.
pub async fn stopped_workspaces(daemon: &impl Daemon) -> Result<Vec<SpaceId>> {
    request(daemon, RecoveryMethod::StoppedWorkspaces).await
}

/// What `shoal stop` or overload protection saved for one workspace.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Saved {
    /// The stopped agents, by execution ID.
    pub agents: Vec<SavedSession>,
    /// The last sessions of agents that exited, by execution ID.
    pub sessions: Vec<SavedSession>,
    /// The stopped commands, by execution ID.
    pub commands: Vec<SavedCommand>,
}

/// How a resume starts the restores it does not run in a terminal of its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Queuing {
    /// Queued as the limits allow, except a workspace's last session, or
    /// for `now` started by the resuming process where each stopped or in
    /// the background.
    Here { now: bool },
    /// Queued for the daemon to start, which a process without a terminal
    /// needs, at once regardless of the limits for `now`.
    Daemon { now: bool },
}

impl Queuing {
    /// How a process starts restores: one without a terminal, `on_terminal`
    /// false, has none to start them beside.
    pub fn of(on_terminal: bool, now: bool) -> Self {
        match on_terminal {
            true => Self::Here { now },
            false => Self::Daemon { now },
        }
    }
}

/// How a resume that starts restores as a [`Queuing`] says restores one
/// workspace's saved work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Restore {
    /// Continue its last session at once where it stopped or in the
    /// background, from the resuming process.
    Continue,
    /// Queue the restore of each agent and last session with
    /// [`crate::queue::restore`], started at once regardless of the limits
    /// when `now`.
    Queue { now: bool },
}

/// A stopped agent or the last session of one that exited.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SavedSession {
    pub id: ExecutionId,
    /// The file that keeps it, which its restore claims and consumes.
    pub path: PathBuf,
    /// Its agent, `None` once a restore consumed it or when its file cannot
    /// be read.
    pub agent: Option<Agent>,
    /// When it stopped, when known.
    pub stopped_at: Option<UnixTime>,
    /// Why it stopped, when known.
    pub stop_reason: Option<String>,
    /// It stopped between turns, with no work in flight, rather than
    /// during one.
    pub idle: bool,
    /// A live wrapper waits to restore it, which restores it only there.
    pub waits_in_wrapper: bool,
}

/// A stopped command, which a restored agent of its workspace is told about.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SavedCommand {
    pub id: ExecutionId,
    /// The file that keeps it, which the restore that reports it consumes.
    pub path: PathBuf,
    pub argv: Vec<String>,
}

impl Saved {
    /// Keep only the agent or last session of execution `id`; a selected
    /// agent keeps the workspace's commands too when `with_commands`, so it
    /// receives them.
    pub fn retain_execution(&mut self, id: &ExecutionId, with_commands: bool) {
        let selected = |session: &SavedSession| session.id == *id;
        if self.agents.iter().chain(&self.sessions).any(selected) {
            self.agents.retain(selected);
            self.sessions.retain(selected);
            if !with_commands {
                self.commands.clear();
            }
        } else {
            self.agents.clear();
            self.sessions.clear();
            self.commands.retain(|command| command.id == *id);
        }
    }

    /// The agent of each stopped agent and then each last session, `None`
    /// for one consumed since the daemon read it.
    pub fn agent_names(&self) -> impl Iterator<Item = Option<&Agent>> {
        (self.agents.iter().chain(&self.sessions)).map(|session| session.agent.as_ref())
    }

    /// Keep only the records of the executions `keep` selects.
    pub fn retain(&mut self, keep: impl Fn(&ExecutionId) -> bool) {
        self.agents.retain(|agent| keep(&agent.id));
        self.sessions.retain(|session| keep(&session.id));
        self.commands.retain(|command| keep(&command.id));
    }

    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty() && !self.is_unfinished()
    }

    /// Whether stopped agents or commands are unfinished work, which last
    /// sessions are not.
    pub fn is_unfinished(&self) -> bool {
        !(self.agents.is_empty() && self.commands.is_empty())
    }

    /// Whether stopped work waits for `shoal resume`, which an agent whose
    /// wrapper still waits to restore it does not.
    pub fn is_resumable(&self) -> bool {
        !self.commands.is_empty() || self.agents.iter().any(|agent| !agent.waits_in_wrapper)
    }

    /// The executions of the saved agents, last sessions and commands.
    pub fn execution_ids(&self) -> Vec<ExecutionId> {
        self.agents
            .iter()
            .chain(&self.sessions)
            .map(|session| session.id.clone())
            .chain(self.commands.iter().map(|command| command.id.clone()))
            .collect()
    }

    /// Drop agents whose wrapper is still waiting to restore them, which
    /// restore on their own and only through that wrapper.
    pub fn retain_unattended(&mut self) {
        self.agents.retain(|agent| !agent.waits_in_wrapper);
    }

    /// How a resume that starts restores as `queuing` says restores this
    /// work. Without stopped agents, a last session continues at once, as
    /// resuming the workspace by itself does not wait for a slot it never
    /// held: from a process with a terminal where it stopped or in the
    /// background, otherwise through the queue, started now.
    pub fn restore(&self, queuing: Queuing) -> Restore {
        let last_session = self.agents.is_empty() && !self.sessions.is_empty();
        match queuing {
            Queuing::Here { .. } if last_session => Restore::Continue,
            Queuing::Here { .. } => Restore::Queue { now: false },
            Queuing::Daemon { now } => Restore::Queue {
                now: now || last_session,
            },
        }
    }

    /// Whether an agent or last session restores outside a waiting wrapper,
    /// receiving the stopped commands; a wrapper's restore receives none.
    pub fn restores_outside_wrappers(&self) -> bool {
        (self.agents.iter().chain(&self.sessions)).any(|session| !session.waits_in_wrapper)
    }
}

#[cfg(test)]
mod tests;
