//! Arguments of commands that create workspaces.
use std::{ffi::OsString, path::PathBuf};

use clap::{Args, Subcommand};

use crate::{
    agent::Agent,
    cli::args::{
        AgentParser, ModelArgs, issue_input, permission_assignment, pull_input, run_input,
    },
    config::permissions::PermissionChange,
};
use shoal_ids::checkpoint_numbers::CheckpointNumber;
use shoal_protocol::launches::{ItemChoice, Launch, SpaceChoice, StartTarget};
use shoal_protocol::space_names::SpaceSelector;

fn start_target(input: &str) -> Result<StartTarget, String> {
    input.parse().map_err(|error| format!("{error:#}"))
}

#[derive(Debug, Subcommand)]
#[command(defer = true)]
pub enum CreateCommand {
    /// Start an agent in a mode, in a new or reopened workspace or in a chat.
    ///
    /// The mode decides what the agent is told and the kind of space it
    /// starts in; with --in, whether it is the workspace's worker or a chat
    /// in its worktree. Run inside a space, a new space goes below it.
    Start(Box<StartArgs>),
    /// Create a workspace in someone else's repository for a PR.
    ///
    /// Shoal clones the repository and, when you cannot push to it, forks it
    /// and pushes your branches to the fork. Once nothing of it is left, it
    /// removes the clone.
    Contribute(ContributeArgs),
    /// Create a workspace without starting an agent there.
    ///
    /// It starts from what shoal start takes for a workspace: a
    /// repository, an issue, PR or branch URL, --branch, --issue or a copy.
    /// Inside a space it goes below that space, or below the one --parent
    /// names, whose agent works on it and receives its issue and PR
    /// activity; outside one it is a top-level workspace. shoal start
    /// starts an agent of its own there.
    Create(Box<CreateArgs>),
    /// Turn a chat into a workspace or a workspace into a chat, or hand a
    /// space's work to another agent tool.
    ///
    /// The space stays the same: it keeps its number, name, parent,
    /// children, links, threads, files and progress. Its agent restarts
    /// in the new place once its current turn ends, carrying on with its
    /// conversation, or another tool takes the work over from copies of
    /// it. A space's agent converts the spaces its scope reaches.
    Convert {
        #[command(subcommand)]
        command: ConvertCommand,
    },
}

#[derive(Debug, Subcommand)]
pub enum ConvertCommand {
    /// Turn a chat into a workspace on a branch, with a worktree and setup.
    ///
    /// The branch is the local or remote one of that name, or a new one
    /// from the default branch or --base; without a branch, --issue or the
    /// chat's linked issue names it. A chat started in a workspace's
    /// worktree stays a chat.
    Workspace {
        #[command(flatten)]
        branch: BranchArgs,
        /// The mode the agent restarts in, which sets the workspace's
        /// permissions; defaults to worker.
        #[arg(short = 'm', long)]
        mode: Option<crate::config::modes::ModeName>,
        /// The chat, by number or ID; defaults to the space this runs in,
        /// else a picker.
        #[arg(short = 's', long)]
        space: Option<SpaceSelector>,
        /// Hand the work to this agent tool, which starts a new session
        /// from copies of the sessions so far, as shoal convert agent does,
        /// also from a stopped agent; the space's own tool continues its
        /// session.
        #[arg(short = 'a', long, value_parser = AgentParser)]
        agent: Option<Agent>,
        #[command(flatten)]
        carrying: CarryingArgs,
    },
    /// Turn a workspace into a chat in the repository's checkout.
    ///
    /// The worktree goes as shoal rm removes it, once a checkpoint keeps its
    /// HEAD and unsaved files; the branch stays, and shoal restore brings
    /// the files back. Demos stop, and ports, resources, simulators and
    /// secrets are released. Refused while the user holds the workspace or
    /// another workspace builds on its branch.
    Chat {
        /// The mode the agent restarts in; defaults to chat.
        #[arg(short = 'm', long)]
        mode: Option<crate::config::modes::ModeName>,
        /// The workspace, by number or ID; defaults to the space this runs
        /// in, else a picker.
        #[arg(short = 's', long)]
        space: Option<SpaceSelector>,
        /// Hand the work to this agent tool, which starts a new session
        /// from copies of the sessions so far, as shoal convert agent does,
        /// also from a stopped agent; the space's own tool continues its
        /// session.
        #[arg(short = 'a', long, value_parser = AgentParser)]
        agent: Option<Agent>,
        #[command(flatten)]
        carrying: CarryingArgs,
    },
    /// Hand a space's work over to another agent tool.
    ///
    /// The tool starts a new session in the same space, told what the
    /// space is and where the copies of its sessions so far are, which go
    /// into its attached files; no session is converted between tools. A
    /// running agent hands over once its turn ends, a stopped or asleep one
    /// at once.
    Agent {
        /// The agent tool that takes the work over, a built-in agent or a
        /// configured command; the space's own is refused.
        #[arg(value_parser = AgentParser)]
        tool: Agent,
        /// The mode the new agent starts in; defaults to the space's.
        #[arg(short = 'm', long)]
        mode: Option<crate::config::modes::ModeName>,
        /// The space, by number or ID; defaults to the space this runs in,
        /// else a picker.
        #[arg(short = 's', long)]
        space: Option<SpaceSelector>,
        #[command(flatten)]
        carrying: CarryingArgs,
    },
}

