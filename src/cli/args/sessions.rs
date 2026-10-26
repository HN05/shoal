//! Arguments of commands for agent sessions and demos.
use std::ffi::OsString;

use clap::Subcommand;

use crate::cli::args::WorkspaceTargets;

#[derive(Debug, Subcommand)]
#[command(defer = true)]
pub enum SessionsCommand {
    /// Restore stopped agent sessions and list interrupted commands.
    Resume {
        #[command(flatten)]
        targets: WorkspaceTargets,
        /// Select a stopped execution when the workspace has several records.
        #[arg(long, conflicts_with = "all")]
        execution: Option<String>,
        /// Forget the saved agents and commands instead of resuming them.
        #[arg(long)]
        discard: bool,
    },
    /// Attach this terminal to a workspace's agent session; Ctrl-\ detaches.
    ///
    /// Terminal agents run in zmx sessions that keep running after their
    /// terminal closes or detaches.
    Attach {
        workspace: Option<String>,
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
