//! Arguments of commands for a workspace's changes, its PRs and issues, and landing it.
use std::{ffi::OsString, path::PathBuf};

use clap::{Args, Subcommand};

use crate::{
    agent::Agent,
    cli::args::{AgentParser, BodyArgs, ConfirmationArgs, item_reference},
};

#[derive(Debug, Subcommand)]
#[command(defer = true)]
pub enum ChangesCommand {
    /// Show changes since the branch's fork point.
    Diff { workspace: Option<String> },
    /// Check whether the branch merges cleanly into another branch.
    ///
    /// Compares committed HEAD with the target: by default the base workspace's
    /// branch, otherwise the default branch. Exits 1 on conflicts.
    Conflicts {
        /// Branch or commit to merge into.
        target: Option<String>,
        /// Workspace to check; defaults to the current workspace or picker.
        #[arg(long)]
        workspace: Option<String>,
    },
    /// Review workspace or PR changes, or refine an issue, with a review tool or agent.
    Review {
        /// Workspace to review, or a PR or issue URL.
        workspace: Option<String>,
        /// Review this PR in the workspace that owns its head branch, opening one when none does.
        #[arg(long, value_name = "NUMBER_OR_URL", conflicts_with = "workspace", value_parser = item_reference)]
        pr: Option<String>,
        /// Have an agent refine this issue before implementation, in the workspace that works on it.
        #[arg(long, value_name = "NUMBER_OR_URL", conflicts_with_all = ["workspace", "pr", "manual"], value_parser = item_reference)]
        issue: Option<String>,
        /// Registered repository of the PR or issue; defaults to the URL's repository or the current checkout/workspace.
        #[arg(long = "repo")]
        repository: Option<String>,
        /// Have the agent post its findings on the PR or issue, overriding [review] post.
        #[arg(long, overrides_with = "no_post", conflicts_with = "manual")]
        post: bool,
        /// Keep the agent's findings local, overriding [review] post.
        #[arg(long, overrides_with = "post", conflicts_with = "manual")]
        no_post: bool,
        /// Run the configured `review` command without asking.
        #[arg(long, conflicts_with = "agent")]
        manual: bool,
        /// Start this agent with a review prompt without asking.
        #[arg(long, value_parser = AgentParser)]
        agent: Option<Agent>,
        /// Arguments forwarded to the review command or agent.
        #[arg(last = true)]
        args: Vec<OsString>,
    },
    /// Open the workspace's PR, or change, comment on, close, reopen or merge a PR.
    Pr {
        #[command(subcommand)]
        command: PrCommand,
    },
    /// Open, change, comment on, close or reopen an issue of the workspace's repository.
    Issue {
        #[command(subcommand)]
        command: IssueCommand,
    },
    /// Merge this workspace into the default branch, then optionally push it.
    Land {
        workspace: Option<String>,
        /// Push the default branch to its upstream after landing, overriding [land] push.
        #[arg(long, overrides_with = "no_push")]
        push: bool,
        /// Keep the landing local, overriding [land] push.
        #[arg(long, overrides_with = "push")]
        no_push: bool,
    },
}

#[derive(Debug, Subcommand)]
#[command(defer = true)]
pub enum PrCommand {
    /// Rerun failed or cancel running CI jobs of a PR.
    Ci {
        #[command(subcommand)]
        command: CiCommand,
    },
    /// List or search the workspace repository's PRs, newest first.
    List(ListArgs),
    /// Push the workspace branch, open a PR for it or find its open PR, and link it.
    Open {
        /// Workspace to use; defaults to the current workspace or picker.
        #[arg(long)]
        workspace: Option<String>,
        #[command(flatten)]
        pull: OpenPullArgs,
    },
    /// Change a PR's title, description, base, labels, reviewers or draft state.
    Edit {
        #[command(flatten)]
        item: ItemSelector,
        #[command(flatten)]
        edit: PrEditArgs,
    },
    /// List the users and teams who can review a PR, named as --add-reviewer takes them.
    Reviewers {
        #[command(flatten)]
        item: ItemSelector,
    },
    /// Comment on a PR.
    Comment {
        #[command(flatten)]
        item: ItemSelector,
        #[command(flatten)]
        body: BodyArgs,
        #[command(flatten)]
        files: AttachArgs,
    },
    /// Review a PR: a body, comments on lines of its diff and a verdict.
    ///
    /// Workspace processes can only comment; approving and requesting
    /// changes use your own login.
    Review {
        #[command(flatten)]
        item: ItemSelector,
        #[command(flatten)]
        review: ReviewArgs,
    },
    /// Reply in the thread of a review comment.
    Reply {
        /// ID of any comment in the thread, as `shoal view` shows it.
        comment: u64,
        #[command(flatten)]
        item: ItemSelector,
        #[command(flatten)]
        body: BodyArgs,
    },
    /// Resolve the thread of a review comment, after replying with the body given.
    Resolve {
        /// ID of any comment in the thread, as `shoal view` shows it.
        comment: u64,
        #[command(flatten)]
        item: ItemSelector,
        #[command(flatten)]
        body: BodyArgs,
        #[command(flatten)]
        confirmation: ConfirmationArgs,
    },
    /// Unresolve the thread of a review comment.
    Unresolve {
        /// ID of any comment in the thread, as `shoal view` shows it.
        comment: u64,
        #[command(flatten)]
        item: ItemSelector,
    },
    /// Close a PR without merging it.
    Close {
        #[command(flatten)]
        item: ItemSelector,
    },
    /// Reopen a closed PR.
    Reopen {
        #[command(flatten)]
        item: ItemSelector,
    },
    /// Merge a PR with your own login; workspace processes cannot merge.
    Merge {
        #[command(flatten)]
        item: ItemSelector,
        /// How to merge.
        #[arg(long, value_enum)]
        method: crate::forge::items::action::MergeMethod,
        /// Delete the PR's branch on the forge after merging.
        #[arg(long)]
        delete_branch: bool,
    },
}

