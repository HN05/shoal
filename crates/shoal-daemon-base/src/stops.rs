//! How the daemon stops a space's executions: why, which their wrappers
//! show, what the stop saves for `shoal resume`, and the proof it requires
//! of an execution that disconnected.
use shoal_protocol::agent_ends::StopCause;

/// The proof required after stopping a disconnected execution. Both policies
/// signal only native identity-verified processes and reject owned survivors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopPolicy {
    /// Retain uncertain records; only clear executions proven fully stopped.
    RequireCompleteProof,
    /// As `RequireCompleteProof`, but the user confirmed that executions
    /// without a recorded launch identity ended once nothing visible survives.
    Acknowledged,
    /// Stop verified processes, leaving records for successful workspace removal.
    /// Unrelated or unverifiable processes do not block that removal.
    ForRemoval,
}

/// Why the daemon stops executions, which their wrappers show, and what the
/// stop saves for `shoal resume`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StopRequest {
    pub reason: String,
    pub save: Save,
    /// What the stopped agent's `agent_stopped` event names as its cause.
    pub cause: StopCause,
}

/// What a stop saves for `shoal resume`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Save {
    Nothing,
    /// Agent sessions and interrupted commands.
    Everything,
    /// Agent sessions, which the removed workspace's removal record keeps.
    RemovedAgents,
    /// Agent sessions and interrupted commands, saved before the stop since
    /// the system or the daemon shuts down, with restores that need no user
    /// queued.
    Shutdown,
}

impl StopRequest {
    /// A stop for `reason` that saves `save`, which names that reason as
    /// its cause.
    pub fn requested(reason: impl Into<String>, save: Save) -> Self {
        let reason = reason.into();
        Self {
            cause: StopCause::Requested {
                reason: reason.clone(),
            },
            reason,
            save,
        }
    }

    /// The stop `shoal stop` requests.
    pub fn manual() -> Self {
        Self {
            reason: "stopped by shoal stop".into(),
            save: Save::Everything,
            cause: StopCause::UserStop,
        }
    }

    /// The stop `shoal queue cancel` requests of an agent waiting for its
    /// restore.
    pub fn queue_cancel() -> Self {
        Self::requested(
            "its restore was cancelled with shoal queue cancel",
            Save::Everything,
        )
    }

    /// The stop of an agent protection stops for `reason` that the store
    /// could not record, which saves its session for `shoal resume`.
    pub fn unrecorded(reason: &str) -> Self {
        Self::requested(
            format!("{reason}; its stop could not be recorded"),
            Save::Everything,
        )
    }

    /// The stop of a space's agents before `shoal move --ssh` exports it,
    /// which saves their sessions for the import on the other machine, or
    /// for their restores here when that import fails.
    pub fn moving() -> Self {
        Self::requested("its space moves to another machine", Save::Everything)
    }

    /// The stop of a space's agents before its removal here, since another
    /// machine took the space over while this one was offline, which saves
    /// their sessions in its removal record.
    pub fn taken_over() -> Self {
        Self::requested("another machine took its space over", Save::Everything)
    }

    pub fn repair() -> Self {
        Self::requested("stopped to repair the workspace", Save::Nothing)
    }

    /// The stop before the system or the daemon shuts down, for `reason`.
    pub fn shutdown(reason: &str) -> Self {
        Self {
            reason: reason.into(),
            save: Save::Shutdown,
            cause: StopCause::Shutdown,
        }
    }

    /// The agent of a chat that became a workspace, whose session its
    /// conversion keeps to continue in the worktree.
    pub fn conversion() -> Self {
        Self::requested("its chat became a workspace", Save::Nothing)
    }

    /// The agent of a space whose work another agent tool takes over from
    /// the copies of its sessions the handoff made.
    pub fn handoff() -> Self {
        Self::requested(
            "another agent tool takes its space's work over",
            Save::Nothing,
        )
    }

    /// Removal of a missing worktree, which keeps no checkpoint or record.
    pub fn removal() -> Self {
        Self {
            reason: "the workspace is being removed".into(),
            save: Save::Nothing,
            cause: StopCause::Removal,
        }
    }

    /// Removal of a chat or an existing worktree, whose removal record keeps
    /// its agents.
    pub fn restorable_removal() -> Self {
        Self {
            save: Save::RemovedAgents,
            ..Self::removal()
        }
    }

    /// Whether the stop is a removal whose removal record keeps the agents.
    pub fn keeps_removed_agents(&self) -> bool {
        self.save == Save::RemovedAgents
    }

    pub fn before_shutdown(&self) -> bool {
        self.save == Save::Shutdown
    }

    /// Whether the wrapper saves its agent's session, or its interrupted
    /// command when it runs no agent.
    pub fn saves(&self, agent: bool) -> bool {
        match self.save {
            Save::Nothing => false,
            Save::Everything | Save::Shutdown => true,
            Save::RemovedAgents => agent,
        }
    }
}
