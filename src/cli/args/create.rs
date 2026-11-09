//! Arguments of commands that create workspaces.
use crate::client::space_ids::SpaceId;
use std::{ffi::OsString, path::PathBuf};

use clap::{Args, Subcommand};

use crate::client::space_names::SpaceSelector;
use crate::model::CheckpointNumber;
use crate::{
    agent::Agent,
    cli::args::{AgentParser, ModelArgs, item_reference, permission_assignment},
};

#[derive(Debug, Subcommand)]
#[command(defer = true)]
pub enum CreateCommand {
    /// Start an agent in a mode, in a new or reopened workspace or in a chat.
    ///
    /// The mode decides what the agent is told and, unless --chat, --worktree
    /// or --in says otherwise, the kind of space it starts in. Run inside a
    /// space, the new space goes below it.
    Start(Box<StartArgs>),
    /// Create a workspace in someone else's repository for a PR.
    ///
    /// Shoal clones the repository and, when you cannot push to it, forks it
    /// and pushes your branches to the fork. Once nothing of it is left, it
    /// removes the clone.
    Contribute(ContributeArgs),
    /// Create a child workspace on a new branch for work in its own PR.
    ///
    /// The parent workspace's agent works on the child and receives its
    /// issue and PR activity; the parent's cleanup waits until its children
    /// are removed. `shoal start` starts an agent of the child's own.
    Split {
        /// Git branch name; a portable workspace name is derived from it.
        /// A copy's defaults to the copied branch's name with -copy added.
        #[arg(required_unless_present_any = ["issue", "no_branch", "copy", "checkpoint"])]
        branch: Option<String>,
        /// Create a workspace at a commit, with no branch of its own, for
        /// comparisons and research: at --base, a tag or commit, or the default
        /// branch. Without a parent workspace, the user owns it.
        #[arg(long, conflicts_with_all = ["branch", "issue"])]
        no_branch: bool,
        /// Derive the branch name from a forge issue number or URL and link the issue.
        #[arg(short = 'i', long, value_parser = item_reference)]
        issue: Option<String>,
        /// Starting Git ref (defaults to the repository's default branch, refreshed from its
        /// upstream); the parent's branch stacks the child on it.
        #[arg(long, value_name = "REF")]
        base: Option<String>,
        /// Parent workspace; defaults to the current workspace or picker, or
        /// with --no-branch to none outside a workspace.
        #[arg(short = 'w', long)]
        workspace: Option<SpaceSelector>,
        #[command(flatten)]
        copy: CopyArgs,
        #[command(flatten)]
        creation: CreationArgs,
    },
}