#[derive(Debug, Default, Clone, Args)]
pub struct CarryingArgs {
    #[command(flatten)]
    pub model: ModelArgs,
    /// Set a permission of the agent, as KEY=VALUE such as push=propose;
    /// KEY=mode drops the value given before, and keys left out keep
    /// theirs. Repeatable. A space's agent gives only what it holds itself.
    #[arg(long = "permission", value_name = "KEY=VALUE", value_parser = permission_assignment)]
    pub permissions: Vec<PermissionChange>,
    /// What the agent tool taking the work over is told after its mode's
    /// instructions.
    #[arg(last = true)]
    pub note: Vec<String>,
}

impl CarryingArgs {
    /// The change these arguments make to the agent, handing the work to
    /// `agent` when given.
    pub fn change(self, agent: Option<Agent>) -> crate::protocol::AgentChange {
        crate::protocol::AgentChange {
            agent,
            model: self.model.into(),
            permissions: PermissionChange::combined(&self.permissions),
            note: (!self.note.is_empty()).then(|| self.note.join(" ")),
        }
    }
}

#[derive(Debug, Default, Clone, Args)]
pub struct BranchArgs {
    /// Git branch name; a portable workspace name is derived from it.
    /// Without one, --issue names it; a copy's defaults to the copied
    /// branch's name with -copy added.
    pub branch: Option<String>,
    /// Derive the branch name from a forge issue number or URL and link the issue.
    #[arg(short = 'i', long, value_parser = issue_input)]
    pub issue: Option<crate::forge::items::reference::ItemInput>,
    /// Starting Git ref of a new branch (defaults to the repository's
    /// default branch, refreshed from its upstream); a parent's branch
    /// stacks the child on it.
    #[arg(long, value_name = "REF")]
    pub base: Option<String>,
    /// Skip setup.command, hooks.pre_setup, setup.copy_ignored, hooks.post_setup
    /// and the removal hooks; `shoal setup` runs the setup later.
    #[arg(long)]
    pub skip_setup: bool,
}

#[derive(Debug, Default, Clone, Args)]
pub struct CreateArgs {
    /// Registered repository, or an issue, PR, CI run or branch URL;
    /// defaults to the current checkout or workspace, else a picker.
    #[arg(value_parser = start_target)]
    pub repository: Option<StartTarget>,
    /// Repository for an issue number or a pasted link.
    #[arg(short = 'r', long = "repo")]
    pub repository_override: Option<String>,
    /// The branch to create it on, as git switch takes it: an existing local
    /// branch or remote/branch opens, in its workspace when one has it; any
    /// other name is a new branch, never suffixed.
    #[arg(long, value_name = "NAME")]
    pub branch: Option<String>,
    /// The forge issue number or URL it links; its branch and name derive
    /// from it.
    #[arg(short = 'i', long, value_parser = issue_input)]
    pub issue: Option<crate::forge::items::reference::ItemInput>,
    /// Create a workspace at a commit, with no branch of its own, for
    /// comparisons and research: at --base, a tag or commit, or the default
    /// branch.
    #[arg(long, conflicts_with_all = ["branch", "issue", "path", "git_profile"])]
    pub no_branch: bool,
    /// Create this worktree at an exact new directory instead of the repository default.
    #[arg(long)]
    pub path: Option<PathBuf>,
    /// Starting Git ref of a new branch (defaults to the repository's default
    /// branch, refreshed from its upstream); a parent's branch stacks the
    /// workspace on it.
    #[arg(long, value_name = "REF")]
    pub base: Option<String>,
    /// Apply a named Git profile to the new worktree, overriding repository defaults.
    #[arg(long)]
    pub git_profile: Option<String>,
    /// The space it goes below, by number or ID; defaults to the space this
    /// runs in, else none.
    #[arg(long, value_name = "SPACE")]
    pub parent: Option<SpaceSelector>,
    /// Its name, free text at most 40 columns wide; defaults to the linked
    /// issue's or PR's title, else its branch.
    #[arg(long)]
    pub name: Option<shoal_protocol::space_names::SpaceName>,
    /// Its priority: low, normal (the default), urgent, or a number from 0
    /// to 100. A space's agent gives at most its own space's.
    #[arg(long)]
    pub priority: Option<shoal_schedule::priority::Priority>,
    #[command(flatten)]
    pub copy: CopyArgs,
    #[command(flatten)]
    pub creation: CreationArgs,
}

