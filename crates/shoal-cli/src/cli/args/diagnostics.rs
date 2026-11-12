//! Arguments of `shoal alerts`.
use crate::client::space_names::SpaceSelector;
use clap::{Args, Subcommand};

#[derive(Debug, Subcommand)]
#[command(defer = true)]
pub enum DiagnosticsCommand {
    /// Show resource conflicts, finished agents, and automatic cleanup.
    #[command(args_conflicts_with_subcommands = true)]
    Alerts {
        #[command(subcommand)]
        command: Option<AlertsCommand>,
        #[command(flatten)]
        list: AlertsArgs,
    },
}

// Arguments of `shoal alerts` without a subcommand, which lists or follows
// notifications.
#[derive(Debug, Args)]
pub struct AlertsArgs {
    /// Include notifications already shown.
    #[arg(short = 'A', long, conflicts_with = "follow")]
    pub all: bool,
    /// Keep printing new notifications as the daemon records them.
    #[arg(long)]
    pub follow: bool,
    /// Show notifications without marking them read.
    #[arg(long)]
    pub keep_unread: bool,
    /// Continue following after this notification, as the installed
    /// release does after an upgrade restarted the daemon.
    #[arg(long, hide = true, requires = "follow")]
    pub resume: Option<i64>,
    #[arg(long, default_value_t = 50, value_parser = clap::value_parser!(u32).range(1..=200))]
    pub limit: u32,
}

#[derive(Debug, Subcommand)]
#[command(defer = true)]
pub enum AlertsCommand {
    /// Send the user a notification about a space without marking it done.
    Send {
        /// One line of text, such as "PR #12 is ready to merge".
        message: String,
        /// Send a file with it, such as a screenshot, as PATH or PATH#TITLE;
        /// repeat for more. shoal files lists it, after cleanup too.
        #[arg(long = "attach", value_name = "FILE")]
        attach: Vec<String>,
        /// Space, a workspace or chat, the notification names; defaults to
        /// the current workspace or chat, or a picker.
        #[arg(short = 's', long)]
        space: Option<SpaceSelector>,
    },
    /// Delete all notifications, including unread ones.
    Clear,
    /// Withdraw an unread message the space sent with shoal alerts send.
    Withdraw {
        /// Notification number, as shoal alerts send printed it.
        id: i64,
        /// Space, a workspace or chat, that sent it; defaults to the current
        /// workspace or chat, or a picker.
        #[arg(short = 's', long)]
        space: Option<SpaceSelector>,
    },
}
