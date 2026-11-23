//! Arguments of commands for agent sessions, demos and triggers.
use shoal_ids::execution_ids::ExecutionId;
use std::{ffi::OsString, path::PathBuf};

use clap::{Args, Subcommand};

use crate::{
    agent::Agent,
    cli::args::{AgentParser, SpaceTargets, permission_assignment},
    sessions::HostName,
};
use shoal_ids::checkpoint_numbers::CheckpointNumber;
use shoal_protocol::space_names::SpaceSelector;

#[derive(Debug, Subcommand)]
#[command(defer = true)]
pub enum SessionsCommand {
    /// Bring back the agents of spaces, workspaces and chats alike.
    ///
    /// Queues the restore of a stopped agent, which the daemon starts as `max_heavy_agents`
    /// allows: where the agent stopped, otherwise in a background place of the terminal host it
    /// was queued from or of `terminal.default_host`, such as a Herdr tab or a detached zmx
    /// session. An asleep agent's restore waits only for its turn in the queue, a turn
    /// `shoal stop --interrupt` ended continues, the last session of an agent that exited on its
    /// own continues, and a launch that failed to start queues again as it was. Stopped commands
    /// are listed.
    ///
    /// Without a space, resumes the current one when its agent can resume; otherwise picks among
    /// the spaces whose agent can resume, or lists them without a terminal. `--all` resumes every
    /// space with a stopped agent whose assignment is not done.
    Resume {
        #[command(flatten)]
        targets: SpaceTargets,
        /// Select a stopped execution when the space has several records.
        #[arg(long, conflicts_with = "all")]
        execution: Option<ExecutionId>,
        /// Start at once regardless of the agent limits, where each agent
        /// stopped or in the background.
        #[arg(long)]
        now: bool,
        /// Resume one agent in this terminal at once, regardless of the
        /// agent limits.
        #[arg(short = 'f', long, conflicts_with = "all")]
        focus: bool,
        /// Set a permission of the resumed agents, as KEY=VALUE such as
        /// push=direct, whatever their mode allows; KEY=mode drops the value
        /// given before, and keys left out keep theirs. Repeatable. A
        /// space's agent gives only what it holds itself.
        #[arg(
            long = "permission",
            value_name = "KEY=VALUE",
            value_parser = permission_assignment,
            conflicts_with_all = ["all", "queue_entry"]
        )]
        permissions: Vec<crate::config::permissions::PermissionChange>,
        /// The queue entry whose slot the resumed agent takes, for the
        /// daemon's start of a queued restore.
        #[arg(long, hide = true, requires_all = ["execution", "focus"])]
        queue_entry: Option<shoal_ids::queue_entry_ids::QueueEntryId>,
        /// The host that opened this place in the background for this
        /// resume, which records it as the space's before resuming.
        #[arg(long, hide = true, requires_all = ["execution", "focus"])]
        opened_in: Option<HostName>,
        /// The host whose place, opened in the background for this resume,
        /// closes once the space is done.
        #[arg(long, hide = true, requires_all = ["execution", "focus"])]
        close_in: Option<HostName>,
    },
    /// Reopen removed spaces, return a workspace to a checkpoint, or discard a space's agent.
    ///
    /// A removed workspace or chat, named by its ID, opens again with its unsaved files and
    /// links, and its agents resume as `shoal resume` resumes them. A space that is not removed
    /// is refused, naming `shoal resume`. Without a space, picks among the removed spaces, or
    /// lists them without a terminal.
    ///
    /// `--from` continues the workspaces and chats shoal move wrote to a file, in the repository
    /// with the same origin, cloned from it when none is registered: each space is recorded as removed and opens again under its ID, a
    /// workspace fetching the default branch when the file builds on commits the repository
    /// lacks, a chat in the worktree it ran in when that workspace is here, else in the
    /// repository's checkout, or in root_dir without a repository, and their agents resume. The
    /// spaces a file carries open after it, parents first, each below its parent. The agents'
    /// session files go where the agents look for them; a session the file lacks continues only
    /// on the machine that wrote it, which --keep-sessions and --new-sessions override. `-`
    /// reads standard input, and a directory each .shoal file in it.
    Restore {
        #[arg(value_name = "SPACE")]
        space: Vec<SpaceSelector>,
        /// Discard the agents of every space with one, with --discard-agent.
        #[arg(
            short = 'A',
            long,
            conflicts_with = "space",
            requires = "discard_agent"
        )]
        all: bool,
        #[command(flatten)]
        import: ImportArgs,
        /// Return the workspace to this checkpoint after keeping its current state in a new
        /// one; without a number, pick one of its checkpoints or list them.
        #[arg(
            long,
            value_name = "N",
            num_args = 0..=1,
            conflicts_with_all = ["discard_agent", "no_resume", "now", "focus", "permissions"]
        )]
        checkpoint: Option<Option<CheckpointNumber>>,
        /// Discard the stopped agent, last sessions, stopped commands and failed launch of the
        /// space instead, which leaves its files, branch and worktree as they are.
        #[arg(long, conflicts_with_all = ["no_resume", "now", "focus", "permissions"])]
        discard_agent: bool,
        /// Select a stopped execution to discard when the space has several records.
        #[arg(long, requires = "discard_agent", conflicts_with = "all")]
        execution: Option<ExecutionId>,
        /// Reopen the spaces, or those --from reads, leaving their agents saved for a later shoal
        /// resume.
        #[arg(long, conflicts_with_all = ["now", "focus", "permissions"])]
        no_resume: bool,
        /// Start the agents at once regardless of the agent limits, where
        /// each agent stopped or in the background.
        #[arg(long)]
        now: bool,
        /// Resume the one agent in this terminal at once, regardless of the
        /// agent limits.
        #[arg(short = 'f', long)]
        focus: bool,
        /// Set a permission of the resumed agents, as shoal resume does.
        #[arg(
            long = "permission",
            value_name = "KEY=VALUE",
            value_parser = permission_assignment
        )]
        permissions: Vec<crate::config::permissions::PermissionChange>,
    },
    /// Restart running agents in new terminals, resuming their sessions.
    ///
    /// Agents restart together and run the agent's current binary; commands
    /// keep running. A chat restarts in a background place, as shoal resume
    /// resumes it. A newer restart of a space supersedes the earlier one,
    /// keeping the permissions that one set unless it sets them again.
    Restart {
        #[command(flatten)]
        targets: SpaceTargets,
        /// Every space of this joined machine instead, by its name or the ID
        /// shoal hub ls shows. Repeatable.
        #[arg(long = "machine", value_name = "MACHINE", conflicts_with_all = ["space", "all"])]
        machines: Vec<String>,
        /// Restart only agents of this name, such as claude.
        #[arg(short = 'a', long, value_parser = AgentParser)]
        agent: Option<Agent>,
        /// Restart each agent once its turn ends. The daemon keeps the
        /// restart, so the command returns at once.
        #[arg(long)]
        wait: bool,
        /// Restart one agent at a time, in turn with the other sequential
        /// restarts, 10 seconds apart.
        #[arg(long)]
        sequential: bool,
        /// Restart each agent in the terminal it runs in.
        #[arg(long)]
        in_place: bool,
        /// Set a permission of the restarted agents, as KEY=VALUE such as
        /// push=direct, whatever their mode allows; KEY=mode drops the value
        /// given before, and keys left out keep theirs. Repeatable. A
        /// space's agent gives only what it holds itself.
        #[arg(
            long = "permission",
            value_name = "KEY=VALUE",
            value_parser = permission_assignment
        )]
        permissions: Vec<crate::config::permissions::PermissionChange>,
        /// Drop the restarts the daemon keeps for later, with their
        /// permissions, instead of restarting.
        #[arg(
            long,
            conflicts_with_all = ["agent", "wait", "sequential", "in_place", "permissions"]
        )]
        cancel: bool,
    },
    /// Stop all work on this machine and send it to other machines, admitting nothing new.
    ///
    /// The pause lasts through daemon restarts until shoal unpause. Every agent stops as shoal
    /// stop stops it, saving its session, and the spaces with something to do, an agent working
    /// or waiting to be restored, go to other connected machines of the hub, a chat in a
    /// workspace's worktree with that workspace; the CI jobs this machine runs end to run again
    /// elsewhere, reviews wait in the queue, and demos stop. An alert names where each space went,
    /// or why it stays.
    Pause {
        /// Pause this joined machine instead, by its name or the ID shoal hub ls shows.
        #[arg(long, value_name = "MACHINE")]
        machine: Option<String>,
    },
    /// Admit work on this machine again after shoal pause.
    ///
    /// The agent queue restores the agents the pause stopped here, and the machine retires its
    /// copies of the spaces that went to other machines.
    Unpause {
        /// Unpause this joined machine instead, by its name or the ID shoal hub ls shows.
        #[arg(long, value_name = "MACHINE")]
        machine: Option<String>,
    },
    /// List the agent queue, or start or cancel an entry.
    ///
    /// The queue holds the agent launches and restores of every repository
    /// waiting for a heavy slot under max_heavy_agents, named by workspace. Without a
    /// command on a terminal, choose an entry, then an action.
    #[command(args_conflicts_with_subcommands = true)]
    Queue {
        #[command(subcommand)]
        command: Option<QueueCommand>,
        /// List only this registered repository's entries.
        #[arg(short = 'r', long = "repo")]
        repository: Option<String>,
        /// Print the list instead of opening the picker, as filters and JSON do.
        #[arg(short = 'p', long)]
        print: bool,
    },
    /// Attach this terminal to a workspace's or chat's agent session; Ctrl-\ detaches zmx.
    ///
    /// Terminal agents and chats run in sessions of a terminal host, such as
    /// zmx sessions, that keep running after their terminal closes or
    /// detaches. Without a target, attaches the
    /// current workspace's session, else the current checkout's only running
    /// chat, else offers a picker. An agent that sleeps or is stopped wakes
    /// at once, whatever the agent limits, and attaches once it runs.
    Attach {
        /// Workspace or chat, by number or ID; `-` for the one
        /// whose session this terminal last detached from.
        #[arg(id = "workspace", value_name = "TARGET")]
        target: Option<SpaceSelector>,
        /// Session to attach when the workspace has several.
        #[arg(long)]
        session: Option<String>,
    },
    /// Open a demo an agent started, or start, list and stop demos.
    ///
    /// A demo is a command Shoal keeps running in a workspace for you to try:
    /// a terminal app, a web server or a simulator app.
    #[command(args_conflicts_with_subcommands = true)]
    Demo {
        #[command(subcommand)]
        command: Option<DemoCommand>,
        /// Demo to open; without one, pick from the running demos.
        name: Option<String>,
        /// Workspace whose demos to offer; defaults to the current workspace,
        /// otherwise every workspace.
        #[arg(short = 'w', long)]
        workspace: Option<SpaceSelector>,
    },
    /// List, pause, resume and run triggers, which start agents on their own.
    ///
    /// A trigger is `[triggers.NAME]` config: a source, such as a timer,
    /// whose firings start an agent as shoal start would. These commands
    /// change a trigger's runtime state, never the config, and act on the
    /// triggers of the repository this runs in, or that --repo names, and
    /// the global config's.
    Trigger {
        #[command(subcommand)]
        command: TriggerCommand,
    },
}

