//! Arguments of commands agents and the user coordinate with: messages, questions and completion.
use std::path::PathBuf;

use clap::{Args, Subcommand};

use crate::client::item_numbers::ItemNumber;

#[derive(Debug, Subcommand)]
#[command(defer = true)]
pub enum CoordinationCommand {
    /// Read new messages or send one to a space's agents.
    #[command(args_conflicts_with_subcommands = true)]
    Messages {
        #[command(subcommand)]
        command: Option<MessagesCommand>,
        /// Space, a workspace or chat, whose messages to read; defaults to the
        /// current workspace or chat, or a picker.
        space: Option<String>,
        /// Run as an agent hook: read the hook event on stdin, print new
        /// messages as additional context, and never fail the agent.
        #[arg(long, conflicts_with = "space")]
        hook: bool,
    },
    /// Review and answer questions from the agents of workspaces and chats.
    #[command(args_conflicts_with_subcommands = true)]
    Questions {
        #[command(subcommand)]
        command: Option<QuestionsCommand>,
        #[command(flatten)]
        args: QuestionsArgs,
    },
    /// List, add or delete the files attached to a space, a workspace or
    /// chat: sent to its agents with a message, added, or handed back with
    /// done.
    #[command(args_conflicts_with_subcommands = true)]
    Files {
        #[command(subcommand)]
        command: Option<FilesCommand>,
        /// Space, a workspace or chat; defaults to the current workspace or
        /// chat, or a picker.
        space: Option<String>,
        /// Print only the paths of Shoal's copies, one per line.
        #[arg(long)]
        path: bool,
        /// Print the list instead of opening the picker on a terminal.
        #[arg(short = 'p', long, conflicts_with = "path")]
        print: bool,
    },
    /// Mark the assignment finished; by default stop tracked commands and clean up safely.
    ///
    /// A chat records that its work is finished and keeps running.
    Done {
        /// Space, a workspace or chat, whose work is finished; defaults to
        /// the current workspace or chat, or a picker.
        space: Option<String>,
        /// Request cleanup even when [cleanup.done] enabled is false; preserve dirty or unpushed work.
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
        /// Hand back a file, such as a report, as PATH or PATH#TITLE; repeat
        /// for more. shoal files lists it, after cleanup too.
        #[arg(long, value_name = "FILE", conflicts_with = "undo")]
        attach: Vec<String>,
    },
}

#[derive(Debug, Subcommand)]
#[command(defer = true)]
pub enum FilesCommand {
    /// Attach copies of files to a space without a message; its agents can
    /// read them.
    Add {
        /// Files to attach, as PATH or PATH#TITLE.
        #[arg(required = true, value_name = "FILE")]
        files: Vec<String>,
        /// Space, a workspace or chat, to use; defaults to the current workspace
        /// or chat, or a picker.
        #[arg(short = 's', long)]
        space: Option<String>,
    },
    /// Delete the attached files of a space; asks first when an agent or
    /// command runs there.
    Rm {
        /// Delete only the files with these names; omit to delete them all.
        #[arg(value_name = "FILE")]
        files: Vec<String>,
        /// Space, a workspace or chat, to use; defaults to the current workspace
        /// or chat, or a picker.
        #[arg(short = 's', long)]
        space: Option<String>,
        /// Delete the files of every workspace without a running agent or
        /// command, and of every removed chat.
        #[arg(short = 'A', long, conflicts_with_all = ["space", "files"])]
        all: bool,
        #[command(flatten)]
        confirmation: crate::cli::args::ConfirmationArgs,
    },
}

#[derive(Debug, Default, Args)]
pub struct QuestionsArgs {
    /// Question number, such as 3.2; omit to choose an open question.
    pub number: Option<ItemNumber>,
    /// List questions from this space, a workspace or chat.
    #[arg(short = 's', long, conflicts_with = "number")]
    pub space: Option<String>,
    /// Send an answer or choice number.
    #[arg(long, requires = "number", conflicts_with = "withdraw")]
    pub answer: Option<String>,
    /// Withdraw a question no longer needed.
    #[arg(long, requires = "number")]
    pub withdraw: bool,
}

