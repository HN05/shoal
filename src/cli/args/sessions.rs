//! Arguments of commands for agent sessions and demos.
use std::ffi::OsString;

use clap::Subcommand;

use crate::{
    agent::Agent,
    cli::args::{AgentParser, WorkspaceTargets},
    model::QueuePlace,
};

#[derive(Debug, Subcommand)]
#[command(defer = true)]
pub enum SessionsCommand {
    /// Queue the restore of stopped agent sessions and list interrupted commands.
    ///
    /// The daemon starts queued restores as `max_agents` allows: where each
    /// agent stopped, otherwise in a background Herdr tab or a detached zmx
    /// session. `--all` leaves out workspaces whose assignment is done.
    Resume {
        #[command(flatten)]
        targets: WorkspaceTargets,
        /// Select a stopped execution when the workspace has several records.
        #[arg(long, conflicts_with = "all")]
        execution: Option<String>,
        /// Forget the saved agents and commands instead of resuming them.
        #[arg(long)]
        discard: bool,
        /// Start at once regardless of the agent limits; one agent restores
        /// in this terminal.
        #[arg(long, conflicts_with = "discard")]
        now: bool,
        /// The queue entry whose slot the restored agent takes, for the
        /// daemon's start of a queued restore.
        #[arg(long, hide = true, requires_all = ["execution", "now"])]
        queue_entry: Option<String>,
        /// The host whose place, opened in the background for this restore,
        /// closes once the workspace is done.
        #[arg(long, hide = true, requires_all = ["execution", "now"])]
        close_in: Option<String>,
    },
    /// Restart running agents in their terminals, resuming their sessions.
    ///
    /// Agents restart together and run the agent's current binary; commands
    /// keep running.
    Restart {
        #[command(flatten)]
        targets: WorkspaceTargets,
        /// Restart only agents of this name, such as claude.
        #[arg(long, value_parser = AgentParser)]
        agent: Option<Agent>,
        /// Restart each agent once its turn ends.
        #[arg(long)]
        wait: bool,
        /// Restart one agent at a time, idle ones first, 10 seconds apart.
        #[arg(long)]
        sequential: bool,
    },
    /// List the agent queue, or start, move or cancel an entry.
    ///
    /// The queue holds the agent launches and restores of every repository
    /// waiting for a slot under max_agents, named by workspace. Without a
    /// command on a terminal, choose an entry, then an action.
    #[command(args_conflicts_with_subcommands = true)]
    Queue {
        #[command(subcommand)]
        command: Option<QueueCommand>,
        /// List only this registered repository's entries.
        #[arg(long = "repo")]
        repository: Option<String>,
        /// Print the list instead of opening the picker, as filters and JSON do.
        #[arg(long)]
        print: bool,
    },
    /// Attach this terminal to a workspace's or chat's agent session; Ctrl-\ detaches.
    ///
    /// Terminal agents and chats run in zmx sessions that keep running after
    /// their terminal closes or detaches. Without a target, attaches the
    /// current workspace's session, else the current checkout's only running
    /// chat, else offers a picker.
    Attach {
        /// Workspace or running chat, by name.
        #[arg(id = "workspace", value_name = "TARGET")]
        target: Option<String>,
        /// Session to attach when the workspace has several.
        #[arg(long)]
        session: Option<String>,
    },
    /// Open a demo an agent started, or start, list and stop demos.
    ///
    /// A demo is a command Shoal keeps running in a workspace for you to try:
    /// a terminal app, a web server or a simulator app.
    #[command(args_conflicts_with_subcommands = true)]
    Demo {
        #[command(subcommand)]
        command: Option<DemoCommand>,
        /// Demo to open; without one, pick from the running demos.
        name: Option<String>,
        /// Workspace whose demos to offer; defaults to the current workspace,
        /// otherwise every workspace.
        #[arg(long)]
        workspace: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
#[command(defer = true)]
pub enum QueueCommand {
    /// Start a waiting entry now, regardless of the agent limits.
    Start {
        /// Entry by workspace name or ID; omit to choose one.
        entry: Option<String>,
    },
    /// Move an entry to another position, or stop a running agent and
    /// queue its restore there.
    Move {
        /// Entry by workspace name or ID, or a workspace with a running
        /// agent; omit to choose an entry.
        entry: Option<String>,
        /// 1-based position as shoal queue numbers them, or last; omit to
        /// choose the entry to move in front of.
        #[arg(value_parser = queue_position)]
        position: Option<QueuePlace>,
    },
    /// Cancel a waiting entry; a cancelled restore leaves its agent stopped.
    Cancel {
        /// Entry by workspace name or ID; omit to choose one.
        entry: Option<String>,
    },
}

/// A position as `shoal queue` numbers entries, or `last`.
fn queue_position(input: &str) -> Result<QueuePlace, String> {
    if input == "last" {
        return Ok(QueuePlace::Last);
    }
    match input.parse() {
        Ok(position) if position > 0 => Ok(QueuePlace::Position(position)),
        _ => Err("expected a position from 1, or last".into()),
    }
}

#[derive(Debug, Subcommand)]
#[command(defer = true)]
pub enum DemoCommand {
    /// Start a demo and tell the user how to open it; replaces a running
    /// demo of the same name.
    Start {
        /// A configured demo, or the name for the command after `--`;
        /// defaults to the only configured demo, or `demo` for a command.
        name: Option<String>,
        /// How the demo reaches the user, replacing the configured preset.
        #[arg(long, value_enum)]
        preset: Option<crate::demos::Preset>,
        /// A web demo's URL scheme, replacing the configured one; http by default.
        #[arg(long, value_enum)]
        scheme: Option<crate::demos::Scheme>,
        /// Shell command `shoal demo NAME` runs instead of opening the demo.
        #[arg(long, value_name = "COMMAND")]
        open: Option<String>,
        /// Port to reserve before the demo starts; repeat for more.
        #[arg(long = "port", value_name = "NAME")]
        ports: Vec<String>,
        /// Wait for a simulator when every one is busy.
        #[arg(long)]
        queue: bool,
        /// Workspace to use; defaults to the current workspace or picker.
        #[arg(long)]
        workspace: Option<String>,
        /// The command to run instead of the configured one.
        #[arg(last = true)]
        command: Vec<OsString>,
    },
    /// Stop a demo, or every demo of the workspace.
    Stop {
        name: Option<String>,
        /// Workspace to use; defaults to the current workspace or picker.
        #[arg(long)]
        workspace: Option<String>,
    },
    /// List configured and running demos.
    Ls {
        /// Workspace to list; defaults to the current workspace, otherwise
        /// the running demos of every workspace.
        #[arg(long)]
        workspace: Option<String>,
    },
    /// Print what a running demo shows.
    Screen {
        name: String,
        /// Lines to print from the end.
        #[arg(long, default_value_t = 50)]
        lines: usize,
        /// Workspace to use; defaults to the current workspace or picker.
        #[arg(long)]
        workspace: Option<String>,
    },
    /// Type text into a running demo, such as $'q' or $'\r' for Enter.
    Send {
        name: String,
        keys: String,
        /// Workspace to use; defaults to the current workspace or picker.
        #[arg(long)]
        workspace: Option<String>,
    },
}