impl CreateArgs {
    /// The start of a workspace without an agent these ask for, below
    /// `parent`, apart from where it is made.
    pub(in crate::cli) fn launch(&self, parent: Option<SpaceSelector>) -> Launch {
        Launch {
            target: self.repository.clone(),
            repository: self.repository_override.clone(),
            branch: self.branch.clone(),
            path: self.path.clone(),
            base: self.base.clone(),
            git_profile: self.git_profile.clone(),
            issue: self.issue.clone().map(ItemChoice::Named),
            copy: self.copy.copy.clone().map(SpaceChoice::from),
            checkpoint: self.copy.checkpoint,
            skip_setup: self.creation.skip_setup,
            disposable: self.creation.disposable,
            name: self.name.clone(),
            priority: self.priority,
            parent,
            ..Launch::default()
        }
    }
}

#[derive(Debug, Default, Clone, Args)]
pub struct StartArgs {
    /// The mode the agent starts in; for an issue or PR carrying a mode's
    /// [modes.NAME] label, that mode; else on a terminal, a picker; else
    /// worker.
    #[arg(short = 'm', long)]
    pub mode: Option<crate::config::modes::ModeName>,
    /// Set a permission of the agent, as KEY=VALUE such as push=propose or
    /// merge=direct, whatever its mode or the space above allows;
    /// repeatable. A space's agent gives only what it holds itself.
    #[arg(long = "permission", value_name = "KEY=VALUE", value_parser = permission_assignment)]
    pub permissions: Vec<crate::config::permissions::PermissionChange>,
    /// The new space's priority: low, normal (the default), urgent, or a
    /// number from 0 to 100; its queued work starts before lower ones'. A
    /// space's agent gives at most its own space's.
    #[arg(long)]
    pub priority: Option<shoal_schedule::priority::Priority>,
    /// Start in this ready workspace instead of a new space: its worker in a
    /// mode that starts in a workspace, else a chat in its worktree; without
    /// a value, the current workspace, else a picker.
    #[arg(
        long = "in",
        num_args = 0..=1,
        value_name = "WORKSPACE",
        conflicts_with_all = ["branch", "path", "base", "git_profile", "skip_setup", "disposable", "copy", "checkpoint"]
    )]
    pub in_workspace: Option<Option<SpaceSelector>>,
    /// Discard the stopped or asleep agent of the workspace the start goes
    /// into, with --in or by its branch or issue, and start the new agent in
    /// its place; a running agent stays.
    #[arg(long)]
    pub replace: bool,
    /// The space the new one goes below, by number or ID; defaults to the
    /// space this runs in, else none.
    #[arg(long, value_name = "SPACE")]
    pub parent: Option<SpaceSelector>,
    /// The joined machine the new space starts on, by its name or the ID
    /// shoal hub ls shows, where that machine's queue starts its agent in
    /// the background, or at once for --here and --focus, which attach this
    /// terminal to it there; defaults to this machine.
    #[arg(long, value_name = "MACHINE", conflicts_with = "in_workspace")]
    pub machine: Option<String>,
    /// Run in this terminal instead of a new tab or detached session.
    #[arg(short = 'H', long)]
    pub here: bool,
    /// Give the new tab or session the terminal, overriding the `focus` setting;
    /// its agent starts now, as with --now.
    #[arg(short = 'f', long, conflicts_with = "here")]
    pub focus: bool,
    /// Return only once a new workspace is set up, with its worktree, setup
    /// and post-setup hook done, instead of once it is recorded.
    #[arg(long)]
    pub wait: bool,
    /// Registered repository, or an issue, PR, CI run or branch URL;
    /// defaults to the current checkout or workspace, else a picker.
    #[arg(value_parser = start_target)]
    pub repository: Option<StartTarget>,
    /// Repository for an issue number or a pasted link.
    #[arg(short = 'r', long = "repo")]
    pub repository_override: Option<String>,
    /// The branch to start on, as git switch takes it: an existing local
    /// branch or remote/branch opens, in its workspace when one has it, as
    /// --in does; any other name is a new branch, never suffixed. A
    /// portable workspace name is derived from it.
    #[arg(long, value_name = "NAME")]
    pub branch: Option<String>,
    /// The forge issue number or URL the agent starts on; a workspace's name
    /// derives from it. Without a value, offers the open issues, those
    /// carrying the label of the mode --mode names where it has one.
    #[arg(short = 'i', long, num_args = 0..=1, value_parser = issue_input)]
    pub issue: Option<Option<crate::forge::items::reference::ItemInput>>,
    /// The forge PR number or URL whose branch a new workspace starts on, as
    /// a PR link does. Without a value, offers the open PRs, those carrying
    /// the label of the mode --mode names where it has one.
    #[arg(long, num_args = 0..=1, value_parser = pull_input)]
    pub pr: Option<Option<crate::forge::items::reference::ItemInput>>,
    /// The CI run the agent starts on, by the number in its URL or the URL,
    /// as a run link does: in its PR's or branch's workspace, or a chat for a
    /// run on the default branch or a tag. Without a value, offers the runs
    /// that fail now.
    #[arg(long, num_args = 0..=1, value_parser = run_input)]
    pub ci: Option<Option<crate::forge::items::reference::ItemInput>>,
    /// Create this worktree at an exact new directory instead of the repository default.
    #[arg(long)]
    pub path: Option<PathBuf>,
    /// Starting Git ref of a new branch (defaults to the repository's default
    /// branch, refreshed from its upstream; a remote branch such as
    /// origin/feature is fetched first); for an existing branch, the ref its
    /// changes are compared against.
    #[arg(long, value_name = "REF")]
    pub base: Option<String>,
    /// Apply a named Git profile to the new worktree, overriding repository defaults.
    #[arg(long)]
    pub git_profile: Option<String>,
    /// Agent to start, a built-in agent or a configured command; defaults to
    /// agent.default, otherwise a picker.
    #[arg(short = 'a', long, value_parser = AgentParser)]
    pub agent: Option<Agent>,
    /// Start the agent from a fork of the session of the parent space's
    /// agent, or with --copy of the copied workspace's, so it begins with
    /// that conversation, which stays unchanged.
    #[arg(long, conflicts_with = "checkpoint")]
    pub fork: bool,
    /// Start the agent now even when max_heavy_agents is reached, instead of queuing it.
    #[arg(long)]
    pub now: bool,
    /// Queue the agent until this space, queued agent or issue is complete;
    /// repeat for more. An issue's agent is also blocked by the issues the
    /// forge records as blocking it.
    #[arg(long, value_name = "BLOCKER", conflicts_with_all = ["now", "focus"])]
    pub add_blocker: Vec<String>,
    /// The new space's name, free text at most 40 columns wide; defaults
    /// to the linked issue's or PR's title, else a workspace's branch, or
    /// for a chat "<repo> <mode>", or "<workspace name> chat" with --in.
    #[arg(long)]
    pub name: Option<shoal_protocol::space_names::SpaceName>,
    #[command(flatten)]
    pub copy: CopyArgs,
    #[command(flatten)]
    pub creation: CreationArgs,
    #[command(flatten)]
    pub model: ModelArgs,
    /// Arguments forwarded to the agent, such as its prompt.
    #[arg(last = true)]
    pub args: Vec<OsString>,
}

