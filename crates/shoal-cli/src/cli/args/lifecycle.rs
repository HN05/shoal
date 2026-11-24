//! Arguments of commands that stop, remove and move workspaces.
use clap::{Args, Subcommand};

use crate::{
    cli::args::{ConfirmationArgs, SpaceTargets, transfer::MoveArgs},
    forge::items::action::MergeMethod,
};
use shoal_protocol::space_names::SpaceSelector;

#[derive(Debug, Subcommand)]
#[command(defer = true)]
pub enum LifecycleCommand {
    /// Stop tracked agents and commands, and chats, saving them for `shoal resume`, and the other
    /// processes of the workspaces; keep the spaces.
    ///
    /// Agents waiting in the agent queue to start in the workspaces leave the queue, as does a queued
    /// agent named instead of a space.
    ///
    /// With `--processes` or `--leftovers`, stop only processes Shoal does not track, of workspaces or
    /// chats, the current one by default. Untracked processes carry the space's identity or are its
    /// leftovers: processes still running after what started them exited, tied to the space by
    /// working directory, temporary directory, program or identity, with what they started.
    Stop(StopArgs),
    /// Remove spaces, workspaces or chats, and release their resources.
    ///
    /// Removing a space first removes the spaces below it and stops its agent. A branch goes
    /// too when a remote or the default branch has all its commits; otherwise it stays.
    /// Deleting uncommitted changes or commits on a detached HEAD needs confirmation, asked
    /// once for every space.
    ///
    /// A space's removal goes on in the background once Shoal has reserved it and checked that
    /// it can go; a failure after that keeps the space and arrives as a notification.
    Rm(RmArgs),
    /// Fold a space below yours into a space of your subtree, which takes over its work, then
    /// remove it.
    ///
    /// Its branch merges into the branch of the workspace that takes it, by `-X` or [merge]
    /// method, and its uncommitted files carry over there as uncommitted changes. Its links,
    /// attached files and open threads go with the work, and the spaces below it move up one
    /// level. A space whose work is not done at its HEAD, or that a chat in its worktree has not
    /// finished, needs `--force`, which stops their agents first. Its removal keeps a
    /// checkpoint, from which `shoal restore` reopens its files.
    Absorb(AbsorbArgs),
    /// Remove what Shoal left behind: spaces cleanup would remove, kept branches and caches.
    ///
    /// Without a kind, lists them; on a terminal, a picker offers them, where Tab selects several
    /// and nothing starts selected.
    Cleanup {
        #[command(subcommand)]
        kind: Option<CleanupKind>,
        #[command(flatten)]
        args: CleanupArgs,
    },
    /// Move workspaces and chats to another machine, over SSH or through a file.
    ///
    /// A moved space ends here, with every space below it and the chats in the worktree of each
    /// workspace among them, and continues where it goes under its ID, its agents' sessions
    /// carried with it. --no-recursive leaves the spaces below here instead, moved up one level;
    /// --keep leaves the spaces running and moves copies of them instead.
    ///
    /// --ssh moves each space once ssh reaches Shoal there, which clones the repository from its
    /// origin when it has none, and resumes its agents there unless --no-resume; nothing ends when it does not reach
    /// Shoal. A workspace whose import fails stays removed here, and shoal restore opens it
    /// again.
    ///
    /// -o writes the spaces to a file that shoal restore --from opens: a workspace is removed
    /// and the file holds its checkpoint, as a Git bundle without the commits of the default
    /// branch, its base, links and stopped agents; a chat's agent is stopped once it has
    /// reported its session. A removed space is written without another removal, with the
    /// spaces its removal took. When -o names a directory, several spaces, or with --all every
    /// space of the repository, go to a file each in it, those below another in its file, once
    /// each one can be written. -o - writes standard output, which must not be a terminal.
    ///
    /// Without a destination, on a terminal, it asks for one after the spaces are picked.
    Move(MoveArgs),
}