#[derive(Debug, Subcommand)]
#[command(defer = true)]
pub enum CiCommand {
    /// Rerun failed jobs on the PR's current head.
    Rerun {
        #[command(flatten)]
        checks: CiChecks,
    },
    /// Cancel running jobs on the PR's current head.
    Cancel {
        #[command(flatten)]
        checks: CiChecks,
    },
}

// A PR and the checks on its head to act on.
#[derive(Debug, Args)]
pub struct CiChecks {
    #[command(flatten)]
    pub item: ItemSelector,
    /// Act only on this check, as `shoal view` names it; repeat for more. Defaults to every failed or running check.
    #[arg(long = "check", value_name = "NAME")]
    pub checks: Vec<String>,
}

#[derive(Debug, Subcommand)]
#[command(defer = true)]
pub enum IssueCommand {
    /// List or search the workspace repository's issues, newest first.
    List(ListArgs),
    /// Open an issue in the workspace's repository or another one.
    Open {
        /// Workspace to use; defaults to the current workspace or picker.
        #[arg(long)]
        workspace: Option<String>,
        /// Open it in another repository: OWNER/NAME on the workspace's
        /// forge, or a repository URL on any forge.
        #[arg(long, value_name = "REPOSITORY")]
        repo: Option<String>,
        /// Issue title.
        #[arg(long)]
        title: String,
        #[command(flatten)]
        body: BodyArgs,
        #[command(flatten)]
        files: AttachArgs,
        /// Add a label; repeat for more.
        #[arg(long = "label", value_name = "LABEL")]
        labels: Vec<String>,
        /// Link the new issue to the workspace.
        #[arg(long)]
        link: bool,
    },
    /// Change an issue's title, description or labels.
    Edit {
        #[command(flatten)]
        item: ItemSelector,
        #[command(flatten)]
        edit: EditArgs,
    },
    /// Comment on an issue.
    Comment {
        #[command(flatten)]
        item: ItemSelector,
        #[command(flatten)]
        body: BodyArgs,
        #[command(flatten)]
        files: AttachArgs,
    },
    /// Close an issue.
    Close {
        #[command(flatten)]
        item: ItemSelector,
    },
    /// Reopen a closed issue.
    Reopen {
        #[command(flatten)]
        item: ItemSelector,
    },
}

// Which issues or PRs of the workspace's repository to list.
#[derive(Debug, Args)]
pub struct ListArgs {
    /// Text the forge searches for in titles and descriptions.
    #[arg(value_name = "TEXT")]
    pub query: Vec<String>,
    /// Workspace to use; defaults to the current workspace or picker.
    #[arg(long)]
    pub workspace: Option<String>,
    /// Which items to list; closed includes merged PRs.
    #[arg(long, value_enum, default_value_t = crate::forge::items::list::StateFilter::Open)]
    pub state: crate::forge::items::list::StateFilter,
    /// List only items with this label; repeat for items with every one.
    #[arg(long = "label", value_name = "LABEL")]
    pub labels: Vec<String>,
    /// List only items this login opened.
    #[arg(long, value_name = "LOGIN")]
    pub author: Option<String>,
    /// List at most this many items.
    #[arg(long, default_value_t = 30, value_parser = clap::value_parser!(u16).range(1..=1000))]
    pub limit: u16,
}

