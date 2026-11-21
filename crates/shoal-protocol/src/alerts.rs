//! Alerts: one-way notices from the user, a space or Shoal, for the user or
//! a space's agent.
use std::path::PathBuf;

use crate::space_names::SpaceName;

use serde::{Deserialize, Serialize};
use shoal_ids::space_ids::{SpaceId, SpaceNumber};

use shoal_states::states;

states!(AlertKind {
    ApprovalRequested => "approval_requested",
    /// A workspace's agent withdrew an approval request the user had been shown.
    ApprovalWithdrawn => "approval_withdrawn",
    /// A push the user approved failed; approving it again retries.
    PushFailed => "push_failed",
    /// A post in a thread the recipient opened, holds or linked.
    ThreadPosted => "thread_posted",
    /// A thread came to the recipient from its earlier holder.
    ThreadForwarded => "thread_forwarded",
    /// A thread the recipient opened or holds was resolved or reopened.
    ThreadResolved => "thread_resolved",
    /// A resource or simulator request found no free capacity.
    ResourceBusy => "resource_busy",
    /// A preferred port was in use.
    PortConflict => "port_conflict",
    /// An agent launched through a Shoal shortcut ended.
    AgentExited => "agent_exited",
    /// The overload monitor asked a tracked agent to stop.
    AgentStopped => "agent_stopped",
    /// A stopped agent was authorized to restore its session.
    AgentResumed => "agent_resumed",
    /// A daemon restart left a stopped agent's processes uncertain.
    RecoveryBlocked => "recovery_blocked",
    /// A move ended where no command followed it to its end: one Shoal
    /// finished after its worker went, or one that waited for the turn of
    /// the agent it stops.
    MoveFinished => "move_finished",
    /// A space moved here from another joined machine, which its agent
    /// hears as it continues here.
    SpaceMoved => "space_moved",
    /// The daemon removed or forgot a space on its own.
    SpaceRemoved => "workspace_removed",
    /// An agent explicitly marked its space's work finished.
    SpaceDone => "workspace_done",
    /// A space's progress came to record what blocks it.
    SpaceBlocked => "space_blocked",
    /// An automatic removal was attempted and the workspace retained, or a
    /// cleanup pass failed.
    CleanupFailed => "cleanup_failed",
    /// A removal the user asked for failed after the command returned, and
    /// the workspace stays.
    RemovalFailed => "removal_failed",
    /// Disk protection, a system shutdown or the daemon's exit could not
    /// stop a workspace's tracked executions.
    StopFailed => "stop_failed",
    /// A best-effort event hook failed.
    HookFailed => "hook_failed",
    /// The gate could not be set up to decide a request, as when no agent
    /// it can run is configured, which denied the request.
    GateUnavailable => "gate_unavailable",
    /// Text the user or a space sent.
    Message => "message",
    /// A decision on an approval request the space made.
    ApprovalDecided => "approval_decided",
    /// The findings of a review of the space's work.
    ReviewFinished => "review_finished",
    /// A space below marked its work ready for review.
    ReadyMarked => "ready_marked",
    /// The gate decided an approval request of a space below.
    GateDecided => "gate_decided",
    /// The agent stayed idle as long as its mode wakes it after.
    IdleWake => "idle_wake",
    /// A timer the agent set with `shoal sleep` ran out.
    WakeTimer => "wake_timer",
    /// Shoal retries a turn an API failure ended.
    TurnRetried => "turn_retried",
    /// A turn's final message the agent marked to send went unsent: a
    /// prompt the user submitted answered the turn, or sending it failed.
    ChatMessageUnsent => "chat_message_unsent",
    /// The machine's memory, CPU or disk space runs short.
    LoadWarning => "load_warning",
    /// What blocks the space's work changed.
    BlockersChanged => "blockers_changed",
    /// Another space's work was absorbed into the space.
    SpaceAbsorbed => "space_absorbed",
    /// The branch the space builds on changed.
    BaseChanged => "base_changed",
    /// Activity on the issues and PRs of a space below without an agent of
    /// its own, which another machine runs.
    ItemActivity => "item_activity",
    /// The daemon started an agent from the queue.
    QueueStarted => "queue_started",
    /// A queued agent could not start.
    QueueFailed => "queue_failed",
    /// Starting a space warned, as of an item it could not link.
    StartWarned => "start_warned",
    /// A new chat's agent did not start, and the daemon removed the chat.
    StartFailed => "start_failed",
    /// The preparation of a workspace a start opened failed: creating its
    /// worktree, its setup or its post-setup hook.
    PreparationFailed => "preparation_failed",
    /// A seed build failed, or a new workspace could not start from it.
    SeedFailed => "seed_failed",
    /// Disk space cleanup emptied a repository's shared caches.
    CachesEmptied => "caches_emptied",
    /// Disk space cleanup deleted the files kept for a removed workspace or an
    /// removed chat.
    ResultsDeleted => "results_deleted",
    /// A forge limits the requests Shoal sends it, so polling waits.
    ForgeLimited => "forge_limited",
    /// The daemon stopped tracked work before the system shut down.
    SystemShutdown => "system_shutdown",
    /// The daemon stopped tracked work before a signal ended it.
    DaemonStopped => "daemon_stopped",
    /// The sleep inhibitor `keep_awake` holds could not start or ended.
    KeepAwakeFailed => "keep_awake_failed",
    /// The daemon removed a contribution repository nothing was left of.
    RepositoryRemoved => "repository_removed",
    /// An API failure ended an agent's turn and Shoal does not retry it.
    TurnFailed => "turn_failed",
    /// An agent's hook could not report its turn state.
    StateReportFailed => "state_report_failed",
    /// The daemon's environment or the global config has a problem, such as
    /// a missing tool, found when the daemon starts or reloads its config.
    EnvironmentProblem => "environment_problem",
    /// A repository's default branch has not synced with its remote for a while.
    SyncFailed => "sync_failed",
    /// File access in a directory timed out, as under heavy memory pressure or
    /// while a macOS privacy prompt waits.
    AccessStalled => "access_stalled",
    /// A repository's runner could not register, be deleted or run a job.
    RunnerFailed => "runner_failed",
    /// The current config format rejects a workspace's config file, which
    /// Shoal ignores.
    ConfigIgnored => "config_ignored",
    /// Shoal could not change an issue's assignees as its links say: take
    /// back an assignee it added once no space linked the issue, or put the
    /// agent account on while an agent works it or take it off once none
    /// does.
    UnassignFailed => "unassign_failed",
    /// A lease could not be released when the agent or demo that acquired
    /// it ended, and stays held.
    ReleaseFailed => "release_failed",
    /// The hub refused a machine or device, such as for another version,
    /// or this machine's link to the hub.
    HubRefused => "hub_refused",
    /// A second joined machine reported a space under an ID another
    /// machine's space has.
    SpaceConflict => "space_conflict",
    /// The hub gave a space a new number, since a space on another of its
    /// machines held its number first.
    SpaceRenumbered => "space_renumbered",
    /// The hub dropped a request or event it held for an offline machine
    /// once it expired.
    HubExpired => "hub_expired",
    /// The hub refused an intermittent machine's package of a space, as
    /// while its disk is low.
    HubPackageRefused => "hub_package_refused",
    /// A space another machine took over changed here after the package
    /// the takeover went by, and this machine removed its copy.
    SpaceDiverged => "space_diverged",
    /// An intermittent machine stayed unreachable while its spaces had
    /// work to do.
    MachineUnreachable => "machine_unreachable",
    /// The hub's copy of a shared config file replaced an edit this
    /// machine had not sent it, which Shoal kept as a backup.
    SharedFileKept => "shared_file_kept",
});