// Arguments of `shoal start`.
#[derive(Debug, Default, Args)]
pub struct StartArgs {
    /// The mode the agent starts in; on a terminal, a picker, which an issue
    /// labelled [issue] planned_label or plan_label answers with worker or
    /// plan when the arguments ask for neither kind of space; else chat with
    /// --chat or --in, else worker.
    #[arg(short = 'm', long)]
    pub mode: Option<crate::config::modes::ModeName>,
    /// Set a permission of the agent, as KEY=VALUE such as push=propose or
    /// merge=direct, whatever its mode or the space above allows;
    /// repeatable. A space's agent gives only what it holds itself.
    #[arg(long = "permission", value_name = "KEY=VALUE", value_parser = permission_assignment)]
    pub permissions: Vec<crate::config::permissions::PermissionChange>,
    /// The permissions a restored or imported chat was given, as JSON,
    /// which a start in a background place keeps unchecked.
    #[arg(long, hide = true, value_parser = recorded, conflicts_with = "permissions")]
    pub recorded_narrowing: Option<crate::config::permissions::PermissionsConfig>,
    /// Start in a chat, whatever the mode's kind.
    #[arg(long, conflicts_with_all = ["worktree", "branch", "existing", "path", "base", "git_profile", "now", "after", "focus", "fork", "skip_setup", "disposable", "copy", "checkpoint"])]
    pub chat: bool,
    /// Start in a workspace with a worktree, whatever the mode's kind.
    #[arg(long, conflicts_with_all = ["name", "in_workspace"])]
    pub worktree: bool,
    /// Run the chat in this ready workspace's worktree; without a value, the
    /// current workspace, else a picker.
    #[arg(
        long = "in",
        num_args = 0..=1,
        value_name = "WORKSPACE",
        conflicts_with_all = ["branch", "existing", "path", "base", "git_profile", "now", "after", "focus", "fork", "skip_setup", "disposable", "copy", "checkpoint"]
    )]
    pub in_workspace: Option<Option<SpaceSelector>>,
    /// The space the new one goes below, by name or number; defaults to the
    /// space this runs in, else none.
    #[arg(long, value_name = "SPACE")]
    pub parent: Option<SpaceSelector>,
    /// Run in this terminal instead of a new tab or detached session.
    #[arg(short = 'H', long)]
    pub here: bool,
    /// Give the new tab or session the terminal, overriding the `focus` setting;
    /// its agent starts now, as with --now.
    #[arg(short = 'f', long, conflicts_with = "here")]
    pub focus: bool,
    /// Registered repository, or an issue, PR or branch URL; defaults to the
    /// current checkout or workspace, else a picker.
    #[arg(value_parser = item_reference)]
    pub repository: Option<String>,
    /// Repository for an issue number or a pasted link.
    #[arg(short = 'r', long = "repo")]
    pub repository_override: Option<String>,
    /// The repository the start resolved and checked before choosing what it
    /// starts, which a workspace's start then takes in place of the
    /// arguments that named it.
    #[arg(skip)]
    pub checked_repository: Option<crate::repository::Selector>,
    /// Git branch name; a portable workspace name is derived from it.
    pub branch: Option<String>,
    /// Use an existing local branch or remote/branch without creating a new branch.
    #[arg(long, conflicts_with_all = ["branch", "issue", "copy", "checkpoint"])]
    pub existing: Option<String>,
    /// The forge issue number or URL the agent starts on; a workspace's name
    /// derives from it. Without a value in plan mode, offers the open issues
    /// labelled [issue] plan_label.
    #[arg(short = 'i', long, num_args = 0..=1, value_parser = item_reference)]
    pub issue: Option<Option<String>>,
    /// Create this worktree at an exact new directory instead of the repository default.
    #[arg(long)]
    pub path: Option<PathBuf>,
    /// Starting Git ref (defaults to the repository's default branch, refreshed from its upstream;
    /// a remote branch such as origin/feature is fetched first);
    /// with --existing, the ref its changes are compared against.
    #[arg(long, value_name = "REF")]
    pub base: Option<String>,
    /// Apply a named Git profile to the new worktree, overriding repository defaults.
    #[arg(long)]
    pub git_profile: Option<String>,
    /// Agent to start, a built-in agent or a configured command; defaults to
    /// agent.default, otherwise a picker.
    #[arg(short = 'a', long, value_parser = AgentParser)]
    pub agent: Option<Agent>,
    /// Start the agent from a fork of the session of the parent workspace's
    /// agent, or with --copy of the copied workspace's, so it begins with
    /// that conversation, which stays unchanged.
    #[arg(long, conflicts_with = "checkpoint")]
    pub fork: bool,
    /// Start the agent now even when max_agents is reached, instead of queuing it.
    #[arg(long)]
    pub now: bool,
    /// Queue the agent until this workspace, queued agent or issue is complete;
    /// repeat for more. An issue's agent also waits for the issues blocking it.
    #[arg(long, value_name = "WORK", conflicts_with_all = ["now", "focus"])]
    pub after: Vec<String>,
    /// Name of a chat; defaults to <repo>-chat, <workspace>-chat with --in,
    /// or <repo>-<mode>-<issue number> with --issue, asked for on a terminal.
    #[arg(long)]
    pub name: Option<crate::client::space_names::SpaceName>,
    #[command(flatten)]
    pub copy: CopyArgs,
    #[command(flatten)]
    pub creation: CreationArgs,
    #[command(flatten)]
    pub model: ModelArgs,
    /// Agent session the chat continues, which shoal move import passes.
    #[arg(long, hide = true, requires = "chat")]
    pub session: Option<String>,
    /// Start from a fork of --session instead of continuing it, which shoal
    /// move import passes for a copy.
    #[arg(long, hide = true, requires = "session")]
    pub fork_session: bool,
    /// Stopped chat, by ID, that this start restores under its own name,
    /// which shoal resume passes.
    #[arg(long, hide = true, requires_all = ["chat", "name"])]
    pub restore: Option<SpaceId>,
    /// Issue, by URL, that an imported chat is linked to, whose fields its
    /// rules take without asking the forge, which shoal move import passes.
    #[arg(long, hide = true, requires = "chat")]
    pub linked_issue: Option<String>,
    /// Title of --linked-issue.
    #[arg(long, hide = true, requires = "linked_issue")]
    pub linked_issue_title: Option<String>,
    /// Handoff of the starter that waits on this background place, which
    /// shoal start passes.
    #[arg(long, hide = true, requires_all = ["chat", "here"])]
    pub handoff: Option<PathBuf>,
    /// Arguments forwarded to the agent, such as its prompt.
    #[arg(last = true)]
    pub args: Vec<OsString>,
}