impl StartArgs {
    /// The launch these ask for, apart from the agent's task, the session
    /// `--fork` forks and where the start is made, which only this process
    /// reads.
    pub(in crate::cli) fn launch(&self) -> Launch {
        Launch {
            target: self.repository.clone(),
            repository: self.repository_override.clone(),
            branch: self.branch.clone(),
            path: self.path.clone(),
            base: self.base.clone(),
            git_profile: self.git_profile.clone(),
            issue: self.issue.clone().map(ItemChoice::from),
            pr: self.pr.clone().map(ItemChoice::from),
            ci: self.ci.clone().map(ItemChoice::from),
            copy: self.copy.copy.clone().map(SpaceChoice::from),
            checkpoint: self.copy.checkpoint,
            skip_setup: self.creation.skip_setup,
            disposable: self.creation.disposable,
            in_workspace: self.in_workspace.clone().map(SpaceChoice::from),
            replace: self.replace,
            mode: self.mode.clone(),
            agent: self.agent.clone(),
            model: self.model.clone().into(),
            fork: None,
            carried_copy: None,
            name: self.name.clone(),
            priority: self.priority,
            permissions: PermissionChange::combined(&self.permissions).set,
            parent: self.parent.clone(),
            blockers: self.add_blocker.clone(),
            now: self.now,
            here: false,
            needs_this_machine: false,
            task: Vec::new(),
            prompt: None,
            setup: None,
            directory: None,
            env: None,
            setup_env: None,
            unplaceable: None,
            machine: self.machine.clone(),
            forwarded: None,
        }
    }
}