#[derive(Debug, Subcommand)]
#[command(defer = true)]
pub enum MessagesCommand {
    /// Queue a message for the agents working in a space.
    Send {
        /// Text such as "Stop the dev server before running tests".
        #[arg(required_unless_present_any = ["body_file", "attach"])]
        message: Option<String>,
        /// Read the message from a file, such as multi-line instructions.
        #[arg(long, value_name = "PATH", conflicts_with = "message")]
        body_file: Option<PathBuf>,
        /// Attach a file, such as a screenshot or log, as PATH or
        /// PATH#TITLE, which Shoal copies where the agents can read it;
        /// repeat for more.
        #[arg(long = "attach", value_name = "FILE")]
        attach: Vec<String>,
        /// Space, a workspace or chat, to send to; defaults to the current
        /// workspace or chat, or a picker.
        #[arg(short = 's', long, conflicts_with_all = ["all", "repository"])]
        space: Option<String>,
        /// Send to every running agent of the workspaces and chats, such as
        /// to tell them the network works again.
        #[arg(short = 'A', long, conflicts_with = "repository")]
        all: bool,
        /// Send to every running agent of this repository's workspaces and
        /// chats, such as to have them rebase onto a fix on its default
        /// branch; without a value, the current repository, else a picker.
        /// Put the message before a --repo without a value.
        #[arg(short = 'r', long = "repo", num_args = 0..=1, value_name = "REPOSITORY")]
        repository: Option<Option<String>>,
    },
}

// Arguments of `shoal questions ask` and the hidden `shoal ask`.
#[derive(Debug, Args)]
#[command(group(clap::ArgGroup::new("asking_space").args(["space", "handed_over"])))]
pub struct QuestionAskArgs {
    /// The decision, in one line, such as "Which database should the cache use?"
    #[arg(required_unless_present = "in_chat")]
    pub question: Option<String>,
    /// The answer you recommend.
    #[arg(long, required_unless_present = "in_chat")]
    pub recommend: Option<String>,
    /// Ask in your conversation instead: your turn's final message becomes
    /// the question, and a reply in the conversation closes it.
    #[arg(long, conflicts_with_all = ["question", "recommend", "reason", "choices", "attach", "wait", "resume"])]
    pub in_chat: bool,
    /// Why you recommend it.
    #[arg(long)]
    pub reason: Option<String>,
    /// A possible answer; repeat for each. The user may answer with its number.
    #[arg(long = "choice", value_name = "CHOICE")]
    pub choices: Vec<String>,
    /// Send a file with it, such as a screenshot of a choice, as PATH or
    /// PATH#TITLE; repeat for more. shoal files lists it, after cleanup too.
    #[arg(long = "attach", value_name = "FILE")]
    pub attach: Vec<String>,
    /// Wait for the answer and print it. Asking again waits for the same question.
    #[arg(long)]
    pub wait: bool,
    /// Space, a workspace or chat, whose agents ask; defaults to the current
    /// workspace or chat, or a picker.
    #[arg(short = 's', long)]
    pub space: Option<String>,
    /// The workspace the wait of an earlier release names when an upgrade
    /// hands it over to this one.
    #[arg(
        long = "workspace",
        hide = true,
        conflicts_with = "space",
        requires = "resume"
    )]
    pub handed_over: Option<String>,
    /// Continue waiting for the question with this number within the
    /// space, as the installed release does after an upgrade restarted the
    /// daemon.
    #[arg(long, hide = true, requires = "asking_space")]
    pub resume: Option<u32>,
}

#[derive(Debug, Subcommand)]
#[command(defer = true)]
pub enum QuestionsCommand {
    /// Ask the user to decide something; the answer arrives as a message.
    Ask(QuestionAskArgs),
    /// Withdraw an open question the space's agent no longer needs answered.
    Withdraw {
        /// Question number, such as 3.2.
        number: ItemNumber,
    },
}
