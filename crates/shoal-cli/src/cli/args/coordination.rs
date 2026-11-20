//! Arguments of commands agents and the user coordinate with: alerts, threads and completion.
use std::path::PathBuf;

use clap::{Args, Subcommand};

use shoal_protocol::space_names::SpaceSelector;

#[derive(Debug, Subcommand)]
#[command(defer = true)]
pub enum CoordinationCommand {
    /// Send an alert: up to your coordinator, or else the user; with
    /// --space down to a space's agent, or with --all or --repo to every
    /// agent below. From the user's terminal it picks the space. With a
    /// thread's number first, post in that thread instead. With --in-chat,
    /// your turn's final message is the text.
    Send {
        /// The text, such as "Stop the dev server before running tests",
        /// after a thread's number to post there, such as 3.2 or 2 for one
        /// of your own space. A single argument without --body-file, or an
        /// option only posts take, is an alert's text.
        #[arg(value_name = "[THREAD] TEXT", num_args = 0..=2)]
        args: Vec<String>,
        /// Read the text from a file, such as multi-line instructions.
        #[arg(long, value_name = "PATH")]
        body_file: Option<PathBuf>,
        /// Send your turn's final message once the turn ends instead: as a
        /// post in the thread named first, which a prompt in your
        /// conversation answers, or else as the alert. A prompt the user
        /// submits during the turn drops it.
        #[arg(long, conflicts_with_all = ["body_file", "code", "choices", "recommend", "choose", "wait", "resume"])]
        in_chat: bool,
        /// Attach a file, such as a screenshot or log, as PATH or
        /// PATH#TITLE, which Shoal copies where the recipient can read it;
        /// repeat for more.
        #[arg(long = "attach", value_name = "FILE")]
        attach: Vec<String>,
        /// Space, a workspace or chat, whose agent receives it.
        #[arg(short = 's', long, conflicts_with_all = ["all", "repository", "PostArgs"])]
        space: Option<SpaceSelector>,
        /// Send to every agent of the workspaces and chats your scope
        /// reaches, such as to tell them the network works again: running
        /// ones now, and stopped, queued or restoring ones when they next
        /// run.
        #[arg(short = 'A', long, conflicts_with_all = ["repository", "PostArgs"])]
        all: bool,
        /// Send to every agent of this repository's workspaces and chats,
        /// such as to have them rebase onto a fix on its default branch;
        /// without a value, the current repository, else a picker. Put the
        /// text before a --repo without a value.
        #[arg(
            short = 'r',
            long = "repo",
            num_args = 0..=1,
            value_name = "REPOSITORY",
            conflicts_with = "PostArgs"
        )]
        repository: Option<Option<String>>,
        #[command(flatten)]
        post: crate::cli::args::threads::PostArgs,
    },
    /// Open, forward, resolve or list threads: conversations between a
    /// space or the user and the space or user it asks.
    Thread {
        #[command(subcommand)]
        command: crate::cli::args::threads::ThreadCommand,
    },
    /// List, withdraw or clear alerts.
    Alert {
        #[command(subcommand)]
        command: AlertCommand,
    },
    /// The agent hook of an earlier release, which agents it started still
    /// run until they restart: `shoal internal deliver`.
    #[command(hide = true)]
    Messages {
        #[arg(long, required = true)]
        hook: bool,
    },
    /// The alert stream of an earlier release, which a follower it started
    /// continues with after an upgrade: `shoal alert list --follow`.
    #[command(hide = true)]
    Alerts {
        #[arg(long, required = true)]
        follow: bool,
        #[arg(long)]
        keep_unread: bool,
        #[arg(long)]
        resume: Option<i64>,
        /// Kept from the listing the follower started as, which a follow
        /// does not read.
        #[arg(long)]
        limit: Option<u32>,
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
        space: Option<SpaceSelector>,
        /// Print only the paths of Shoal's copies, one per line.
        #[arg(long)]
        path: bool,
        /// Print the list instead of opening the picker on a terminal.
        #[arg(short = 'p', long, conflicts_with = "path")]
        print: bool,
    },
    /// Show or set where a space's agent is in its work: a status line and
    /// the steps of its plan.
    ///
    /// Claude Code's and Codex's plan tools set the steps through Shoal's
    /// hooks; other agents set them with --steps.
    Progress {
        /// The status line, such as "Reproduced; the cause is the query
        /// limit"; "" clears it. Omit it and --steps to show the progress.
        status: Option<String>,
        /// Replace the steps with a markdown task list read from stdin, one
        /// `- [ ] name` or `- [x] name` per line; empty input clears them.
        #[arg(long)]
        steps: bool,
        /// Record what blocks the space, or with --step one step: an issue
        /// number or URL, a PR or CI run URL, or a space's ID.
        #[arg(
            long,
            value_name = "BLOCKER",
            requires = "reason",
            conflicts_with_all = ["status", "steps", "remove_blocker"]
        )]
        add_blocker: Option<String>,
        /// Why --add-blocker blocks, in one line.
        #[arg(long, requires = "add_blocker")]
        reason: Option<String>,
        /// Remove a blocker of the space, or with --step of one step.
        #[arg(long, value_name = "BLOCKER", conflicts_with_all = ["status", "steps"])]
        remove_blocker: Option<String>,
        /// The step a blocker blocks, numbered as shoal progress shows them.
        #[arg(long, value_name = "NUMBER")]
        step: Option<usize>,
        /// Space, a workspace or chat; defaults to the current workspace or
        /// chat, or a picker.
        #[arg(short = 's', long)]
        space: Option<SpaceSelector>,
    },
    /// Put a space's agent to sleep from the end of its turn until its next
    /// turn starts; --for and --until also set a timer that wakes it.
    ///
    /// Shoal does not wake a sleeping agent for having stayed idle; it stops
    /// an idle agent once the user is away from it, saving it to come back
    /// on its own. Updates, messages, the user and its timer still wake it.
    Sleep {
        /// Space, a workspace or chat; defaults to the current workspace or
        /// chat, or a picker.
        space: Option<SpaceSelector>,
        /// Put the agent back to sleep after every turn until --undo.
        #[arg(long, conflicts_with = "undo")]
        permanent: bool,
        /// End either kind of sleep, cancel the timer and wake the agent if
        /// it stopped.
        #[arg(long)]
        undo: bool,
        #[command(flatten)]
        timer: SleepTimerArgs,
        /// What the agent receives when its timer fires, such as "Check
        /// whether CI finished"; without it, only that the timer fired.
        #[arg(last = true, value_name = "MESSAGE", requires = "timer")]
        message: Vec<String>,
    },
    /// Set a space's priority, which ranks its queued work ahead of
    /// everything else.
    ///
    /// Work that the space waits on takes it too. A space's agent gives
    /// at most its own space's priority.
    Priority {
        /// low (25), normal (50, the default), urgent (75), or a number
        /// from 0 to 100; higher goes first.
        priority: shoal_schedule::priority::Priority,
        /// Space, a workspace or chat; defaults to the current workspace or
        /// chat, or a picker.
        space: Option<SpaceSelector>,
    },
    /// Mark the assignment finished; by default stop tracked commands and clean up safely.
    ///
    /// A chat records that its work is finished and keeps running.
    Done {
        /// Space, a workspace or chat, whose work is finished; defaults to
        /// the current workspace or chat, or a picker.
        space: Option<SpaceSelector>,
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
        /// State what the work found in one line of at most 512 bytes; the
        /// completion notice ends with it. Put longer findings in a file
        /// handed back with --attach.
        #[arg(long, value_name = "TEXT", conflicts_with = "undo")]
        summary: Option<String>,
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
        space: Option<SpaceSelector>,
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
        space: Option<SpaceSelector>,
        /// Delete the files of every workspace without a running agent or
        /// command, and of every removed space.
        #[arg(short = 'A', long, conflicts_with_all = ["space", "files"])]
        all: bool,
        #[command(flatten)]
        confirmation: crate::cli::args::ConfirmationArgs,
    },
}

