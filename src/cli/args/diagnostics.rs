//! Arguments of `shoal alerts`.
use clap::Subcommand;

#[derive(Debug, Subcommand)]
#[command(defer = true)]
pub enum DiagnosticsCommand {
    /// Show resource conflicts, finished agents, and automatic cleanup.
    #[command(args_conflicts_with_subcommands = true)]
    Alerts {
        #[command(subcommand)]
        command: Option<AlertsCommand>,
        /// Include notifications already shown.
        #[arg(long, conflicts_with = "follow")]
        all: bool,
        /// Keep printing new notifications as the daemon records them.
        #[arg(long)]
        follow: bool,
        /// Show notifications without marking them read.
        #[arg(long)]
        keep_unread: bool,
        /// Continue following after this notification, as the installed
        /// release does after an upgrade restarted the daemon.
        #[arg(long, hide = true, requires = "follow")]
        resume: Option<i64>,
        #[arg(long, default_value_t = 50, value_parser = clap::value_parser!(u32).range(1..=200))]
        limit: u32,
    },
}

#[derive(Debug, Subcommand)]
#[command(defer = true)]
pub enum AlertsCommand {
    /// Send the user a notification about a workspace without marking it done.
    Send {
        /// One line of text, such as "PR #12 is ready to merge".
        message: String,
        /// Workspace to use; defaults to the current workspace or picker.
        #[arg(long)]
        workspace: Option<String>,
    },
    /// Delete all notifications, including unread ones.
    Clear,
    /// Withdraw an unread message the workspace sent with shoal alerts send.
    Withdraw {
        /// Notification number, as shoal alerts send printed it.
        id: i64,
        /// Workspace to use; defaults to the current workspace or picker.
        #[arg(long)]
        workspace: Option<String>,
    },
}