#[derive(Debug, Subcommand)]
#[command(defer = true)]
pub enum TriggerCommand {
    /// List the triggers with their source, whether they are paused, their
    /// next firing and their last run's outcome and space.
    Ls {
        /// A registered repository other than the one this runs in.
        #[arg(short = 'r', long = "repo")]
        repository: Option<String>,
    },
    /// Pause a trigger: its source fires it no more, and a run it queued
    /// waits, until resumed, through config edits and restarts.
    Pause {
        name: String,
        /// A registered repository other than the one this runs in.
        #[arg(short = 'r', long = "repo")]
        repository: Option<String>,
    },
    /// Resume a paused trigger, counting its times from now, so the times
    /// it missed while paused never run.
    Resume {
        name: String,
        /// A registered repository other than the one this runs in.
        #[arg(short = 'r', long = "repo")]
        repository: Option<String>,
    },
    /// Fire a trigger now, through its filter, templates, key and overlap,
    /// and print its run's outcome, or the filter condition that dropped it.
    Run {
        name: String,
        /// A registered repository other than the one this runs in.
        #[arg(short = 'r', long = "repo")]
        repository: Option<String>,
        /// The body of the sample request a webhook trigger fires with,
        /// whose signature is not checked; an empty one without it.
        #[arg(long, value_name = "FILE")]
        payload: Option<std::path::PathBuf>,
        /// A header of the sample request; repeatable.
        #[arg(long = "header", value_name = "NAME=VALUE")]
        headers: Vec<String>,
    },
}

