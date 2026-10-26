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
    /// Report the agent's turn state; Shoal's agent hooks call it.
    AgentState {
        state: crate::state::AgentState,
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
    /// Add the planned workspace inside its Herdr tab.
    #[command(name = internal::HERDR)]
    Herdr {
        #[arg(long)]
        close_when_done: bool,
        #[arg(env = crate::env::ADD_PLAN, hide_env_values = true)]
        plan: String,
    },
    /// Close a Herdr tab once its workspace completes or is removed.
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
    /// Add the planned workspace inside its detached zmx session.
    #[command(name = internal::ADD)]
    Add {
        #[arg(env = crate::env::ADD_PLAN, hide_env_values = true)]
        plan: String,
    },
    /// Tracked agent inside a zmx session; consumes recovery records once started.
    #[command(name = internal::SESSION)]
    Session {
        workspace: String,
        agent: String,
        #[arg(long = "record")]
        records: Vec<PathBuf>,
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
    /// Copy the main checkout's ignored files inside tracked setup, then run setup.
    #[command(name = internal::COPY_IGNORED)]
    CopyIgnored {
        #[arg(last = true)]
        setup_cmd: Option<PathBuf>,
    },
}
