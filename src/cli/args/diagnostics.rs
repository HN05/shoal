use clap::Subcommand;

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
