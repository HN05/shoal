//! Arguments of commands that open agents and apps in a workspace.
use std::ffi::OsString;

use clap::Subcommand;

#[derive(Debug, Subcommand)]
#[command(defer = true)]
pub enum AgentsCommand {
    /// Start a detached agent session in Happy.
    #[cfg(feature = "happy")]
    Happy {
        #[arg(value_enum)]
        agent: crate::happy::Agent,
        workspace: Option<String>,
        /// First message for the session; delivered through Happy's server when the agent takes no prompt argument.
        #[arg(long)]
        prompt: Option<String>,
        #[arg(last = true)]
        args: Vec<OsString>,
    },
}