states!(Sender {
    /// The user, from a terminal outside every space.
    User => "user",
    /// A space's agent or process.
    Space => "space",
    /// Shoal itself, about an event it saw.
    Shoal => "shoal",
});

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Alert {
    pub id: i64,
    pub created_at: crate::time::UnixTime,
    pub kind: AlertKind,
    pub sender: Sender,
    /// The space the alert is for; none when it is for the user.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recipient: Option<Recipient>,
    /// The space that sent the alert, or else the space it is about, by
    /// its name then; kept after removal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub space: Option<SpaceName>,
    /// The ID of that space, which selects it while it exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub space_id: Option<SpaceId>,
    /// That space's number while it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub number: Option<SpaceNumber>,
    /// Empty when the alert is only its files.
    pub message: String,
    pub read: bool,
    /// Shoal's copies of the files attached to it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<crate::files::SentFile>,
}

impl Alert {
    /// The paths of Shoal's copies of its files.
    pub fn attachments(&self) -> Vec<PathBuf> {
        self.files.iter().map(|file| file.path.clone()).collect()
    }

    /// Its text as an agent reads it, after the space it names.
    pub fn text(&self) -> String {
        match self.space_label() {
            Some(space) => format!("{space}: {}", self.message),
            None => self.message.clone(),
        }
    }