// The workspace a new one copies. A doc comment here would replace the
// description of every command that flattens it.
#[derive(Debug, Default, Args)]
pub struct CopyArgs {
    /// Start from a copy of this workspace's current files, uncommitted and
    /// untracked ones included; without a value, the current workspace, else
    /// a picker. Conflicts with --base.
    #[arg(long, num_args = 0..=1, value_name = "WORKSPACE", conflicts_with = "base")]
    pub copy: Option<Option<SpaceSelector>>,
    /// Start from this checkpoint of the --copy workspace, live or removed,
    /// instead of its current files; shoal work restore <workspace> lists
    /// them.
    #[arg(long, value_name = "NUMBER", conflicts_with = "base")]
    pub checkpoint: Option<CheckpointNumber>,
}

impl CopyArgs {
    /// Whether a copy was asked for.
    pub fn is_set(&self) -> bool {
        self.copy.is_some() || self.checkpoint.is_some()
    }
}

// What a new workspace leaves out. A doc comment here would replace the
// description of every command that flattens it.
#[derive(Debug, Default, Args)]
pub struct CreationArgs {
    /// Skip setup.command, hooks.pre_setup, setup.copy_ignored, hooks.post_setup
    /// and the removal hooks; `shoal work setup` runs the setup later.
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
            skip_setup: args.skip_setup,
            disposable: args.disposable,
        }
    }
}

// Arguments of `shoal contribute`.
#[derive(Debug, Args)]
pub struct ContributeArgs {
    /// Run in this terminal instead of a new tab or detached session.
    #[arg(short = 'H', long)]
    pub here: bool,
    /// Give the new tab or session the terminal, overriding the `focus` setting;
    /// its agent starts now, as with --now.
    #[arg(short = 'f', long, conflicts_with = "here")]
    pub focus: bool,
    /// Repository URL, or an issue, PR or branch URL.
    pub target: String,
    /// Git branch name; a portable workspace name is derived from it.
    pub branch: Option<String>,
    /// Start a built-in agent or a configured command after worktree creation.
    #[arg(short = 'a', long, value_parser = AgentParser)]
    pub agent: Option<Agent>,
    /// Start the agent now even when max_agents is reached, instead of queuing it.
    #[arg(long)]
    pub now: bool,
    #[command(flatten)]
    pub model: ModelArgs,
    /// Arguments forwarded to the agent.
    #[arg(last = true)]
    pub args: Vec<OsString>,
}

/// A recorded narrowing passed on as JSON.
fn recorded(json: &str) -> Result<crate::config::permissions::PermissionsConfig, String> {
    serde_json::from_str(json).map_err(|error| error.to_string())
}
