//! Shoal enums whose persisted and wire spellings remain stable.

use shoal_states::states;

states!(WorkspaceState: Variants {
    Preparing => "preparing",
    Ready => "ready",
    Stopping => "stopping",
    Removing => "removing",
    Reconciling => "reconciling",
    /// Returning to a checkpoint.
    Restoring => "restoring",
    /// Fast-forwarding to its branch's pushed head.
    Updating => "updating",
    Failed => "failed",
});

impl WorkspaceState {
    /// Lifecycle operations interrupted when the daemon stops.
    pub fn transient() -> impl Iterator<Item = Self> {
        Self::ALL.iter().copied().filter(|state| match state {
            Self::Preparing
            | Self::Stopping
            | Self::Removing
            | Self::Reconciling
            | Self::Restoring
            | Self::Updating => true,
            Self::Ready | Self::Failed => false,
        })
    }

    /// A workspace being stopped, removed, reconciled, restored or updated
    /// takes no link changes.
    pub fn accepts_links(self) -> bool {
        matches!(self, Self::Ready | Self::Preparing | Self::Failed)
    }
}

states!(ExecutionState {
    Running => "running",
    Unknown => "unknown",
});
states!(QueueState {
    /// Waiting for an agent slot.
    Queued => "queued",
    /// Admitted to a slot; its agent has not registered yet.
    Starting => "starting",
});
states!(QueueKind {
    /// A new agent that `shoal start` plans.
    Launch => "launch",
    /// A protection-stopped agent's automatic restore in its own workspace.
    Restore => "restore",
    /// A CI job waiting in the forge's queue for this machine's runner.
    Job => "job",
});
states!(BlockerKind {
    /// A space, by ID.
    Space => "space",
    /// A queued launch, by entry ID, until it opens its workspace.
    Launch => "launch",
    /// An issue, by URL.
    Issue => "issue",
    /// A PR, by URL.
    Pr => "pr",
    /// A CI run, by URL.
    CiRun => "ci_run",
    /// The setup of the workspace a launch opens, by ID, after it failed,
    /// until a preparation there gets past its post-setup hook.
    Setup => "setup",
});
states!(AgentState: ValueEnum {
    /// The agent is working on a turn.
    Working => "working",
    /// The agent stopped mid-turn for the user, such as a permission prompt.
    Waiting => "waiting",
    /// The agent finished its turn.
    Idle => "idle",
});
states!(
    /// How long a space's agent sleeps: while it does, Shoal does not wake
    /// it for having stayed idle, though updates and messages still do.
    Sleep {
        /// Until its next turn starts.
        Turn => "turn",
        /// Until `shoal sleep --undo`.
        Permanent => "permanent",
    }
);
states!(TurnFailure {
    /// The API refused the request for a rate or usage limit.
    RateLimited => "rate_limited",
    /// The API was overloaded.
    Overloaded => "overloaded",
    /// The API failed on its side.
    ServerError => "server_error",
    /// Any other error, such as authentication or an invalid request.
    Other => "other",
});

impl TurnFailure {
    /// Whether the same turn may succeed once the API recovers.
    pub fn retryable(self) -> bool {
        self != Self::Other
    }

    /// The failure as the user reads it.
    pub fn describe(self) -> &'static str {
        match self {
            Self::RateLimited => "rate limited",
            Self::Overloaded => "API overloaded",
            Self::ServerError => "API server error",
            Self::Other => "API error",
        }
    }
}

#[cfg(all(test, feature = "rusqlite"))]
mod tests;