#[derive(Debug, Default, Clone, Args)]
pub struct CopyArgs {
    /// Start from a copy of this workspace's current files, uncommitted and
    /// untracked ones included; without a value, the current workspace, else
    /// a picker. Conflicts with --base.
    #[arg(long, num_args = 0..=1, value_name = "WORKSPACE", conflicts_with = "base")]
    pub copy: Option<Option<SpaceSelector>>,
    /// Start from this checkpoint of the --copy workspace, live or removed,
    /// instead of its current files; shoal restore <workspace> --checkpoint
    /// lists them.
    #[arg(long, value_name = "NUMBER", conflicts_with = "base")]
    pub checkpoint: Option<CheckpointNumber>,
}

impl CopyArgs {
    pub fn is_set(&self) -> bool {
        self.copy.is_some() || self.checkpoint.is_some()
    }
}

#[derive(Debug, Default, Clone, Args)]
pub struct CreationArgs {
    /// Skip setup.command, hooks.pre_setup, setup.copy_ignored, hooks.post_setup
    /// and the removal hooks; `shoal setup` runs the setup later.
    #[arg(long)]
    pub skip_setup: bool,
    /// Nothing in it is worth keeping: every removal deletes it whatever it
    /// holds, without asking or keeping a checkpoint, and deletes its branch.
    #[arg(long)]
    pub disposable: bool,
}

impl From<CreationArgs> for crate::protocol::CreationOptions {
    fn from(args: CreationArgs) -> Self {
        Self {
            name: None,
            skip_setup: args.skip_setup,
            disposable: args.disposable,
            priority: None,
        }
    }
}

#[derive(Debug, Args)]
pub struct ContributeArgs {
    /// Run in this terminal instead of a new tab or detached session.
    #[arg(short = 'H', long)]
    pub here: bool,
    /// Give the new tab or session the terminal, overriding the `focus` setting;
    /// its agent starts now, as with --now.
    #[arg(short = 'f', long, conflicts_with = "here")]
    pub focus: bool,
    /// Return only once a new workspace is set up, with its worktree, setup
    /// and post-setup hook done, instead of once it is recorded.
    #[arg(long)]
    pub wait: bool,
    /// Repository URL, or an issue, PR or branch URL.
    #[arg(value_parser = start_target)]
    pub target: StartTarget,
    /// The branch to start on, as git switch takes it: an existing local
    /// branch or remote/branch opens, in its workspace when one has it, as
    /// --in does; any other name is a new branch, never suffixed. A
    /// portable workspace name is derived from it.
    #[arg(long, value_name = "NAME")]
    pub branch: Option<String>,
    /// Start a built-in agent or a configured command after worktree creation.
    #[arg(short = 'a', long, value_parser = AgentParser)]
    pub agent: Option<Agent>,
    /// Start the agent now even when max_heavy_agents is reached, instead of queuing it.
    #[arg(long)]
    pub now: bool,
    #[command(flatten)]
    pub model: ModelArgs,
    /// Arguments forwarded to the agent.
    #[arg(last = true)]
    pub args: Vec<OsString>,
}
