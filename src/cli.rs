pub(crate) mod agents;
pub mod availability;
pub mod client;
pub mod commands;
pub mod completion;
pub mod context;
mod help;
mod installed;
pub mod internal;
pub mod list_status;
pub mod output;
mod progress;
pub(crate) mod terminal;
pub mod ui;
pub mod workspace_context;

use std::{ffi::OsString, path::PathBuf};

use clap::{Args, Parser, Subcommand, ValueEnum};

use crate::{agent::Agent, forge::IssueInput, model::Relation};

#[derive(Debug, Parser)]
#[command(
    version,
    about,
    help_template = help::template(),
    after_help = "Start here (with a registered repository):\n  shoal add my-project fix-login\n  shoal exec fix-login -- cargo test\n  shoal status fix-login\n\nRun `shoal run` to list configured commands.\nUse `shoal <command> --help` for details."
)]
pub struct Cli {
    #[arg(skip)]
    pub(crate) daemon_handoff: Option<crate::daemon::handoff::Handoff>,
    /// What the command needs of its workspace, from its subcommand path.
    #[arg(skip)]
    pub(crate) need: availability::Need,
    /// Override Shoal's state directory (also isolates the daemon).
    #[arg(long, global = true, env = crate::env::STATE_DIR)]
    pub state_dir: Option<PathBuf>,
    /// Emit machine-readable output.
    #[arg(long, global = true)]
    pub json: bool,
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Default, Args)]
pub struct ConfirmationArgs {
    /// Confirm without prompting.
    #[arg(short = 'y', long)]
    pub yes: bool,
}

