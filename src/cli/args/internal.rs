//! Arguments of commands Shoal, its agent hooks and integrations run.
use std::{ffi::OsString, path::PathBuf};

use clap::Subcommand;

use crate::client::{events::EventCursor, space_names::SpaceSelector};
use crate::{cli::commands, sessions::HostName, workers};

#[derive(Debug, Subcommand)]
#[command(defer = true)]
pub enum InternalCommand {
    /// Stream durable workspace lifecycle events without consuming notifications.
    Events {
        /// Keep the stream open as the daemon records new events.
        #[arg(long)]
        follow: bool,
        /// Replay events after this event ID.
        #[arg(long = "since")]
        after: Option<EventCursor>,
    },
    /// Print a view's rows for the bare `shoal` picker to reload.
    MenuRows { view: commands::menu::View },
    /// Print the fzf actions that switch the bare `shoal` picker to its next view.
    MenuNext,
    /// Print the `shoal approvals` picker's rows for it to reload.
    #[command(name = workers::APPROVAL_ROWS)]
    ApprovalRows {
        #[arg(long)]
        space: Option<String>,
    },
    /// Decide the request a `shoal approvals` picker row names, and print the outcome.
    #[command(name = workers::APPROVAL_DECIDE)]
    ApprovalDecide {
        decision: crate::cli::args::approvals::Decision,
        row: String,
    },
    /// Send a proxy's report for an agent wrapper of an older release, which
    /// an upgraded daemon's protocol no longer accepts.
    #[command(name = workers::PROXY_REPORT)]
    ProxyReport { report: String },
    /// Report the agent's turn state; Shoal's agent hooks call it.
    AgentState {
        state: crate::state::AgentState,
        /// The integration of an agent whose turn this idle report ends,
        /// which reads how long the agent's prompt cache lives.
        #[arg(long, value_name = "INTEGRATION")]
        ends_turn: Option<crate::ai::IntegrationKind>,
        /// Workspace or chat to report for; defaults to the chat this runs
        /// in, else the caller's own or the current workspace.
        #[arg(long)]
        space: Option<String>,
    },
    /// Report a turn an API error ended, with the failure the hook event of an
    /// agent with this integration names; Shoal's agent hooks call it.
    TurnFailed {
        /// The integration whose hook event stdin carries.
        #[arg(long)]
        integration: crate::ai::IntegrationKind,
        /// Workspace or chat to report for; defaults to the chat this runs
        /// in, else the caller's own or the current workspace.
        #[arg(long)]
        space: Option<String>,
    },
    /// Wait for the next agent messages or item activity of the workspace or
    /// chat, print them and exit 2; Shoal's Claude Code `Stop` and `StopFailure`
    /// hooks run it in the background so the update starts the idle agent's
    /// next turn.
    Wake {
        /// The agent's process ID; the hook ends when that process exits.
        #[arg(long)]
        agent: u32,
        /// Workspace or chat whose updates to wait for; defaults to the
        /// agent's own.
        #[arg(long, hide = true)]
        space: Option<String>,
        /// The workspace the wake hook of an earlier release names when an
        /// upgrade hands its wait over to this one.
        #[arg(
            long = "workspace",
            hide = true,
            conflicts_with = "space",
            requires = "resume"
        )]
        handed_over: Option<SpaceSelector>,
        /// Continue a wait across a daemon restart, as the installed release
        /// does after an upgrade restarted the daemon.
        #[arg(long, hide = true)]
        resume: bool,
    },
    /// Print which workspace a starting subagent works in as hook context;
    /// Shoal's Claude Code and Codex `SubagentStart` hooks run it.
    SubagentStart,
    /// Split a child workspace for the worktree Claude Code creates and print
    /// its path; Shoal's Claude Code `WorktreeCreate` hook runs it.
    WorktreeCreate,
    /// Mark the child workspace at the worktree Claude Code removes done;
    /// Shoal's Claude Code `WorktreeRemove` hook runs it.
    WorktreeRemove,
    /// Finish a planned merge inside its tracked execution.
    #[command(name = workers::MERGE)]
    Merge {
        /// Where to write what it merged, or why it could not.
        #[arg(long)]
        result: PathBuf,
        plan: String,
    },
    /// Finish a planned unmerge inside its tracked execution.
    #[command(name = workers::UNMERGE)]
    Unmerge {
        /// Where to write what it undid, or why it could not.
        #[arg(long)]
        result: PathBuf,
        plan: String,
    },
    /// Close a terminal host's place once its workspace completes or is removed.
    #[command(name = workers::PLACE_WATCH)]
    PlaceWatch {
        host: HostName,
        workspace: String,
        place: String,
        /// What told the place apart when the watch began.
        #[arg(long)]
        instance: Option<String>,
        /// The HEAD of the kept completion the watch began under.
        #[arg(long)]
        kept_at: Option<String>,
    },
    /// The Herdr tab watcher of earlier releases, which continues here after
    /// an upgrade.
    #[command(name = workers::HERDR_WATCH)]
    HerdrWatch { workspace: String, tab: String },
    /// Detached execution wrapper; reports the launch on stdout, then keeps tracking it.
    #[command(name = workers::DETACHED)]
    Detached {
        workspace: String,
        #[arg(long)]
        log: PathBuf,
        #[arg(long)]
        agent: Option<String>,
        #[command(flatten)]
        model: crate::cli::args::ModelArgs,
        /// The mode the agent started in.
        #[arg(long, default_value = crate::config::modes::DEFAULT)]
        mode: String,
        #[arg(last = true, required = true)]
        command: Vec<OsString>,
    },
    /// Run a tool with its configured secrets; an agent's secret launchers call it.
    #[command(name = workers::SECRET_TOOL)]
    SecretTool {
        launchers: PathBuf,
        tool: String,
        #[arg(last = true)]
        args: Vec<OsString>,
    },
    /// Git's remote helper for an agent's pushes, which asks the daemon
    /// before pushing; the agent's push helper launcher calls it.
    #[command(name = workers::PUSH_GATE)]
    PushGate { remote: String, url: String },
    /// Run an operation the daemon started, reading its order on stdin.
    #[command(name = workers::OPERATION)]
    Operation,
    /// Start an admitted agent queue entry; the daemon runs it.
    #[command(name = workers::QUEUE_START)]
    QueueStart { entry: String },
    /// Add the planned workspace inside the place `host` opened for it.
    #[command(name = workers::ADD)]
    Add {
        #[arg(long)]
        host: HostName,
        /// Close the place once the workspace is done.
        #[arg(long)]
        close_when_done: bool,
        /// The place was opened focused, showing the workspace's session.
        #[arg(long)]
        focused: bool,
        #[arg(env = crate::env::ADD_PLAN, hide_env_values = true)]
        plan: String,
    },
    /// Tracked agent inside a new place; consumes recovery records once started.
    #[command(name = workers::SESSION)]
    Session {
        workspace: String,
        agent: String,
        #[command(flatten)]
        model: crate::cli::args::ModelArgs,
        /// The mode the agent started in.
        #[arg(long, default_value = crate::config::modes::DEFAULT)]
        mode: String,
        #[arg(long = "record")]
        records: Vec<PathBuf>,
        /// The host that opened the place.
        #[arg(long)]
        host: HostName,
        /// Close the place of this host once the workspace is done.
        #[arg(long)]
        close_in: Option<HostName>,
        #[arg(last = true, required = true)]
        command: Vec<OsString>,
    },
    /// A demo's wrapper, restarting its command until the demo is stopped.
    #[command(name = workers::DEMO)]
    Demo {
        workspace: String,
        #[arg(long)]
        launch: String,
        #[arg(last = true, required = true)]
        command: Vec<OsString>,
    },
    /// Run the command a launch file carries, deleting the file first; hosts
    /// that cannot set a place's environment type it into the place.
    #[command(name = workers::LAUNCH)]
    Launch { file: PathBuf },
    /// Continue an agent's wrapper from the installed binary, which an
    /// upgrade put in place of its own, over the daemon connection it hands over.
    #[command(name = workers::TAKE_OVER)]
    TakeOver {
        workspace: String,
        #[arg(long)]
        agent: String,
        #[command(flatten)]
        model: crate::cli::args::ModelArgs,
        /// The mode the agent started in.
        #[arg(long, default_value = crate::config::modes::DEFAULT)]
        mode: String,
        /// The descriptor of the wrapper's daemon connection.
        #[arg(long)]
        connection: i32,
        /// The recovery record the agent resumes from.
        #[arg(long)]
        record: Option<PathBuf>,
        /// The log a detached agent writes to.
        #[arg(long)]
        log: Option<PathBuf>,
    },
    /// Copy the main checkout's ignored files inside tracked setup, then run setup.
    #[command(name = workers::COPY_IGNORED)]
    CopyIgnored {
        #[arg(last = true)]
        setup_cmd: Option<PathBuf>,
    },
}