#[derive(Debug, Subcommand)]
#[command(defer = true)]
pub enum QueueCommand {
    /// Start a waiting entry now, regardless of the agent limits.
    Start {
        /// Entry by its ID, or a space by number or ID for its queued
        /// restore; omit to choose one.
        entry: Option<String>,
        /// Attach this terminal to the agent's session once it has one, as
        /// shoal attach does.
        #[arg(long)]
        attach: bool,
    },
    /// Cancel a waiting entry; a cancelled restore leaves its agent stopped.
    Cancel {
        /// Entry by its ID; omit to choose one.
        entry: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
#[command(defer = true)]
pub enum DemoCommand {
    /// Start a demo and tell the user how to open it; replaces a running
    /// demo of the same name.
    Start {
        /// A configured demo, or the name for the command after `--`;
        /// defaults to the only configured demo, or `demo` for a command.
        name: Option<String>,
        /// How the demo reaches the user, replacing the configured preset.
        #[arg(long, value_enum)]
        preset: Option<shoal_protocol::demos::Preset>,
        /// A web demo's URL scheme, replacing the configured one; http by default.
        #[arg(long, value_enum)]
        scheme: Option<shoal_protocol::demos::Scheme>,
        /// Shell command `shoal demo NAME` runs instead of opening the demo.
        #[arg(long, value_name = "COMMAND")]
        open: Option<String>,
        /// Port to reserve before the demo starts; repeat for more.
        #[arg(long = "port", value_name = "NAME")]
        ports: Vec<String>,
        /// Wait for a simulator when every one is busy.
        #[arg(long)]
        queue: bool,
        /// Workspace to use; defaults to the current workspace or picker.
        #[arg(short = 'w', long)]
        workspace: Option<SpaceSelector>,
        /// The command to run instead of the configured one.
        #[arg(last = true)]
        command: Vec<OsString>,
    },
    /// Stop a demo, or every demo of the workspace.
    Stop {
        name: Option<String>,
        /// Workspace to use; defaults to the current workspace or picker.
        #[arg(short = 'w', long)]
        workspace: Option<SpaceSelector>,
    },
    /// List configured and running demos.
    Ls {
        /// Workspace to list; defaults to the current workspace, otherwise
        /// the running demos of every workspace.
        #[arg(short = 'w', long)]
        workspace: Option<SpaceSelector>,
    },
    /// Print what a running demo shows.
    Screen {
        name: String,
        /// Lines to print from the end.
        #[arg(long, default_value_t = 50)]
        lines: usize,
        /// Workspace to use; defaults to the current workspace or picker.
        #[arg(short = 'w', long)]
        workspace: Option<SpaceSelector>,
    },
    /// Type text into a running demo, such as $'q' or $'\r' for Enter.
    Send {
        name: String,
        keys: String,
        /// Workspace to use; defaults to the current workspace or picker.
        #[arg(short = 'w', long)]
        workspace: Option<SpaceSelector>,
    },
}

// The options of `shoal restore --from`.
#[derive(Debug, Args)]
pub struct ImportArgs {
    /// A file shoal move wrote, a directory of them, or - for standard input; repeatable.
    #[arg(
        long,
        value_name = "FILE",
        conflicts_with_all = ["space", "all", "checkpoint", "execution", "discard_agent", "now", "focus", "permissions"]
    )]
    pub from: Vec<PathBuf>,
    /// Repository to restore into, when it does not share the written repository's origin.
    #[arg(short = 'r', long = "repo", requires = "from")]
    pub repository: Option<String>,
    /// Continue every agent's session, such as one whose files were copied by hand; refused for
    /// a copy, whose agents start from forks.
    #[arg(long, requires = "from", conflicts_with = "new_sessions")]
    pub keep_sessions: bool,
    /// Start new sessions even on the machine that wrote the file.
    #[arg(long, requires = "from", conflicts_with = "keep_sessions")]
    pub new_sessions: bool,
}

impl ImportArgs {
    /// Whether the agents keep their sessions; `None` keeps those on the
    /// machine that wrote the file or whose files it carries.
    pub fn sessions(&self) -> Option<bool> {
        match (self.keep_sessions, self.new_sessions) {
            (true, _) => Some(true),
            (_, true) => Some(false),
            _ => None,
        }
    }
}