#[derive(Debug, Default, Args)]
pub struct WorkspaceScope {
    pub workspace: Option<String>,
    /// All workspaces.
    #[arg(long, conflicts_with = "workspace")]
    pub all: bool,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Daemons from earlier releases read the protocol before they update.
    #[command(name = "__build-info", hide = true)]
    BuildInfo,
    /// List or run configured commands.
    Run {
        name: Option<String>,
        workspace: Option<String>,
        #[arg(last = true, requires = "name")]
        args: Vec<OsString>,
    },
    /// Run a configured command or AI tool.
    #[command(external_subcommand)]
    Custom(Vec<OsString>),
    /// Show or install Shoal instructions for agents.
    #[command(args_conflicts_with_subcommands = true)]
    Skill {
        /// Skill to show: worker for agents in a workspace, orchestrator for console agents.
        #[arg(value_enum, default_value_t = SkillName::Worker)]
        name: SkillName,
        #[command(subcommand)]
        command: Option<SkillCommand>,
    },
    /// Generate shell completion scripts.
    Completions {
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },
    /// Print shell integration for Bash or Zsh.
    Shell {
        #[command(subcommand)]
        command: ShellCommand,
    },
    /// Register and manage repositories; omit the subcommand for a menu.
    Repo {
        #[command(subcommand)]
        command: Option<RepoCommand>,
    },
    /// Create or reopen a workspace.
    Add {
        /// Run in the current pane instead of opening a Herdr tab.
        #[arg(long)]
        here: bool,
        /// Give the new tab or session the terminal, overriding the `focus` setting;
        /// its agent starts now, as with --now.
        #[arg(long, conflicts_with = "here")]
        focus: bool,
        /// Registered repository, or an issue, PR or branch URL.
        #[arg(value_parser = item_reference)]
        repository: Option<String>,
        /// Repository for an issue number or a pasted link.
        #[arg(long = "repo")]
        repository_override: Option<String>,
        /// Git branch name; a portable workspace name is derived from it.
        branch: Option<String>,
        /// Use an existing local branch or remote/branch without creating a new branch.
        #[arg(long, conflicts_with_all = ["branch", "issue"])]
        existing: Option<String>,
        /// Derive a name and agent prompt from a forge issue number or URL.
        #[arg(long, value_parser = item_reference)]
        issue: Option<String>,
        /// Create this worktree at an exact new directory instead of the repository default.
        #[arg(long)]
        path: Option<PathBuf>,
        /// Starting Git ref (defaults to the repository's default branch, refreshed from its upstream;
        /// a remote branch such as origin/feature is fetched first);
        /// with --existing, the ref its changes are compared against.
        #[arg(long, value_name = "REF")]
        base: Option<String>,
        /// Apply a named Git profile to the new worktree, overriding repository defaults.
        #[arg(long)]
        git_profile: Option<String>,
        /// Start a built-in agent or a configured command after worktree creation.
        #[arg(long, value_parser = AgentParser)]
        agent: Option<Agent>,
        /// Start the agent now even when max_agents is reached, instead of queuing it.
        #[arg(long)]
        now: bool,
        /// Arguments forwarded to the agent.
        #[arg(last = true)]
        args: Vec<OsString>,
    },
    /// Create a child workspace on a new branch for work in its own PR.
    ///
    /// The parent workspace's agent works on the child and receives its
    /// issue and PR activity; the parent's cleanup waits until its children
    /// are removed.
    Split {
        /// Git branch name; a portable workspace name is derived from it.
        #[arg(required_unless_present = "issue")]
        branch: Option<String>,
        /// Derive the branch name from a forge issue number or URL and link the issue.
        #[arg(long, value_parser = item_reference)]
        issue: Option<String>,
        /// Starting Git ref (defaults to the repository's default branch, refreshed from its
        /// upstream); the parent's branch stacks the child on it.
        #[arg(long, value_name = "REF")]
        base: Option<String>,
        /// Parent workspace; defaults to the current workspace or picker.
        #[arg(long)]
        workspace: Option<String>,
        /// Start an agent of its own in the child, which reports to the parent's agent;
        /// it starts detached, from the agent queue.
        #[arg(long, value_parser = AgentParser)]
        agent: Option<Agent>,
        /// Arguments forwarded to the agent, such as its prompt.
        #[arg(last = true, requires = "agent")]
        args: Vec<OsString>,
    },
    /// Link an issue or PR with a relation: `link 42 assigned`, `link pr 43 related`.
    ///
    /// Assigned items drive completion and cleanup; opened and related items
    /// are recorded only.
    Link {
        #[command(flatten)]
        items: RelationArgs,
    },
    /// Unlink a relation's items, one kind, or a selected issue or PR:
    /// `unlink assigned`, `unlink pr 43 related`.
    Unlink {
        #[command(flatten)]
        items: RelationArgs,
    },
    /// Wait for activity on linked items, one kind, or an explicit item.
    Watch {
        #[command(flatten)]
        items: ItemArgs,
        /// Return no updates when the wait expires (1–3600 seconds); omitted means unbounded.
        #[arg(long, value_parser = clap::value_parser!(u64).range(1..=3600))]
        timeout: Option<u64>,
        /// Continue a wait across a daemon restart, as the installed release
        /// does after an upgrade restarted the daemon.
        #[arg(long, hide = true, requires = "workspace")]
        resume: bool,
    },
    /// Show linked items, one kind, or a selected issue or PR with status and discussion.
    View {
        #[command(flatten)]
        items: ItemArgs,
        /// Leave out comments and reviews.
        #[arg(long)]
        no_comments: bool,
        /// Show only this failed Forgejo Actions check's log.
        #[arg(long, value_name = "NAME")]
        check: Option<String>,
    },
    /// List the repository's labels with their descriptions.
    Labels {
        /// Workspace to use; defaults to the current workspace or picker.
        #[arg(long)]
        workspace: Option<String>,
    },
    /// Mark linked items, or the workspace when none are linked, ready for review at HEAD.
    Ready {
        #[command(flatten)]
        items: ItemArgs,
    },
    /// Withdraw ready-for-review marks from all items, one kind, or a selected issue or PR.
    Unready {
        #[command(flatten)]
        items: ItemArgs,
        /// Return the PRs whose marks are withdrawn to draft, overriding [unready] draft.
        #[arg(long, overrides_with = "no_draft")]
        draft: bool,
        /// Leave the PRs' draft state unchanged, overriding [unready] draft.
        #[arg(long, overrides_with = "draft")]
        no_draft: bool,
    },
    /// Bring an existing worktree under Shoal management, including cleanup.
    Adopt {
        /// Existing worktree root; files and Git settings are preserved, setup is skipped.
        path: PathBuf,
        /// Registered repository; defaults to the one whose checkout or origin remote the worktree shares.
        #[arg(long = "repo")]
        repository: Option<String>,
        /// Copy the work into a new worktree under the repository's Shoal directory.
        #[arg(long)]
        copy: bool,
    },
    /// Rename a workspace's Git branch and derived workspace name.
    #[command(allow_missing_positional = true)]
    Rename {
        workspace: Option<String>,
        /// New literal Git branch name.
        branch: String,
    },
    /// Restore stopped agent sessions and list interrupted commands.
    Resume {
        #[command(flatten)]
        scope: WorkspaceScope,
        /// Select a stopped execution when the workspace has several records.
        #[arg(long, conflicts_with = "all")]
        execution: Option<String>,
        /// Forget the saved agents and commands instead of resuming them.
        #[arg(long)]
        discard: bool,
    },
    /// Attach this terminal to a workspace's agent session; Ctrl-\ detaches.
    ///
    /// Terminal agents run in zmx sessions that keep running after their
    /// terminal closes or detaches.
    Attach {
        workspace: Option<String>,
        /// Session to attach when the workspace has several.
        #[arg(long)]
        session: Option<String>,
    },
    /// Run or retry workspace setup.
    Setup { workspace: Option<String> },
    /// Send the user a notification about a workspace without marking it done.
    Notify {
        /// One line of text, such as "PR #12 is ready to merge".
        message: String,
        /// Workspace to use; defaults to the current workspace or picker.
        #[arg(long)]
        workspace: Option<String>,
    },
    /// Queue a message for the agents working in a workspace.
    Message {
        /// One line of text, such as "Stop the dev server before running tests".
        message: String,
        /// Workspace to use; defaults to the current workspace or picker.
        #[arg(long)]
        workspace: Option<String>,
    },
    /// Show the new agent messages of the workspace, or of the chat it runs in, once.
    Messages {
        workspace: Option<String>,
        /// Run as an agent hook: read the hook event on stdin, print new
        /// messages as additional context, and never fail the agent.
        #[arg(long, conflicts_with = "workspace")]
        hook: bool,
    },
    /// Ask the user to decide something; the answer arrives as a message.
    Ask {
        /// The decision, in one line, such as "Which database should the cache use?"
        question: String,
        /// The answer you recommend.
        #[arg(long)]
        recommend: String,
        /// Why you recommend it.
        #[arg(long)]
        reason: Option<String>,
        /// A possible answer; repeat for each. The user may answer with its number.
        #[arg(long = "choice", value_name = "CHOICE")]
        choices: Vec<String>,
        /// Wait for the answer and print it. Asking again waits for the same question.
        #[arg(long)]
        wait: bool,
        /// Workspace to use; defaults to the current workspace or picker.
        #[arg(long)]
        workspace: Option<String>,
        /// Continue waiting for this question, as the installed release does
        /// after an upgrade restarted the daemon.
        #[arg(long, hide = true, requires = "workspace")]
        resume: Option<i64>,
    },
    /// List open questions from workspace agents.
    Questions { workspace: Option<String> },
    /// Answer a question from a workspace's agents.
    Answer {
        /// Question number; omit to choose an open question.
        id: Option<i64>,
        /// The answer, or a choice number; omit to be prompted.
        answer: Option<String>,
    },
    /// Mark the assignment finished; by default stop tracked commands and clean up safely.
    Done {
        workspace: Option<String>,
        /// Request cleanup even when [done] cleanup is false; preserve dirty or unpushed work.
        #[arg(long)]
        cleanup: bool,
    },
    /// Withdraw a recorded completion without keeping the workspace.
    Undone { workspace: Option<String> },
    /// Hold a workspace against automatic cleanup.
    #[command(args_conflicts_with_subcommands = true)]
    Hold {
        #[command(subcommand)]
        command: Option<HoldCommand>,
        #[command(flatten)]
        scope: WorkspaceScope,
    },
    /// Show, set or clear the workspace a stacked workspace builds on.
    #[command(args_conflicts_with_subcommands = true)]
    Base {
        #[command(subcommand)]
        command: Option<BaseCommand>,
        workspace: Option<String>,
    },
    /// List what needs you: questions, approvals, agents needing input, failures and ready work.
    Inbox {
        /// List only this registered repository's items.
        #[arg(long = "repo")]
        repository: Option<String>,
        /// Print the list instead of opening the picker, as filters and JSON do.
        #[arg(long)]
        print: bool,
    },
    /// List the work going on: workspaces, chats and queued agents.
    Work {
        /// List only the agents waiting for a slot under max_agents.
        #[arg(long, conflicts_with = "chats")]
        queue: bool,
        /// List only the running chats.
        #[arg(long)]
        chats: bool,
        /// List only this registered repository's workspaces, chats and agents.
        #[arg(long = "repo")]
        repository: Option<String>,
        /// List only workspaces, chats and agents with this status; repeat or separate with commas.
        #[arg(long, value_delimiter = ',')]
        status: Vec<list_status::ListStatus>,
        /// Print the list instead of opening the picker, as filters and JSON do.
        #[arg(long)]
        print: bool,
    },
    /// Show workspace activity, changes, resources, and issue and PR state.
    ///
    /// Name a workspace, or find the workspaces that link an issue or PR or
    /// hold a resource: `status pr 12`, `status <issue-or-PR-URL>`, `status
    /// resource <pool-or-member>`.
    Status {
        /// Workspace, issue or PR URL, or pr, issue or resource.
        #[arg(value_name = "TARGET")]
        workspace: Option<String>,
        /// Number, URL, or resource name after pr, issue or resource.
        #[arg(value_parser = item_reference)]
        item: Option<String>,
    },
    /// Enter a workspace, or use - for the previous directory.
    ///
    /// Omit the workspace to open the picker, even inside a workspace.
    Cd { workspace: Option<String> },
    /// Open a workspace in an editor.
    ///
    /// Runs the configured `edit` command, otherwise `$VISUAL` or `$EDITOR` with the workspace path.
    Edit {
        workspace: Option<String>,
        /// Arguments forwarded to the editor.
        #[arg(last = true)]
        args: Vec<OsString>,
    },
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
    /// Fetch a repository's remote and fast-forward its default branch.
    ///
    /// Never pushes or changes workspace branches; rebase or merge onto the
    /// updated branch with Git.
    Sync {
        /// Registered repository; defaults to the current checkout or workspace.
        repository: Option<String>,
    },
    /// Open the workspace's PR, or change, comment on, close, reopen or merge a PR.
    Pr {
        #[command(subcommand)]
        command: PrCommand,
    },
    /// Rerun failed or cancel running CI jobs of a PR.
    Ci {
        #[command(subcommand)]
        command: CiCommand,
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
    /// Commands Shoal, its agent hooks and integrations run, not part of the
    /// interface for people.
    #[command(name = internal::GROUP, hide = true)]
    Internal {
        #[command(subcommand)]
        command: InternalCommand,
    },
    /// Acquire a port, simulator, resource permit, or related repository.
    Acquire {
        #[command(subcommand)]
        kind: AcquireKind,
        /// Workspace to use; defaults to the current workspace or picker.
        #[arg(long, global = true)]
        workspace: Option<String>,
    },
    /// Release every lease, every lease of one kind, or a selected lease.
    Release {
        #[command(subcommand)]
        kind: Option<ReleaseKind>,
        /// Workspace to use; defaults to the current workspace or picker.
        #[arg(long, global = true)]
        workspace: Option<String>,
    },
    /// Show capacity and leases for every kind, or one.
    Leases {
        kind: Option<LeaseKind>,
        /// Workspace to show; defaults to the current workspace or picker.
        #[arg(long)]
        workspace: Option<String>,
        /// Show every managed workspace.
        #[arg(long, conflicts_with = "workspace")]
        all: bool,
    },
    /// Reserve and release workspace TCP ports; superseded by acquire, release, and leases.
    #[command(
        hide = true,
        args_conflicts_with_subcommands = true,
        mut_arg("workspace", |arg| arg.help("Show the effective configuration and reservations for this workspace")),
        mut_arg("all", |arg| arg.help("Show every managed workspace"))
    )]
    Port {
        #[command(subcommand)]
        command: Option<PortCommand>,
        #[command(flatten)]
        scope: WorkspaceScope,
    },
    /// List approval requests and retained grants (scoped callers see their own workspace).
    Approvals { workspace: Option<String> },
    /// Approve the exact settings recorded in a request; does not allocate
    /// capacity. Approving an issue or PR proposal opens it.
    Approve {
        /// Request number; omit to choose a pending request.
        id: Option<i64>,
        /// Replace a proposed issue's or PR's title.
        #[arg(long)]
        title: Option<String>,
        /// Replace a proposed issue's or PR's description.
        #[command(flatten)]
        body: BodyArgs,
    },
    /// Deny a pending approval request.
    Deny {
        /// Request number; omit to choose a pending request.
        id: Option<i64>,
        /// Tell the agent why its proposal was denied.
        #[arg(long)]
        reason: Option<String>,
        /// Deny an issue proposal as a duplicate of this issue number or URL
        /// and add its text there as a comment.
        #[arg(long, value_name = "ISSUE", conflicts_with = "duplicate_of_approval")]
        duplicate_of: Option<String>,
        /// Deny an issue proposal as a duplicate of another pending one, which
        /// adds its text as a comment once opened.
        #[arg(long, value_name = "NUMBER")]
        duplicate_of_approval: Option<i64>,
    },
    /// Pass an approval request routed to this workspace's agent to the next
    /// coordinating agent that holds a grant for it, or to the user.
    Escalate {
        /// Request number.
        id: i64,
    },
    /// Acquire and release shared resource permits; superseded by acquire, release, and leases.
    #[command(
        hide = true,
        args_conflicts_with_subcommands = true,
        mut_arg("workspace", |arg| arg.help("Show effective capacity and leases for this workspace")),
        mut_arg("all", |arg| arg.help("Show every managed workspace"))
    )]
    Resource {
        #[command(subcommand)]
        command: Option<ResourceCommand>,
        #[command(flatten)]
        scope: WorkspaceScope,
    },
    /// Show the simulator catalog and clean-device history.
    #[command(
        args_conflicts_with_subcommands = true,
        mut_arg("workspace", |arg| arg.help("Show configured profiles, capacity, and devices for this workspace")),
        mut_arg("all", |arg| arg.help("Show every managed device"))
    )]
    Sim {
        #[command(subcommand)]
        command: Option<SimCommand>,
        #[command(flatten)]
        scope: WorkspaceScope,
    },
    /// Export the environment for processes started in a workspace.
    Env {
        workspace: Option<String>,
        /// Revoke a previously exported scope token.
        #[arg(long)]
        revoke: Option<String>,
    },
    /// Show detailed workspace and execution records.
    Inspect { workspace: Option<String> },
    /// Show resource conflicts, finished agents, and automatic cleanup.
    #[command(args_conflicts_with_subcommands = true)]
    Notifications {
        #[command(subcommand)]
        command: Option<NotificationsCommand>,
        /// Include notifications already shown.
        #[arg(long, conflicts_with = "follow")]
        all: bool,
        /// Keep printing new notifications as the daemon records them.
        #[arg(long)]
        follow: bool,
        /// Show notifications without marking them read.
        #[arg(long)]
        keep_unread: bool,
        /// Continue following after this notification, as the installed
        /// release does after an upgrade restarted the daemon.
        #[arg(long, hide = true, requires = "follow")]
        resume: Option<i64>,
        #[arg(long, default_value_t = 50, value_parser = clap::value_parser!(u32).range(1..=200))]
        limit: u32,
    },
    /// Diagnose Shoal and its workspaces; optionally repair verified state.
    Doctor {
        #[command(flatten)]
        scope: WorkspaceScope,
        /// Apply safe state repairs; preserve files, branches, and resource leases.
        #[arg(long)]
        repair: bool,
        /// Stop connected commands and identity-verified surviving processes.
        #[arg(long, requires = "repair")]
        stop: bool,
        /// Confirm untracked/legacy processes have stopped; visible survivors still block repair.
        #[arg(long, requires = "repair")]
        acknowledge_stopped: bool,
        /// Confirm the worktree at the recorded path is this workspace's; re-establish its ownership.
        #[arg(long, requires = "repair")]
        reclaim: bool,
    },
    /// Stop tracked agents and commands, saving them for `shoal resume`; keep the workspace.
    Stop {
        #[command(flatten)]
        scope: WorkspaceScope,
    },
    /// Remove a workspace and release its resources.
    ///
    /// The branch goes too when a remote or the default branch has all its commits; otherwise it
    /// stays. Deleting uncommitted changes or commits on a detached HEAD needs confirmation.
    Rm {
        workspace: Option<String>,
        #[command(flatten)]
        confirmation: ConfirmationArgs,
    },
    /// Remove every workspace idle or merged cleanup would remove, without waiting for their idle periods.
    ///
    /// Applies where those rules are disabled; held, busy, unpushed or unmerged dirty workspaces are
    /// kept, and the unsaved files of merged ones go into their checkpoints.
    Cleanup {
        /// List the workspaces without removing them.
        #[arg(long)]
        dry_run: bool,
        /// Also empty the shared caches of repositories without running executions.
        #[arg(long)]
        caches: bool,
    },
    /// List or delete the branches Shoal kept when it removed their workspaces.
    ///
    /// Removal keeps a branch only while some of its commits are on no remote and not on the
    /// default branch, and deletes it once they are. Without a subcommand, lists them.
    Branch {
        #[command(subcommand)]
        command: Option<BranchCommand>,
    },
    /// Keep a workspace's HEAD and unsaved files in a checkpoint that shoal restore returns to.
    Checkpoint {
        workspace: Option<String>,
        /// Describe the checkpoint in shoal restore's list, in at most 200 characters.
        #[arg(long)]
        name: Option<String>,
    },
    /// Return a workspace to a checkpoint, or open a removed workspace again with its unsaved files and links.
    ///
    /// Without a workspace, picks a removed workspace to open again. With an existing workspace,
    /// picks one of its checkpoints; with a checkpoint number as well, returns to it after
    /// keeping the current state in a new checkpoint. Without a terminal, the choices are listed.
    Restore {
        workspace: Option<String>,
        #[arg(requires = "workspace")]
        checkpoint: Option<u64>,
    },
    /// Remove a workspace and write it to a file that shoal import opens on another machine.
    ///
    /// The file holds the removal's checkpoint, as a Git bundle without the commits of the
    /// default branch, and the workspace's base, links and stopped agents. A removed workspace
    /// is exported without another removal. `-` writes to standard output.
    Export {
        workspace: Option<String>,
        #[arg(short, long)]
        output: PathBuf,
        #[command(flatten)]
        confirmation: ConfirmationArgs,
    },
    /// Open a workspace shoal export wrote, in the repository with the same origin.
    ///
    /// Fetches the default branch when the file builds on commits the repository lacks. Stopped
    /// agents start new sessions with shoal resume, since their sessions stayed on the other
    /// machine. `-` reads from standard input.
    Import {
        file: PathBuf,
        /// Repository to import into, when it does not share the exported repository's origin.
        #[arg(long = "repo")]
        repository: Option<String>,
    },
    /// Run an arbitrary command in a workspace.
    Exec {
        workspace: Option<String>,
        #[arg(last = true, required = true)]
        command: Vec<OsString>,
    },
    /// Run Claude Code in a workspace.
    Claude {
        workspace: Option<String>,
        #[arg(last = true)]
        args: Vec<OsString>,
    },
    /// Run Codex CLI or open the Codex app.
    Codex {
        workspace: Option<String>,
        /// Run the Codex CLI, overriding codex.default_mode.
        #[arg(long, conflicts_with = "app")]
        cli: bool,
        /// Open the workspace in the Codex app, overriding codex.default_mode.
        #[arg(long, conflicts_with = "cli")]
        app: bool,
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
    /// a zmx session. Chats are not tracked by the daemon.
    Chat {
        /// Run in the current pane instead of opening a Herdr tab.
        #[arg(long)]
        here: bool,
        /// Registered repository; defaults to the current checkout or workspace, else a picker.
        repository: Option<String>,
        /// Agent to start, one with an instructions template such as claude or codex; defaults to default_agent, otherwise a picker.
        #[arg(long, value_parser = AgentParser)]
        agent: Option<Agent>,
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
    /// Show and change Shoal configuration.
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
    /// Install and start the per-user daemon service.
    Install {
        /// Preview the service definition without changing anything.
        #[arg(long)]
        dry_run: bool,
        /// Executable path to register (preserves symlinks for package upgrades).
        #[arg(long)]
        executable: Option<PathBuf>,
    },
    /// Replace a downloaded shoal with the latest GitHub release.
    Update,
    /// Inspect and control the local daemon.
    Daemon {
        #[command(subcommand)]
        command: DaemonCommand,
    },
}

#[derive(Debug, Subcommand)]
pub enum PrCommand {
    /// List or search the workspace repository's PRs, newest first.
    List(ListArgs),
    /// Push the workspace branch, open a PR for it or find its open PR, and link it.
    Open {
        /// Workspace to use; defaults to the current workspace or picker.
        #[arg(long)]
        workspace: Option<String>,
        /// PR title; defaults to the linked issue's title, then the last commit's subject.
        #[arg(long)]
        title: Option<String>,
        #[command(flatten)]
        body: BodyArgs,
        #[command(flatten)]
        files: AttachArgs,
        /// Base branch; defaults to the base workspace's branch, then the default branch.
        #[arg(long)]
        base: Option<String>,
        /// Open the PR as a draft, overriding [pr] draft.
        #[arg(long, overrides_with = "no_draft")]
        draft: bool,
        /// Open the PR ready for review, overriding [pr] draft.
        #[arg(long, overrides_with = "draft")]
        no_draft: bool,
        /// Add a label; repeat for more.
        #[arg(long = "label", value_name = "LABEL")]
        labels: Vec<String>,
        /// Request a review; repeat for more.
        #[arg(long = "reviewer", value_name = "LOGIN")]
        reviewers: Vec<String>,
    },
    /// Change a PR's title, description, base, labels, reviewers or draft state.
    Edit {
        #[command(flatten)]
        item: ItemSelector,
        #[command(flatten)]
        edit: EditArgs,
        /// New base branch.
        #[arg(long)]
        base: Option<String>,
        /// Request a review; repeat for more.
        #[arg(long = "add-reviewer", value_name = "LOGIN")]
        add_reviewers: Vec<String>,
        /// Withdraw a review request; repeat for more.
        #[arg(long = "remove-reviewer", value_name = "LOGIN")]
        remove_reviewers: Vec<String>,
        /// Mark the PR as a draft.
        #[arg(long, conflicts_with = "no_draft")]
        draft: bool,
        /// Mark the PR ready for review.
        #[arg(long)]
        no_draft: bool,
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

/// A PR and the checks on its head to act on.
#[derive(Debug, Args)]
pub struct CiChecks {
    #[command(flatten)]
    pub item: ItemSelector,
    /// Act only on this check, as `shoal view` names it; repeat for more. Defaults to every failed or running check.
    #[arg(long = "check", value_name = "NAME")]
    pub checks: Vec<String>,
}

#[derive(Debug, Subcommand)]
pub enum IssueCommand {
    /// List or search the workspace repository's issues, newest first.
    List(ListArgs),
    /// Open an issue in the workspace's repository.
    Open {
        /// Workspace to use; defaults to the current workspace or picker.
        #[arg(long)]
        workspace: Option<String>,
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

/// Which issues or PRs of the workspace's repository to list.
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

/// An issue or PR of the workspace's repository.
#[derive(Debug, Args)]
pub struct ItemSelector {
    /// Number or URL; defaults to the linked one.
    #[arg(value_parser = item_reference)]
    pub item: Option<String>,
    /// Workspace to use; defaults to the current workspace or picker.
    #[arg(long)]
    pub workspace: Option<String>,
}

/// Title, description and label changes either kind of item takes.
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

/// A description or comment, given inline or read from a file.
#[derive(Debug, Args)]
pub struct BodyArgs {
    /// Text of the description or comment.
    #[arg(long)]
    pub body: Option<String>,
    /// Read the text from a file.
    #[arg(long, value_name = "PATH", conflicts_with = "body")]
    pub body_file: Option<PathBuf>,
}

/// Image and video files to show in a description or comment.
#[derive(Debug, Args)]
pub struct AttachArgs {
    /// Upload an image or video, as PATH or PATH#ALT_TEXT; repeat for more. It
    /// replaces the body's image references to the file, such as
    /// `![alt](shot.png)` relative to the body file or working directory, and
    /// otherwise follows the body.
    #[arg(long = "attach", value_name = "FILE")]
    pub attach: Vec<String>,
}

/// An optional item selection followed by its relation.
#[derive(Debug, Args)]
pub struct RelationArgs {
    /// pr/issue, a number or URL, then assigned, opened or related.
    #[arg(required = true, num_args = 1..=3, value_name = "[ITEM] RELATION")]
    pub args: Vec<String>,
    /// Workspace to use; defaults to the current workspace or picker.
    #[arg(long)]
    pub workspace: Option<String>,
}

impl RelationArgs {
    /// Split the trailing relation from the item selection before it.
    pub fn split(self) -> anyhow::Result<(ItemArgs, Relation)> {
        let mut args = self.args;
        let last = args.pop().unwrap_or_default();
        let relation = Relation::from_str(&last, false).map_err(|_| {
            anyhow::anyhow!("end with a relation: assigned, opened or related (got {last})")
        })?;
        let mut items = args
            .into_iter()
            .map(|arg| item_reference(&arg).unwrap_or_else(|never| match never {}));
        let items = ItemArgs {
            kind_or_url: items.next(),
            item: items.next(),
            workspace: self.workspace,
        };
        Ok((items, relation))
    }
}

#[derive(Debug, Args)]
pub struct ItemArgs {
    /// pr/issue to select a kind, or an issue or PR number or URL.
    #[arg(value_parser = item_reference)]
    pub kind_or_url: Option<String>,
    /// Number or URL when a kind is supplied.
    #[arg(requires = "kind_or_url", value_parser = item_reference)]
    pub item: Option<String>,
    /// Workspace to use; defaults to the current workspace or picker.
    #[arg(long)]
    pub workspace: Option<String>,
}

#[derive(Debug, Subcommand)]
pub enum NotificationsCommand {
    /// Delete all notifications, including unread ones.
    Clear,
}

#[derive(Debug, Subcommand)]
pub enum BaseCommand {
    /// Record the workspace whose branch this workspace builds on.
    Set {
        /// Workspace name or branch in the same repository; omit to choose one.
        base: Option<String>,
        #[arg(long)]
        workspace: Option<String>,
    },
    /// Remove the recorded base workspace.
    Clear {
        #[arg(long)]
        workspace: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
pub enum HoldCommand {
    /// Keep a workspace against automatic cleanup until the hold is released.
    Acquire {
        workspace: Option<String>,
        #[arg(long)]
        name: String,
        #[arg(long)]
        reason: Option<String>,
    },
    /// Release one named hold.
    Release {
        workspace: Option<String>,
        /// Hold to release; omit to choose one.
        #[arg(long)]
        name: Option<String>,
    },
    /// List holds on a workspace, or every workspace with --all.
    List {
        #[command(flatten)]
        scope: WorkspaceScope,
    },
}

impl Cli {
    /// Parse the process arguments as `Cli::parse` does, recording what the
    /// command needs of its workspace.
    pub fn from_args() -> Self {
        let mut matches = <Self as clap::CommandFactory>::command().get_matches();
        let mut path = vec![];
        let mut leaf = &matches;
        while let Some((name, child)) = leaf.subcommand() {
            path.push(name.to_owned());
            leaf = child;
        }
        let mut cli = <Self as clap::FromArgMatches>::from_arg_matches_mut(&mut matches)
            .unwrap_or_else(|error| {
                error
                    .format(&mut <Self as clap::CommandFactory>::command())
                    .exit()
            });
        let path: Vec<_> = path.iter().map(String::as_str).collect();
        cli.need = availability::Need::of_command(&path);
        cli
    }
}

impl Command {
    /// Lifecycle and service administration is denied to scoped workspace
    /// processes; everything else is ordinary workspace work.
    pub fn is_administrative(&self) -> bool {
        matches!(
            self,
            Command::Doctor { .. }
                | Command::Branch { .. }
                | Command::Install { .. }
                | Command::Update
                | Command::Config {
                    command: ConfigCommand::Install { .. }
                        | ConfigCommand::Reset
                        | ConfigCommand::Set { .. }
                        | ConfigCommand::Unset { .. }
                }
                | Command::Daemon {
                    command: DaemonCommand::Run { .. }
                        | DaemonCommand::Start
                        | DaemonCommand::Stop
                        | DaemonCommand::Restart
                        | DaemonCommand::Reload
                }
        )
    }
}

/// An issue or PR argument, where `#42` means `42`.
fn item_reference(input: &str) -> Result<String, std::convert::Infallible> {
    Ok(match input.strip_prefix('#') {
        Some(number) if IssueInput::parse(number) == IssueInput::Number => number.to_owned(),
        _ => input.to_owned(),
    })
}

/// clap parser for [`Agent`] that also advertises its values for completion.
#[derive(Clone)]
pub struct AgentParser;

impl clap::builder::TypedValueParser for AgentParser {
    type Value = Agent;

    fn parse_ref(
        &self,
        cmd: &clap::Command,
        arg: Option<&clap::Arg>,
        value: &std::ffi::OsStr,
    ) -> Result<Agent, clap::Error> {
        let value = value
            .to_str()
            .ok_or_else(|| clap::Error::new(clap::error::ErrorKind::InvalidUtf8).with_cmd(cmd))?;
        value.parse().map_err(|_| {
            let mut error = clap::Error::new(clap::error::ErrorKind::InvalidValue).with_cmd(cmd);
            if let Some(arg) = arg {
                error.insert(
                    clap::error::ContextKind::InvalidArg,
                    clap::error::ContextValue::String(arg.to_string()),
                );
            }
            error.insert(
                clap::error::ContextKind::InvalidValue,
                clap::error::ContextValue::String(value.to_owned()),
            );
            error.insert(
                clap::error::ContextKind::ValidValue,
                clap::error::ContextValue::Strings(agents::builtin_names()),
            );
            error
        })
    }

    fn possible_values(
        &self,
    ) -> Option<Box<dyn Iterator<Item = clap::builder::PossibleValue> + '_>> {
        Some(Box::new(
            agents::builtin_names()
                .into_iter()
                .map(clap::builder::PossibleValue::new),
        ))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum SkillName {
    Worker,
    Orchestrator,
}

#[derive(Debug, Subcommand)]
pub enum SkillCommand {
    /// Install or refresh the bundled skills for configured AI tools (no daemon needed).
    Install {
        /// Install for one AI tool, or all configured tools by default.
        #[arg(default_value = "all")]
        agent: String,
        /// Replace skills changed since Shoal installed them without asking.
        #[arg(long)]
        force: bool,
    },
}

#[derive(Debug, Subcommand)]
pub enum BranchCommand {
    /// List kept branches and how many commits only they have.
    Ls,
    /// Delete kept branches and the commits only they have.
    ///
    /// Without branch names, choose them in a picker, where Tab selects several.
    Rm {
        branches: Vec<String>,
        /// Delete every kept branch.
        #[arg(long, conflicts_with = "branches")]
        all: bool,
        /// Only branches of this repository, by name or ID.
        #[arg(long)]
        repo: Option<String>,
        #[command(flatten)]
        confirmation: ConfirmationArgs,
    },
}

#[derive(Debug, Subcommand)]
pub enum RepoCommand {
    /// Show or replace repository configuration stored locally by Shoal.
    Config {
        /// Registered repository; omit to choose one.
        repository: Option<String>,
        /// Import TOML as the complete config for all this repository's workspaces.
        #[arg(long, conflicts_with = "clear", value_hint = clap::ValueHint::FilePath)]
        file: Option<PathBuf>,
        /// Remove the local config and use each worktree's repository config again.
        #[arg(long)]
        clear: bool,
    },
    /// Register a local checkout (no remote required), or clone a repository URL.
    Add {
        source: String,
        #[arg(long)]
        name: Option<String>,
        /// Clone this URL into this exact directory instead of root_dir/<name>/.checkout.
        #[arg(long)]
        path: Option<PathBuf>,
    },
    Rename {
        /// Registered repository; omit to choose one.
        repository: Option<String>,
        /// New name; omit to enter one.
        name: Option<String>,
    },
    /// Have an agent write the repository's Shoal config in a shoal-setup workspace.
    Setup {
        /// Registered repository; omit to choose one.
        repository: Option<String>,
        /// Agent to start; defaults to default_agent, otherwise a picker.
        #[arg(long, value_parser = AgentParser)]
        agent: Option<Agent>,
        /// Arguments forwarded to the agent.
        #[arg(last = true)]
        args: Vec<OsString>,
    },
    /// Delete a repository checkout and all its Shoal workspaces and resources.
    ///
    /// Uncommitted and unpushed work is permanently lost.
    #[command(alias = "remove")]
    Rm {
        /// Registered repository; omit to choose one.
        repository: Option<String>,
        #[command(flatten)]
        confirmation: ConfirmationArgs,
    },
    List,
}

#[derive(Debug, Subcommand)]
pub enum PortCommand {
    /// Acquire a port, or return the existing reservation with this name.
    Acquire {
        name: String,
        workspace: Option<String>,
        #[arg(long)]
        port: Option<u16>,
        /// Environment variable exported to subsequent exec/claude/codex commands.
        #[arg(long)]
        env: Option<String>,
        /// Explain what the reservation is used for.
        #[arg(long)]
        reason: Option<String>,
        /// Override the repo's conflict behavior (default: suggest).
        #[arg(long, value_enum)]
        on_conflict: Option<crate::config::repo::ConflictPolicy>,
    },
    /// Show configured ports and current reservations.
    List {
        #[command(flatten)]
        scope: WorkspaceScope,
    },
    Release {
        name: String,
        workspace: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
pub enum InternalCommand {
    /// Stream durable workspace lifecycle events without consuming notifications.
    Events {
        /// Keep the stream open as the daemon records new events.
        #[arg(long)]
        follow: bool,
        /// Replay events after this event ID.
        #[arg(long, value_parser = clap::value_parser!(i64).range(0..))]
        since: Option<i64>,
    },
    /// Print a view's rows for the bare `shoal` picker to reload.
    MenuRows { view: commands::menu::View },
    /// Print the fzf actions that switch the bare `shoal` picker to its next view.
    MenuNext,
    /// Report the agent's turn state; Shoal's agent hooks call it.
    AgentState {
        state: crate::state::AgentState,
        /// Workspace to use; defaults to the current workspace or picker.
        #[arg(long)]
        workspace: Option<String>,
    },
    /// Wait for the next agent messages or item activity of the workspace or
    /// chat, print them and exit 2; Shoal's Claude Code `Stop` hook runs it in the
    /// background so the update starts the idle agent's next turn.
    Wake {
        /// The agent's process ID; the hook ends when that process exits.
        #[arg(long)]
        agent: u32,
    },
    /// Finish a planned landing inside its tracked execution.
    #[command(name = internal::LAND)]
    Land {
        #[arg(long)]
        push: bool,
        plan: String,
    },
    /// Add the planned workspace inside its Herdr tab.
    #[command(name = internal::HERDR)]
    Herdr {
        #[arg(long)]
        close_when_done: bool,
        #[arg(env = crate::env::ADD_PLAN, hide_env_values = true)]
        plan: String,
    },
    /// Close a Herdr tab once its workspace completes or is removed.
    #[command(name = internal::HERDR_WATCH)]
    HerdrWatch { workspace: String, tab: String },
    /// Detached execution wrapper; reports the launch on stdout, then keeps tracking it.
    #[command(name = internal::DETACHED)]
    Detached {
        workspace: String,
        #[arg(long)]
        log: PathBuf,
        #[arg(long)]
        agent: Option<String>,
        #[arg(last = true, required = true)]
        command: Vec<OsString>,
    },
    /// Run a tool with its configured secrets; an agent's secret launchers call it.
    #[command(name = internal::SECRET_TOOL)]
    SecretTool {
        launchers: PathBuf,
        tool: String,
        #[arg(last = true)]
        args: Vec<OsString>,
    },
    /// Start an admitted agent queue entry; the daemon runs it.
    #[command(name = internal::QUEUE_START)]
    QueueStart { entry: String },
    /// Add the planned workspace inside its detached zmx session.
    #[command(name = internal::ADD)]
    Add {
        #[arg(env = crate::env::ADD_PLAN, hide_env_values = true)]
        plan: String,
    },
    /// Tracked agent inside a zmx session; consumes recovery records once started.
    #[command(name = internal::SESSION)]
    Session {
        workspace: String,
        agent: String,
        #[arg(long = "record")]
        records: Vec<PathBuf>,
        #[arg(last = true, required = true)]
        command: Vec<OsString>,
    },
    /// Copy the main checkout's ignored files inside tracked setup, then run setup.
    #[command(name = internal::COPY_IGNORED)]
    CopyIgnored {
        #[arg(last = true)]
        setup_cmd: Option<PathBuf>,
    },
}

#[derive(Debug, Subcommand)]
pub enum ShellCommand {
    Init,
    #[command(hide = true)]
    Recover {
        path: PathBuf,
    },
}

#[derive(Debug, Subcommand)]
pub enum ConfigCommand {
    /// Set config values using TOML dotted keys (global by default), saved together.
    Set {
        /// KEY VALUE pairs; each value is TOML, or an unquoted string.
        #[arg(required = true, num_args = 2.., value_names = ["KEY", "VALUE"])]
        assignments: Vec<String>,
        /// Edit this repository's saved config instead of the global file.
        #[arg(long = "repo")]
        repository: Option<String>,
    },
    /// Remove config keys or tables, falling back to lower layers or defaults.
    Unset {
        #[arg(required = true)]
        keys: Vec<String>,
        /// Edit this repository's saved config instead of the global file.
        #[arg(long = "repo")]
        repository: Option<String>,
    },
    /// Show effective repository settings and the layer each value came from.
    Show { workspace: Option<String> },
    /// Install a packaged global config, keeping the old file as config.toml.backup.
    Install {
        /// Packaged config; omit to choose one.
        #[arg(value_parser = clap::builder::PossibleValuesParser::new(
            crate::config::PACKAGED.iter().map(|(name, _)| *name)
        ))]
        name: Option<String>,
    },
    /// Rewrite the config with the defaults; the old file becomes config.toml.backup.
    Reset,
}

#[derive(Debug, Subcommand)]
pub enum DaemonCommand {
    Status,
    Start,
    Stop,
    Restart,
    /// Reread the global config without stopping running agents.
    Reload,
    /// Run in the foreground, without registering an OS service.
    Run {
        #[arg(long, hide = true)]
        managed: bool,
    },
}

#[derive(Debug, Subcommand)]
pub enum SimCommand {
    /// Show available device types, installed runtimes, and machine profiles.
    Catalog,
    /// Show configured profiles, capacity, and managed instances.
    #[command(hide = true)]
    List {
        #[command(flatten)]
        scope: WorkspaceScope,
    },
    /// Acquire exclusive use; reuse the same named lease on repeated requests.
    #[command(hide = true)]
    Acquire {
        workspace: Option<String>,
        #[arg(long, default_value = "default")]
        name: String,
        #[arg(long, conflicts_with_all = ["device", "runtime"])]
        profile: Option<String>,
        #[arg(long)]
        device: Option<String>,
        /// Installed runtime; defaults to the latest compatible iOS runtime.
        #[arg(long, requires = "device")]
        runtime: Option<String>,
        #[arg(long)]
        reason: Option<String>,
        /// Require a fresh or erased device; a reason is mandatory and audited.
        #[arg(long, requires = "reason")]
        clean: bool,
        /// Queue for capacity and wait until acquired or denied.
        #[arg(long)]
        queue: bool,
    },
    /// Review clean-device requests, including failed and busy requests.
    History {
        #[command(flatten)]
        scope: WorkspaceScope,
        #[arg(long, default_value_t = 20, value_parser = clap::value_parser!(u32).range(1..=50))]
        limit: u32,
        /// Show entries older than this audit ID.
        #[arg(long)]
        before: Option<i64>,
    },
    /// End exclusive use; the idle policy controls shutdown and deletion.
    #[command(hide = true)]
    Release {
        #[arg(default_value = "default")]
        name: String,
        workspace: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum LeaseKind {
    Port,
    Sim,
    Resource,
    Repo,
}

/// Options shared by leases that a workspace may hold several of.
#[derive(Debug, Args)]
pub struct LeaseOptions {
    /// Stable lease name; use different names to hold several leases.
    #[arg(long, default_value = "default")]
    pub name: String,
    /// Explain what the lease is used for.
    #[arg(long)]
    pub reason: Option<String>,
    /// Queue for capacity and wait until acquired or denied.
    #[arg(long)]
    pub queue: bool,
}

#[derive(Debug, Subcommand)]
pub enum AcquireKind {
    /// Reserve a named TCP port, or return the existing reservation.
    Port {
        /// Port name; omit to choose a configured port.
        name: Option<String>,
        #[arg(long)]
        port: Option<u16>,
        /// Environment variable exported to subsequent exec/claude/codex commands.
        #[arg(long)]
        env: Option<String>,
        /// Explain what the reservation is used for.
        #[arg(long)]
        reason: Option<String>,
        /// Override the repo's conflict behavior (default: suggest).
        #[arg(long, value_enum)]
        on_conflict: Option<crate::config::repo::ConflictPolicy>,
    },
    /// Lease an Xcode simulator; repeated requests return the same named lease.
    Sim {
        #[command(flatten)]
        lease: LeaseOptions,
        /// Configured simulator profile.
        #[arg(long, conflicts_with_all = ["device", "runtime"])]
        profile: Option<String>,
        /// Device type name or identifier.
        #[arg(long)]
        device: Option<String>,
        /// Installed runtime; defaults to the latest compatible iOS runtime.
        #[arg(long, requires = "device")]
        runtime: Option<String>,
        /// Require a fresh or erased device; a reason is mandatory and audited.
        #[arg(long, requires = "reason")]
        clean: bool,
    },
    /// Acquire a permit or reader/writer lock from a configured pool.
    Resource {
        /// Configured resource or pool; omit to choose one.
        pool: Option<String>,
        /// Pool member to acquire instead of any available one.
        #[arg(long)]
        member: Option<String>,
        /// Lock mode (defaults to permit for semaphores, write for rwlocks).
        #[arg(long, value_enum)]
        mode: Option<crate::daemon::resources::LockMode>,
        #[command(flatten)]
        lease: LeaseOptions,
    },
    /// Borrow a configured related repository's checkout for reading.
    Repo {
        /// Configured `kind = "repo"` resource name; omit to choose one.
        #[arg(value_name = "NAME")]
        resource: Option<String>,
        #[command(flatten)]
        lease: LeaseOptions,
    },
    /// Request access to a configured secret that requires approval.
    Secret {
        /// Variable name of the secret in `[secrets]`.
        name: String,
        /// Explain what the secret is needed for.
        #[arg(long)]
        reason: Option<String>,
        /// Wait until approved or denied.
        #[arg(long)]
        queue: bool,
    },
}

/// A pool, optionally narrowed to one named lease.
#[derive(Debug, Args)]
pub struct PoolSelection {
    /// Configured resource or pool; omit to release every lease of this kind.
    #[arg(value_name = "NAME")]
    pub pool: Option<String>,
    /// Release only this lease name.
    #[arg(long = "name", value_name = "LEASE", requires = "pool")]
    pub lease: Option<String>,
}

#[derive(Debug, Subcommand)]
pub enum ReleaseKind {
    /// Release a port reservation, or every one.
    Port { name: Option<String> },
    /// Release a simulator lease, or every one.
    Sim {
        #[arg(long)]
        name: Option<String>,
    },
    /// Release resource permits and locks.
    Resource {
        #[command(flatten)]
        selection: PoolSelection,
    },
    /// Release related repository leases.
    Repo {
        #[command(flatten)]
        selection: PoolSelection,
    },
    /// End a secret grant or cancel its request, or every one.
    Secret { name: Option<String> },
}

#[derive(Debug, Subcommand)]
pub enum ResourceCommand {
    /// Acquire a permit, reader/writer lock, or related repository path.
    Acquire {
        pool: String,
        workspace: Option<String>,
        #[arg(long)]
        resource: Option<String>,
        /// Lock mode (defaults to permit for semaphores, write for rwlocks, read for repos).
        #[arg(long, value_enum)]
        mode: Option<crate::daemon::resources::LockMode>,
        /// Stable lease name; use different names to request multiple permits.
        #[arg(long, default_value = "default")]
        name: String,
        #[arg(long)]
        reason: Option<String>,
        /// Queue for capacity and wait until acquired or denied.
        #[arg(long)]
        queue: bool,
    },
    Release {
        pool: String,
        workspace: Option<String>,
        #[arg(long, default_value = "default")]
        name: String,
    },
    /// Show configured pools, capacity, and current leases.
    List {
        #[command(flatten)]
        scope: WorkspaceScope,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_cli_and_config_spellings_remain_compatible() {
        for spelling in [
            "codex",
            "claude",
            "happy-claude",
            "happy-codex",
            "my-agent_2",
            "t3",
        ] {
            let expected: Agent = spelling.parse().unwrap();
            assert_eq!(expected.name(), spelling);
            let parsed = Cli::try_parse_from(["shoal", "add", "--agent", spelling]).unwrap();
            let Some(Command::Add { agent, .. }) = parsed.command else {
                panic!("wrong command")
            };
            assert_eq!(agent, Some(expected.clone()));
            let text = format!("default_agent = {spelling:?}");
            let global: crate::config::Config = toml::from_str(&text).unwrap();
            let repo: crate::config::repo::RepoConfig = toml::from_str(&text).unwrap();
            assert_eq!(global.default_agent, agent);
            assert_eq!(repo.default_agent, agent);
            assert_eq!(serde_json::to_value(&expected).unwrap(), spelling);
            assert_eq!(
                serde_json::from_value::<Agent>(spelling.into()).unwrap(),
                expected
            );
        }
        for invalid in ["Codex", "happy-Codex", "", "two words", "-agent"] {
            assert!(Cli::try_parse_from(["shoal", "add", "--agent", invalid]).is_err());
            let text = format!("default_agent = {invalid:?}");
            assert!(toml::from_str::<crate::config::Config>(&text).is_err());
            assert!(toml::from_str::<crate::config::repo::RepoConfig>(&text).is_err());
            assert!(serde_json::from_value::<Agent>(invalid.into()).is_err());
        }
    }

    #[test]
    fn link_arguments_end_with_a_relation_and_accept_hash_numbers() {
        let items = |args: &[&str]| {
            let parsed = Cli::try_parse_from(args).unwrap();
            let Some(Command::Link { items }) = parsed.command else {
                panic!("wrong command")
            };
            let (items, relation) = items.split().unwrap();
            (items.kind_or_url, items.item, relation)
        };
        assert_eq!(
            items(&["shoal", "link", "#42", "assigned"]),
            (Some("42".into()), None, Relation::Assigned)
        );
        assert_eq!(
            items(&["shoal", "link", "pr", "#42", "related"]),
            (Some("pr".into()), Some("42".into()), Relation::Related)
        );
        assert_eq!(
            items(&["shoal", "link", "opened"]),
            (None, None, Relation::Opened)
        );
        for unchanged in [
            "#",
            "#x",
            "##42",
            "https://forge.example/team/repo/issues/1",
        ] {
            assert_eq!(
                items(&["shoal", "link", unchanged, "assigned"]),
                (Some(unchanged.into()), None, Relation::Assigned)
            );
        }
        assert!(Cli::try_parse_from(["shoal", "link"]).is_err());
        assert!(Cli::try_parse_from(["shoal", "link", "pr", "1", "2", "assigned"]).is_err());
        let Some(Command::Unlink { items }) = Cli::try_parse_from(["shoal", "unlink", "42"])
            .unwrap()
            .command
        else {
            panic!("wrong command")
        };
        assert!(items.split().is_err());
    }

    #[test]
    fn agent_parsers_preserve_completion_order() {
        use clap::builder::TypedValueParser;
        let names: Vec<_> = AgentParser
            .possible_values()
            .unwrap()
            .map(|value| value.get_name().to_owned())
            .collect();
        assert_eq!(names[..5], ["codex", "claude", "opencode", "pi", "grok"]);
        #[cfg(feature = "happy")]
        assert_eq!(names[5..], ["happy-claude", "happy-codex"]);
    }

    #[cfg(feature = "happy")]
    #[test]
    fn happy_takes_the_agents_happy_runs() {
        let names: Vec<_> = crate::happy::Agent::value_variants()
            .iter()
            .map(|agent| agent.to_possible_value().unwrap().get_name().to_owned())
            .collect();
        assert_eq!(names, ["claude", "codex"]);
        for (name, expected) in [
            ("claude", crate::happy::Agent::Claude),
            ("codex", crate::happy::Agent::Codex),
        ] {
            let parsed = Cli::try_parse_from(["shoal", "happy", name]).unwrap();
            assert!(
                matches!(parsed.command, Some(Command::Happy { agent, .. }) if agent == expected)
            );
        }
        for invalid in ["Claude", "Codex", "happy-codex", "custom"] {
            assert!(Cli::try_parse_from(["shoal", "happy", invalid]).is_err());
        }
    }

    #[test]
    fn doctor_replaces_reconcile_without_an_alias() {
        use clap::CommandFactory;
        let command = Cli::command();
        assert!(command.find_subcommand("doctor").is_some());
        assert!(command.find_subcommand("reconcile").is_none());
        for flag in ["--stop", "--acknowledge-stopped", "--reclaim"] {
            assert!(Cli::try_parse_from(["shoal", "doctor", flag]).is_err());
            assert!(Cli::try_parse_from(["shoal", "doctor", "--repair", flag]).is_ok());
        }
    }

    #[test]
    fn removal_commands_share_confirmation_flags() {
        for flag in ["-y", "--yes"] {
            let workspace = Cli::try_parse_from(["shoal", "rm", "workspace", flag]).unwrap();
            assert!(matches!(
                workspace.command,
                Some(Command::Rm {
                    confirmation: ConfirmationArgs { yes: true },
                    ..
                })
            ));

            let repository =
                Cli::try_parse_from(["shoal", "repo", "rm", "repository", flag]).unwrap();
            assert!(matches!(
                repository.command,
                Some(Command::Repo {
                    command: Some(RepoCommand::Rm {
                        confirmation: ConfirmationArgs { yes: true },
                        ..
                    })
                })
            ));
        }
    }

    #[test]
    fn repository_setup_names_its_agent_and_forwards_arguments() {
        let cli = Cli::try_parse_from([
            "shoal", "repo", "setup", "project", "--agent", "codex", "--", "--model", "fast",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Repo {
                command: Some(RepoCommand::Setup {
                    repository: Some(repository),
                    agent: Some(agent),
                    args,
                })
            }) if repository == "project" && agent.name() == "codex" && args == ["--model", "fast"]
        ));
    }

    #[test]
    fn codex_mode_flags_do_not_reserve_workspace_names() {
        for (args, expected_workspace, expected_cli, expected_app) in [
            (["shoal", "codex", "cli", "--app"], "cli", false, true),
            (["shoal", "codex", "--cli", "app"], "app", true, false),
        ] {
            let parsed = Cli::try_parse_from(args).unwrap();
            let Some(Command::Codex {
                workspace,
                cli,
                app,
                ..
            }) = parsed.command
            else {
                panic!("wrong command")
            };
            assert_eq!(workspace.as_deref(), Some(expected_workspace));
            assert_eq!(cli, expected_cli);
            assert_eq!(app, expected_app);
        }

        assert!(Cli::try_parse_from(["shoal", "codex", "--cli", "--app"]).is_err());
        assert!(Cli::try_parse_from(["shoal", "codex", "cli", "workspace"]).is_err());
    }

    #[test]
    fn add_uses_a_positional_new_branch_and_existing_branch_flag() {
        let new = Cli::try_parse_from(["shoal", "add", "repo", "feature/topic"]).unwrap();
        assert!(matches!(
            new.command,
            Some(Command::Add {
                repository,
                branch,
                existing: None,
                ..
            }) if repository.as_deref() == Some("repo")
                && branch.as_deref() == Some("feature/topic")
        ));

        let existing =
            Cli::try_parse_from(["shoal", "add", "repo", "--existing", "origin/topic"]).unwrap();
        assert!(matches!(
            existing.command,
            Some(Command::Add {
                repository,
                branch: None,
                existing,
                ..
            }) if repository.as_deref() == Some("repo")
                && existing.as_deref() == Some("origin/topic")
        ));
    }

    #[test]
    fn creation_accepts_base_refs() {
        for args in [
            vec!["shoal", "add", "repo", "topic", "--base", "release/v1"],
            vec!["shoal", "add", "repo", "--issue", "122", "--base", "v1.0"],
            vec!["shoal", "add", "122", "--base", "HEAD~1"],
            vec![
                "shoal",
                "add",
                "repo",
                "--existing",
                "topic",
                "--base",
                "main",
            ],
        ] {
            let expected = *args.last().unwrap();
            let parsed = Cli::try_parse_from(args).unwrap();
            let base = match parsed.command.unwrap() {
                Command::Add { base, .. } => base,
                _ => panic!("wrong command"),
            };
            assert_eq!(base.as_deref(), Some(expected));
        }
    }

    #[test]
    fn done_flags_override_the_default_and_remain_available_to_agents() {
        for (args, expected) in [
            (vec!["shoal", "done"], false),
            (vec!["shoal", "done", "--cleanup"], true),
        ] {
            let command = Cli::try_parse_from(args).unwrap().command.unwrap();
            assert!(!command.is_administrative());
            let Command::Done { cleanup, workspace } = command else {
                panic!("wrong command");
            };
            assert_eq!(cleanup, expected);
            assert!(workspace.is_none());
        }
        assert!(Cli::try_parse_from(["shoal", "done", "--keep"]).is_err());
        assert!(
            matches!(Cli::try_parse_from(["shoal", "done", "review", "--cleanup"]).unwrap().command,
            Some(Command::Done { workspace: Some(name), cleanup: true }) if name == "review")
        );
    }

    #[test]
    fn review_selects_a_workspace_or_a_pr() {
        let review = Cli::try_parse_from(["shoal", "review", "--pr", "7", "--agent", "claude"]);
        assert!(matches!(review.unwrap().command,
            Some(Command::Review { workspace: None, pr: Some(pr), agent: Some(agent), .. }) if pr == "7" && agent.name() == "claude"));
        for args in [
            vec![
                "shoal", "review", "--pr", "7", "--manual", "--agent", "claude",
            ],
            vec!["shoal", "review", "workspace", "--pr", "7"],
        ] {
            assert!(Cli::try_parse_from(args).is_err());
        }
    }

    #[test]
    fn config_show_is_available_to_workspace_processes() {
        let show = Cli::try_parse_from(["shoal", "config", "show", "workspace"]).unwrap();
        assert!(matches!(
            &show.command,
            Some(Command::Config {
                command: ConfigCommand::Show { workspace }
            }) if workspace.as_deref() == Some("workspace")
        ));
        assert!(!show.command.unwrap().is_administrative());
        let reset = Cli::try_parse_from(["shoal", "config", "reset"]).unwrap();
        assert!(reset.command.unwrap().is_administrative());
    }

    #[test]
    fn kept_branch_commands_are_administrative() {
        for args in [
            vec!["shoal", "branch"],
            vec!["shoal", "branch", "ls"],
            vec!["shoal", "branch", "rm", "--all", "--yes"],
        ] {
            let command = Cli::try_parse_from(args).unwrap().command.unwrap();
            assert!(command.is_administrative());
        }
    }

    #[test]
    fn update_is_administrative() {
        let update = Cli::try_parse_from(["shoal", "update"]).unwrap();
        assert!(update.command.unwrap().is_administrative());
    }
}