// An issue or PR of the workspace's repository.
#[derive(Debug, Args)]
pub struct ItemSelector {
    /// Number or URL; defaults to the linked one.
    #[arg(value_parser = item_reference)]
    pub item: Option<String>,
    /// Workspace to use; defaults to the current workspace or picker.
    #[arg(long)]
    pub workspace: Option<String>,
}

// What a new PR is opened with.
#[derive(Debug, Args)]
pub struct OpenPullArgs {
    /// PR title; defaults to the linked issue's title, then the last commit's subject.
    #[arg(long)]
    pub title: Option<String>,
    #[command(flatten)]
    pub body: BodyArgs,
    #[command(flatten)]
    pub files: AttachArgs,
    /// Base branch; defaults to the base workspace's branch, then the default branch.
    #[arg(long)]
    pub base: Option<String>,
    /// Open the PR as a draft, overriding [pr] draft.
    #[arg(long, overrides_with = "no_draft")]
    pub draft: bool,
    /// Open the PR ready for review, overriding [pr] draft.
    #[arg(long, overrides_with = "draft")]
    pub no_draft: bool,
    /// Add a label; repeat for more.
    #[arg(long = "label", value_name = "LABEL")]
    pub labels: Vec<String>,
    /// Request a review; repeat for more.
    #[arg(long = "reviewer", value_name = "LOGIN")]
    pub reviewers: Vec<String>,
}

// The changes a PR takes besides those of any item.
#[derive(Debug, Args)]
pub struct PrEditArgs {
    #[command(flatten)]
    pub edit: EditArgs,
    /// New base branch.
    #[arg(long)]
    pub base: Option<String>,
    /// Request a review; repeat for more.
    #[arg(long = "add-reviewer", value_name = "LOGIN")]
    pub add_reviewers: Vec<String>,
    /// Withdraw a review request; repeat for more.
    #[arg(long = "remove-reviewer", value_name = "LOGIN")]
    pub remove_reviewers: Vec<String>,
    /// Mark the PR as a draft.
    #[arg(long, conflicts_with = "no_draft")]
    pub draft: bool,
    /// Mark the PR ready for review.
    #[arg(long)]
    pub no_draft: bool,
}

// A review's verdict, body and comments on lines of the PR's diff.
#[derive(Debug, Args)]
pub struct ReviewArgs {
    #[command(flatten)]
    pub verdict: VerdictArgs,
    #[command(flatten)]
    pub body: BodyArgs,
    /// Comment on a line of the PR's version of a file; repeat for more.
    #[arg(long = "line", num_args = 2, value_names = ["PATH:LINE", "BODY"])]
    pub lines: Vec<String>,
    /// Comment on a removed line, numbered in the base's version; repeat for more.
    #[arg(long = "old-line", num_args = 2, value_names = ["PATH:LINE", "BODY"])]
    pub old_lines: Vec<String>,
    /// Read more line comments from a JSON array; `-` reads standard input.
    #[arg(long, value_name = "PATH")]
    pub comments_file: Option<PathBuf>,
}

// The verdict of a review; exactly one.
#[derive(Debug, Args)]
#[group(required = true, multiple = false)]
pub struct VerdictArgs {
    /// Comment without approving or requesting changes.
    #[arg(long)]
    pub comment: bool,
    /// Approve the PR.
    #[arg(long)]
    pub approve: bool,
    /// Request changes; needs a body.
    #[arg(long)]
    pub request_changes: bool,
}

// Title, description and label changes either kind of item takes.
#[derive(Debug, Args)]
pub struct EditArgs {
    /// New title.
    #[arg(long)]
    pub title: Option<String>,
    #[command(flatten)]
    pub body: BodyArgs,
    #[command(flatten)]
    pub files: AttachArgs,
    /// Add a label; repeat for more.
    #[arg(long = "add-label", value_name = "LABEL")]
    pub add_labels: Vec<String>,
    /// Remove a label; repeat for more.
    #[arg(long = "remove-label", value_name = "LABEL")]
    pub remove_labels: Vec<String>,
}

// Files to show or link in a description or comment.
#[derive(Debug, Args)]
pub struct AttachArgs {
    /// Upload a file, as PATH or PATH#ALT_TEXT; repeat for more. Images and
    /// videos are shown, other files linked. It replaces the body's image
    /// references to the file, such as `![alt](shot.png)` relative to the body
    /// file or working directory, and otherwise follows the body.
    #[arg(long = "attach", value_name = "FILE")]
    pub attach: Vec<String>,
}
