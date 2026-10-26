use std::path::PathBuf;

use clap::{Args, Subcommand};

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
        #[arg(required_unless_present = "body_file")]
        message: Option<String>,
        /// Read the message from a file, such as multi-line instructions.
        #[arg(long, value_name = "PATH", conflicts_with = "message")]
        body_file: Option<PathBuf>,
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