#[derive(Debug, Subcommand)]
#[command(defer = true)]
pub enum AlertCommand {
    /// Show alerts for you: in a picker on a terminal, else as a list.
    /// Showing them marks them read.
    List(AlertListArgs),
    /// Withdraw an alert you sent before its recipient reads it.
    Withdraw {
        /// Alert number, as shoal send printed it.
        id: i64,
        /// Space, a workspace or chat, the alert went to; finds it on the
        /// machine that has that space.
        #[arg(short = 's', long)]
        space: Option<SpaceSelector>,
    },
    /// Delete the user's alerts, including unread ones.
    Clear,
}

// Arguments of `shoal alert list`; a doc comment would replace the
// command's help.
#[derive(Debug, Args)]
pub struct AlertListArgs {
    /// Space, a workspace or chat, whose agent's alerts to show; defaults
    /// to your own space, or the user's alerts from the user's terminal.
    #[arg(short = 's', long, conflicts_with = "all")]
    pub space: Option<SpaceSelector>,
    /// Show the alerts of every recipient you may read: the user and each
    /// space your scope reaches.
    #[arg(short = 'A', long)]
    pub all: bool,
    /// Show only unread alerts, oldest first.
    #[arg(short = 'u', long)]
    pub unread: bool,
    /// Keep printing the user's new alerts as Shoal records them.
    #[arg(long, conflicts_with_all = ["space", "all", "print"])]
    pub follow: bool,
    /// Show alerts without marking them read.
    #[arg(long)]
    pub keep_unread: bool,
    /// Print the list instead of opening the picker on a terminal.
    #[arg(short = 'p', long)]
    pub print: bool,
    /// Continue following after this alert, as the installed release does
    /// after an upgrade restarted the daemon.
    #[arg(long, hide = true, requires = "follow")]
    pub resume: Option<i64>,
    #[arg(long, default_value_t = 50, value_parser = clap::value_parser!(u32).range(1..=200))]
    pub limit: u32,
}

// Arguments of `shoal sleep` that set the agent's timer, replacing the one
// it had; a doc comment would replace the command's help.
#[derive(Debug, Args)]
#[group(id = "timer", multiple = false, conflicts_with = "undo")]
pub struct SleepTimerArgs {
    /// Wake the agent after this long, such as 20m or 1h30m.
    #[arg(long = "for", value_name = "DURATION")]
    pub after: Option<crate::config::span::Span>,
    /// Wake the agent when the local clock next shows this time, HH:MM.
    #[arg(long, value_name = "HH:MM")]
    pub until: Option<shoal_core::triggers::timer::ClockTime>,
}
