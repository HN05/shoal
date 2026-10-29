//! Arguments of commands agents and the user coordinate with: messages, questions and completion.
use std::path::PathBuf;

use clap::{Args, Subcommand};

#[derive(Debug, Subcommand)]
#[command(defer = true)]
pub enum CoordinationCommand {
    /// Read new messages or send one to a workspace's agents.
    #[command(args_conflicts_with_subcommands = true)]
    Messages {
        #[command(subcommand)]
        command: Option<MessagesCommand>,
        workspace: Option<String>,
        /// Run as an agent hook: read the hook event on stdin, print new
        /// messages as additional context, and never fail the agent.
        #[arg(long, conflicts_with = "workspace")]
        hook: bool,
    },
    /// Review and answer questions from workspace agents.
    #[command(args_conflicts_with_subcommands = true)]
    Questions {
        #[command(subcommand)]
        command: Option<QuestionsCommand>,
        #[command(flatten)]
        args: QuestionsArgs,
    },
    /// List, add or delete the files attached to a workspace: sent to its
    /// agents with a message, added, or handed back with done.
    #[command(args_conflicts_with_subcommands = true)]
    Files {
        #[command(subcommand)]
        command: Option<FilesCommand>,
        workspace: Option<String>,
        /// Print only the paths of Shoal's copies, one per line.
        #[arg(long)]
        path: bool,
        /// Print the list instead of opening the picker on a terminal.
        #[arg(long, conflicts_with = "path")]
        print: bool,
    },
    /// Mark the assignment finished; by default stop tracked commands and clean up safely.
    Done {
        workspace: Option<String>,
        /// Request cleanup even when [done] cleanup is false; preserve dirty or unpushed work.
        #[arg(long, conflicts_with = "undo")]
        cleanup: bool,
        /// Another branch has this work under new commits, such as after a
        /// cherry-pick or an edited rebase; unpushed commits no longer keep
        /// the workspace.
        #[arg(long, conflicts_with = "undo")]
        integrated: bool,
        /// Withdraw a recorded completion without keeping the workspace.
        #[arg(long)]
        undo: bool,
        /// Leave the workspace's demos running until cleanup removes it.
        #[arg(long, conflicts_with = "undo")]
        keep_demos: bool,
        /// Hand back a file, such as a report; repeat for more. shoal files
        /// lists it, after cleanup too.
        #[arg(long, value_name = "FILE", conflicts_with = "undo")]
        attach: Vec<PathBuf>,
    },
}

#[derive(Debug, Subcommand)]
#[command(defer = true)]
pub enum FilesCommand {
    /// Attach copies of files to a workspace without a message; its agents
    /// can read them.
    Add {
        /// Files to attach.
        #[arg(required = true, value_name = "FILE")]
        files: Vec<PathBuf>,
        /// Workspace to use; defaults to the current workspace or picker.
        #[arg(long)]
        workspace: Option<String>,
    },
    /// Delete a workspace's attached files; asks first when an agent or
    /// command runs there.
    Rm {
        /// Delete only the files with these names; omit to delete them all.
        #[arg(value_name = "FILE")]
        files: Vec<String>,
        /// Workspace to use; defaults to the current workspace or picker.
        #[arg(long)]
        workspace: Option<String>,
        /// Delete the files of every workspace without a running agent or command.
        #[arg(long, conflicts_with_all = ["workspace", "files"])]
        all: bool,
        #[command(flatten)]
        confirmation: crate::cli::args::ConfirmationArgs,
    },
}

#[derive(Debug, Default, Args)]
pub struct QuestionsArgs {
    /// Question number; omit to choose an open question.
    pub id: Option<i64>,
    /// List questions from this workspace.
    #[arg(long, conflicts_with = "id")]
    pub workspace: Option<String>,
    /// Send an answer or choice number.
    #[arg(long, requires = "id", conflicts_with = "withdraw")]
    pub answer: Option<String>,
    /// Withdraw a question no longer needed.
    #[arg(long, requires = "id")]
    pub withdraw: bool,
}

#[derive(Debug, Subcommand)]
#[command(defer = true)]
pub enum MessagesCommand {
    /// Queue a message for the agents working in a workspace.
    Send {
        /// Text such as "Stop the dev server before running tests".
        #[arg(required_unless_present_any = ["body_file", "attach"])]
        message: Option<String>,
        /// Read the message from a file, such as multi-line instructions.
        #[arg(long, value_name = "PATH", conflicts_with = "message")]
        body_file: Option<PathBuf>,
        /// Attach a file, such as a screenshot or log, which Shoal copies
        /// where the agents can read it; repeat for more.
        #[arg(long = "attach", value_name = "FILE")]
        attach: Vec<PathBuf>,
        /// Workspace to use; defaults to the current workspace or picker.
        #[arg(long)]
        workspace: Option<String>,
    },
}

// Arguments of `shoal questions ask` and the hidden `shoal ask`.
#[derive(Debug, Args)]
pub struct QuestionAskArgs {
    /// The decision, in one line, such as "Which database should the cache use?"
    pub question: String,
    /// The answer you recommend.
    #[arg(long)]
    pub recommend: String,
    /// Why you recommend it.
    #[arg(long)]
    pub reason: Option<String>,
    /// A possible answer; repeat for each. The user may answer with its number.
    #[arg(long = "choice", value_name = "CHOICE")]
    pub choices: Vec<String>,
    /// Wait for the answer and print it. Asking again waits for the same question.
    #[arg(long)]
    pub wait: bool,
    /// Workspace to use; defaults to the current workspace or picker.
    #[arg(long)]
    pub workspace: Option<String>,
    /// Continue waiting for this question, as the installed release does
    /// after an upgrade restarted the daemon.
    #[arg(long, hide = true, requires = "workspace")]
    pub resume: Option<i64>,
}

#[derive(Debug, Subcommand)]
#[command(defer = true)]
pub enum QuestionsCommand {
    /// Ask the user to decide something; the answer arrives as a message.
    Ask(QuestionAskArgs),
    /// Withdraw an open question the workspace's agent no longer needs answered.
    Withdraw {
        /// Question number.
        id: i64,
        /// Workspace to use; defaults to the current workspace or picker.
        #[arg(long)]
        workspace: Option<String>,
    },
}