    /// The space it names, by its number while it has one, since names
    /// repeat.
    pub fn space_label(&self) -> Option<String> {
        let name = self.space.as_ref()?;
        Some(match self.number {
            Some(number) => crate::space_names::label(number, name),
            None => name.to_string(),
        })
    }
}

/// Where an alert went: the alerts recorded, which their sender may
/// withdraw while unread, to the user or the spaces it reached, or passed
/// up to a coordinator on another machine.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sent {
    /// The IDs of the alerts recorded, one for each recipient that took it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub alerts: Vec<i64>,
    /// Whether it went to the user.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub user: bool,
    /// Whether it went to the coordinator on another machine.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub passed_up: bool,
    /// The spaces it reached.
    #[serde(flatten)]
    pub reached: Reached,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Cleared {
    /// Alerts deleted, read or not.
    pub cleared: u64,
}

/// The spaces a message reached, by whether their agent receives it now or
/// once it next runs, or they have none.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reached {
    /// The spaces whose agent runs.
    pub now: ReachedSpaces,
    /// The spaces whose agent is stopped, queued, waits for its restore or
    /// has a wrapper waiting to reattach.
    pub on_start: ReachedSpaces,
    /// The space without an agent, where the message waits for one to
    /// start; never one a message to every agent reaches.
    #[serde(default, skip_serializing_if = "ReachedSpaces::is_empty")]
    pub without_agent: ReachedSpaces,
    /// The spaces that refused the message, such as one whose unread
    /// messages are full, which the others still received.
    #[serde(default, skip_serializing_if = "Refused::is_empty")]
    pub refused: Refused,
}

impl Reached {
    pub fn is_empty(&self) -> bool {
        self.now.is_empty()
            && self.on_start.is_empty()
            && self.without_agent.is_empty()
            && self.refused.is_empty()
    }
}

/// Workspaces and chats that refused a message.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Refused {
    pub workspaces: Vec<RefusedSpace>,
    pub chats: Vec<RefusedSpace>,
}

impl Refused {
    pub fn is_empty(&self) -> bool {
        self.workspaces.is_empty() && self.chats.is_empty()
    }
}

/// A space that refused a message, with why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefusedSpace {
    #[serde(flatten)]
    pub space: Recipient,
    pub error: String,
}

/// Workspaces and chats a message reached.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReachedSpaces {
    pub workspaces: Vec<Recipient>,
    pub chats: Vec<Recipient>,
}

/// A space a message was sent to, by its number, ID and name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Recipient {
    pub number: SpaceNumber,
    pub id: SpaceId,
    pub name: SpaceName,
}

impl Recipient {
    /// Its number with its name, as text that names it does.
    pub fn label(&self) -> String {
        crate::space_names::label(self.number, &self.name)
    }
}

impl ReachedSpaces {
    pub fn is_empty(&self) -> bool {
        self.workspaces.is_empty() && self.chats.is_empty()
    }
}