#[derive(Debug, Subcommand)]
#[command(defer = true)]
pub enum CleanupKind {
    /// Remove the spaces, workspaces and chats, idle or merged cleanup would remove, without
    /// waiting for their idle periods.
    ///
    /// Applies where those rules are disabled; held, busy, unpushed or unmerged dirty workspaces and
    /// running chats are kept, and the unsaved files of merged ones go into their checkpoints.
    /// Spaces, by number or ID, limit it to those.
    Spaces { spaces: Vec<SpaceSelector> },
    /// Delete branches Shoal kept when it removed their workspaces, with the commits only they have.
    ///
    /// Removal keeps a branch only while some of its commits are on no remote and not on the
    /// default branch, and deletes it once they are. Without names or --all, choose them in a
    /// picker, where Tab selects several.
    Branches {
        names: Vec<String>,
        /// Delete every kept branch.
        #[arg(short = 'A', long, conflicts_with = "names")]
        all: bool,
    },
    /// Empty the shared caches of repositories without running executions.
    Caches,
}

// Options of `shoal cleanup` that every kind takes.
#[derive(Debug, Args)]
pub struct CleanupArgs {
    /// List what would be removed without removing anything.
    #[arg(long, global = true)]
    pub dry_run: bool,
    /// Only this repository's spaces, kept branches and caches, by name or ID.
    #[arg(short = 'r', long, global = true)]
    pub repo: Option<String>,
    /// Delete kept branches without asking.
    #[arg(short = 'y', long, global = true)]
    pub yes: bool,
}

// Arguments of `shoal stop`.
#[derive(Debug, Args)]
pub struct StopArgs {
    #[command(flatten)]
    pub targets: SpaceTargets,
    /// Treat executions Shoal cannot prove ended as stopped without asking.
    #[arg(short = 'y', long)]
    pub yes: bool,
    /// Keep running the spaces below the selected ones.
    #[arg(long, conflicts_with_all = ["all", "machines"])]
    pub skip_children: bool,
    /// Cancel the current turns of the agents instead, keeping them running.
    ///
    /// Shoal types each agent's interrupt key into its terminal session, records its turn
    /// as ended, and leaves agents without a running turn alone.
    #[arg(long, conflicts_with_all = ["all", "machines", "yes", "skip_children"])]
    pub interrupt: bool,
    /// Stop only the processes Shoal does not track, leftovers included.
    #[arg(
        long,
        conflicts_with_all = ["all", "machines", "yes", "skip_children", "interrupt", "leftovers"]
    )]
    pub processes: bool,
    /// Stop only leftovers.
    #[arg(long, conflicts_with_all = ["all", "machines", "yes", "skip_children", "interrupt"])]
    pub leftovers: bool,
}

// Arguments of `shoal absorb`.
#[derive(Debug, Args)]
pub struct AbsorbArgs {
    /// The space to absorb, by number or ID.
    #[arg(value_name = "SPACE")]
    pub space: SpaceSelector,
    /// The space that takes its work, by number or ID; defaults to your own space, or from your
    /// terminal to the absorbed space's parent.
    #[arg(long, value_name = "SPACE")]
    pub into: Option<SpaceSelector>,
    /// How its branch merges, by [merge] method when left out, and else by a fast-forward or
    /// merge commit.
    #[arg(short = 'X', long, value_enum)]
    pub method: Option<MergeMethod>,
    /// The commit message of a squash.
    #[arg(long)]
    pub message: Option<String>,
    /// Leave its uncommitted files behind instead of carrying them over; its checkpoint keeps
    /// them.
    #[arg(long)]
    pub discard_uncommitted: bool,
    /// Absorb work that is not done, stopping its agents first, or leave behind code that the
    /// space taking it has no branch for.
    #[arg(long)]
    pub force: bool,
}

// Arguments of `shoal rm`.
#[derive(Debug, Args)]
pub struct RmArgs {
    #[arg(value_name = "SPACE")]
    pub space: Vec<SpaceSelector>,
    #[command(flatten)]
    pub confirmation: ConfirmationArgs,
    /// Wait until each space is removed, reporting the branches kept.
    #[arg(long)]
    pub wait: bool,
}
