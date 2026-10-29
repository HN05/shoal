//! Arguments of commands Shoal, its agent hooks and integrations run.
use std::{ffi::OsString, path::PathBuf};

use clap::Subcommand;

use crate::cli::{commands, internal};

#[derive(Debug, Subcommand)]
#[command(defer = true)]
pub enum InternalCommand {
    /// Stream durable workspace lifecycle events without consuming notifications.
    Events {
        /// Keep the stream open as the daemon records new events.
        #[arg(long)]
        follow: bool,
        /// Replay events after this event ID.
        #[arg(long, value_parser = clap::value_parser!(i64).range(0..))]
        since: Option<i64>,
    },
    /// Print a view's rows for the bare `shoal` picker to reload.
    MenuRows { view: commands::menu::View },
    /// Print the fzf actions that switch the bare `shoal` picker to its next view.
    MenuNext,
    /// Send a proxy's report for an agent wrapper of an older release, which
    /// an upgraded daemon's protocol no longer accepts.
    #[command(name = internal::PROXY_REPORT)]
    ProxyReport { report: String },
    /// Report the agent's turn state; Shoal's agent hooks call it.
    AgentState {
        state: crate::state::AgentState,
        /// The integration of an agent whose turn this idle report ends,
        /// which reads how long the agent's prompt cache lives.
        #[arg(long, value_name = "INTEGRATION")]
        ends_turn: Option<crate::ai::IntegrationKind>,
        /// Workspace to use; defaults to the current workspace or picker.
        #[arg(long)]
        workspace: Option<String>,
    },
    /// Report a turn an API error ended, with the failure the hook event of an
    /// agent with this integration names; Shoal's agent hooks call it.
    TurnFailed {
        /// The integration whose hook event stdin carries.
        #[arg(long)]
        integration: crate::ai::IntegrationKind,
        /// Workspace to use; defaults to the current workspace or picker.
        #[arg(long)]
        workspace: Option<String>,
    },
    /// Wait for the next agent messages or item activity of the workspace or
    /// chat, print them and exit 2; Shoal's Claude Code `Stop` and `StopFailure`
    /// hooks run it in the background so the update starts the idle agent's
    /// next turn.
    Wake {
        /// The agent's process ID; the hook ends when that process exits.
        #[arg(long)]
        agent: u32,
        /// Workspace whose updates to wait for; defaults to the agent's own.
        #[arg(long, hide = true)]
        workspace: Option<String>,
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
    /// Finish a planned landing inside its tracked execution.
    #[command(name = internal::LAND)]
    Land {
        #[arg(long)]
        push: bool,
        plan: String,
    },
    /// Close a terminal host's place once its workspace completes or is removed.
    #[command(name = internal::PLACE_WATCH)]
    PlaceWatch {
        host: String,
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
    #[command(name = internal::HERDR_WATCH)]
    HerdrWatch { workspace: String, tab: String },
    /// Detached execution wrapper; reports the launch on stdout, then keeps tracking it.
    #[command(name = internal::DETACHED)]
    Detached {
        workspace: String,
        #[arg(long)]
        log: PathBuf,
        #[arg(long)]
        agent: Option<String>,
        #[arg(last = true, required = true)]
        command: Vec<OsString>,
    },
    /// Run a tool with its configured secrets; an agent's secret launchers call it.
    #[command(name = internal::SECRET_TOOL)]
    SecretTool {
        launchers: PathBuf,
        tool: String,
        #[arg(last = true)]
        args: Vec<OsString>,
    },
    /// Start an admitted agent queue entry; the daemon runs it.
    #[command(name = internal::QUEUE_START)]
    QueueStart { entry: String },
    /// Add the planned workspace inside the place `host` opened for it.
    #[command(name = internal::ADD)]
    Add {
        #[arg(long)]
        host: String,
        /// Close the place once the workspace is done.
        #[arg(long)]
        close_when_done: bool,
        #[arg(env = crate::env::ADD_PLAN, hide_env_values = true)]
        plan: String,
    },
    /// Tracked agent inside a new place; consumes recovery records once started.
    #[command(name = internal::SESSION)]
    Session {
        workspace: String,
        agent: String,
        #[arg(long = "record")]
        records: Vec<PathBuf>,
        /// The host that opened the place.
        #[arg(long)]
        host: String,
        /// Close the place of this host once the workspace is done.
        #[arg(long)]
        close_in: Option<String>,
        #[arg(last = true, required = true)]
        command: Vec<OsString>,
    },
    /// A demo's wrapper, restarting its command until the demo is stopped.
    #[command(name = internal::DEMO)]
    Demo {
        workspace: String,
        #[arg(long)]
        launch: String,
        #[arg(last = true, required = true)]
        command: Vec<OsString>,
    },
    /// Run the command a launch file carries, deleting the file first; hosts
    /// that cannot set a place's environment type it into the place.
    #[command(name = internal::LAUNCH)]
    Launch { file: PathBuf },
    /// Continue an agent's wrapper from the installed binary, which an
    /// upgrade put in place of its own, over the daemon connection it hands over.
    #[command(name = internal::TAKE_OVER)]
    TakeOver {
        workspace: String,
        #[arg(long)]
        agent: String,
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
    #[command(name = internal::COPY_IGNORED)]
    CopyIgnored {
        #[arg(last = true)]
        setup_cmd: Option<PathBuf>,
    },
}
