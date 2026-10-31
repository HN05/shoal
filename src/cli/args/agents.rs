//! Arguments of commands that start agents, apps and chats.
use std::ffi::OsString;

use clap::Subcommand;

use crate::{
    agent::Agent,
    cli::args::{AgentParser, ModelArgs, item_reference},
};

#[derive(Debug, Subcommand)]
#[command(defer = true)]
pub enum AgentsCommand {
    /// Run Claude Code or open the Claude app.
    Claude {
        workspace: Option<String>,
        /// Run the Claude Code CLI, overriding agents.claude.default_mode.
        #[arg(long, conflicts_with = "app")]
        cli: bool,
        /// Open the workspace in the Claude app, overriding agents.claude.default_mode.
        #[arg(long, conflicts_with_all = ["cli", "model", "effort"])]
        app: bool,
        #[command(flatten)]
        model: ModelArgs,
        #[arg(last = true)]
        args: Vec<OsString>,
    },
    /// Run Codex CLI or open the Codex app.
    Codex {
        workspace: Option<String>,
        /// Run the Codex CLI, overriding agents.codex.default_mode.
        #[arg(long, conflicts_with = "app")]
        cli: bool,
        /// Open the workspace in the Codex app, overriding agents.codex.default_mode.
        #[arg(long, conflicts_with_all = ["cli", "model", "effort"])]
        app: bool,
        #[command(flatten)]
        model: ModelArgs,
        #[arg(last = true)]
        args: Vec<OsString>,
    },
    /// Open a workspace in the running T3 Code app.
    T3 {
        workspace: Option<String>,
        #[arg(last = true)]
        args: Vec<OsString>,
    },
    /// Start a new chat with an agent about a repository in its registered checkout.
    ///
    /// Inside Herdr the chat opens in a focused tab; on a terminal it runs in
    /// a zmx session. A Happy chat starts detached, to continue in the Happy
    /// app. Chats are not tracked executions.
    #[command(args_conflicts_with_subcommands = true)]
    Chat {
        #[command(subcommand)]
        command: Option<ChatCommand>,
        /// Run in the current pane instead of opening a Herdr tab.
        #[arg(long)]
        here: bool,
        /// Registered repository; defaults to the current checkout or workspace, else a picker.
        repository: Option<String>,
        /// Agent to start; defaults to agent.default, otherwise a picker.
        #[arg(long, value_parser = AgentParser)]
        agent: Option<Agent>,
        /// Name of the chat; defaults to <repo>-chat, asked for on a terminal.
        #[arg(long)]
        name: Option<String>,
        #[command(flatten)]
        model: ModelArgs,
        /// Arguments forwarded to the agent.
        #[arg(last = true)]
        args: Vec<OsString>,
    },
    /// Settle an issue's plan with an agent in a chat before anyone implements it.
    ///
    /// Without an issue, offers the open issues labelled [issue] plan_label.
    /// The chat opens like `shoal chat`, with the issue as its first task.
    Plan {
        /// Issue number or URL; defaults to a picker.
        #[arg(value_parser = item_reference)]
        issue: Option<String>,
        /// Plan the oldest open issue labelled [issue] plan_label.
        #[arg(long, conflicts_with_all = ["issue", "newest"])]
        oldest: bool,
        /// Plan the newest open issue labelled [issue] plan_label.
        #[arg(long, conflicts_with = "issue")]
        newest: bool,
        /// Registered repository of the issue; defaults to the URL's repository or the current checkout/workspace.
        #[arg(long = "repo")]
        repository: Option<String>,
        /// Run in the current pane instead of opening a Herdr tab.
        #[arg(long)]
        here: bool,
        /// Agent to start; defaults to agent.default, otherwise a picker.
        #[arg(long, value_parser = AgentParser)]
        agent: Option<Agent>,
        /// Name of the chat; defaults to <repo>-plan-<issue number>.
        #[arg(long)]
        name: Option<String>,
        #[command(flatten)]
        model: ModelArgs,
        /// Arguments forwarded to the agent.
        #[arg(last = true)]
        args: Vec<OsString>,
    },
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

#[derive(Debug, Subcommand)]
#[command(defer = true)]
pub enum ChatCommand {
    /// Rename a running chat, `shoal plan` chats included.
    ///
    /// Without a chat, renames the chat this runs in, else the current
    /// checkout's only running chat, else offers a picker. A chat's zmx
    /// session keeps its name; `shoal attach` finds it by the new one.
    #[command(allow_missing_positional = true)]
    Rename {
        /// Running chat, by name.
        chat: Option<String>,
        /// New name, free among workspaces and running chats.
        name: String,
    },
}
