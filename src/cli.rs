pub(crate) mod agents;
pub mod availability;
pub mod client;
pub mod commands;
pub mod completion;
pub mod context;
pub mod forge_hosts;
mod help;
mod installed;
pub mod internal;
pub mod list_status;
pub mod output;
mod progress;
pub mod recent;
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
    disable_help_subcommand = true,
    help_template = help::template(),
    after_help = "Start here (with a registered repository):\n  shoal add my-project fix-login\n  shoal run --workspace fix-login -- cargo test\n  shoal status fix-login\n\nRun `shoal run` to list configured commands.\nUse `shoal <command> --help` for details."
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
    /// Emit machine-readable output and never prompt; the default when
    /// SHOAL_OUTPUT=json.
    #[arg(long, global = true)]
    pub json: bool,
    /// Emit human-readable output, overriding SHOAL_OUTPUT.
    #[arg(long, global = true)]
    pub text: bool,
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Default, Args)]
pub struct ConfirmationArgs {
    /// Confirm without prompting.
    #[arg(short = 'y', long)]
    pub yes: bool,
}

// Arguments of `shoal repo labels` and `shoal forge capabilities`.
#[derive(Debug, Args)]
pub struct RepositoryArgs {
    /// Workspace to use; defaults to the current workspace or picker.
    #[arg(long)]
    pub workspace: Option<String>,
}

// Arguments of `shoal forge api`.
#[derive(Debug, Args)]
pub struct ApiArgs {
    /// Path under the forge's API root, where `{owner}` and `{repo}` name the
    /// repository, such as `repos/{owner}/{repo}/branches`.
    pub path: String,
    /// HTTP method; defaults to POST with --input and GET otherwise.
    #[arg(short = 'X', long, value_enum, ignore_case = true)]
    pub method: Option<crate::forge::raw::HttpMethod>,
    /// JSON request body from a file, or `-` for standard input.
    #[arg(long, value_name = "FILE")]
    pub input: Option<PathBuf>,
    /// Workspace to use; defaults to the current workspace or picker.
    #[arg(long)]
    pub workspace: Option<String>,
}

// Arguments of `shoal forge ci run`.
#[derive(Debug, Args)]
pub struct RunArgs {
    /// Workflow file or ID, such as ci.yml, or a Bitbucket custom pipeline;
    /// GitLab runs the branch's pipeline and takes none.
    pub workflow: Option<String>,
    /// Workspace to use; defaults to the current workspace or picker.
    #[arg(long)]
    pub workspace: Option<String>,
    /// Branch to run on; defaults to the workspace's branch, the only one
    /// workspace processes may run on.
    #[arg(long)]
    pub branch: Option<String>,
    /// Pass a workflow input; repeat for more.
    #[arg(long = "input", value_name = "NAME=VALUE", value_parser = workflow_input)]
    pub inputs: Vec<(String, String)>,
}

// Arguments of `shoal export`.
#[derive(Debug, Args)]
pub struct ExportArgs {
    #[arg(conflicts_with = "all", value_name = "WORKSPACE")]
    pub workspace: Vec<String>,
    /// Export every workspace of the repository --repo names.
    #[arg(long, requires = "repository")]
    pub all: bool,
    /// Repository whose workspaces --all exports.
    #[arg(long = "repo", requires = "all")]
    pub repository: Option<String>,
    /// File to write, or with several workspaces or --all, the directory to write a file per
    /// workspace in.
    #[arg(short, long)]
    pub output: PathBuf,
    #[command(flatten)]
    pub confirmation: ConfirmationArgs,
}

// Arguments of `shoal import`.
#[derive(Debug, Args)]
pub struct ImportArgs {
    /// Export files or directories of them.
    #[arg(required = true)]
    pub files: Vec<PathBuf>,
    /// Repository to import into, when it does not share the exported repository's origin.
    #[arg(long = "repo")]
    pub repository: Option<String>,
    /// Continue the stopped agents' sessions on any machine, such as one the agents'
    /// sessions were copied to.
    #[arg(long, conflicts_with = "new_sessions")]
    pub keep_sessions: bool,
    /// Start new sessions even on the machine that exported the workspace.
    #[arg(long, conflicts_with = "keep_sessions")]
    pub new_sessions: bool,
}

#[derive(Debug, Default, Args)]
pub struct WorkspaceScope {
    pub workspace: Option<String>,
    /// All workspaces.
    #[arg(long, conflicts_with = "workspace")]
    pub all: bool,
}

// The workspaces of a command that acts on several.
#[derive(Debug, Default, Args)]
pub struct WorkspaceTargets {
    #[arg(value_name = "WORKSPACE")]
    pub workspace: Vec<String>,
    /// All workspaces.
    #[arg(long, conflicts_with = "workspace")]
    pub all: bool,
}

// Every subcommand enum defers its commands' arguments until parsing reaches
// them: the whole tree takes megabytes that stay resident in each long-lived
// process, such as an agent's wrapper and hooks. A command's attributes apply
// before its deferred arguments, so they cannot change those arguments, an
// argument struct's doc comment would replace the command's description, and
// code that reads arguments from the tree builds it first.
#[derive(Debug, Subcommand)]
#[command(defer = true)]
pub enum Command {
    /// List configured commands, run one by name, or run an arbitrary command after --.
    Run {
        /// Configured command; omit it to execute the arguments after -- directly.
        name: Option<String>,
        /// Workspace to use; defaults to the current workspace or picker.
        #[arg(long)]
        workspace: Option<String>,
        #[arg(last = true)]
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
    /// Inspect and access the current repository's forge.
    Forge {
        #[command(subcommand)]
        command: ForgeCommand,
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
        /// Queue the agent until this workspace, queued agent or issue is complete;
        /// repeat for more. An issue's agent also waits for the issues blocking it.
        #[arg(long, value_name = "WORK", conflicts_with_all = ["now", "focus"])]
        after: Vec<String>,
        /// Arguments forwarded to the agent.
        #[arg(last = true)]
        args: Vec<OsString>,
    },
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
        /// Show only this failed check's log, where the forge reads job logs.
        #[arg(long, value_name = "NAME")]
        check: Option<String>,
    },
    /// Mark linked items, or the workspace when none are linked, ready for review at HEAD.
    Ready {
        #[command(flatten)]
        items: ItemArgs,
        /// Withdraw ready-for-review marks from the selected items.
        #[arg(long)]
        undo: bool,
        /// Return the PRs whose marks are withdrawn to draft, overriding [unready] draft.
        #[arg(long, requires = "undo", overrides_with = "no_draft")]
        draft: bool,
        /// Leave the PRs' draft state unchanged, overriding [unready] draft.
        #[arg(long, requires = "undo", overrides_with = "draft")]
        no_draft: bool,
    },
    /// Restore stopped agent sessions and list interrupted commands.
    Resume {
        #[command(flatten)]
        targets: WorkspaceTargets,
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
        #[arg(long)]
        workspace: Option<String>,
    },
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
    /// Mark the assignment finished; by default stop tracked commands and clean up safely.
    Done {
        workspace: Option<String>,
        /// Request cleanup even when [done] cleanup is false; preserve dirty or unpushed work.
        #[arg(long, conflicts_with = "undo")]
        cleanup: bool,
        /// Withdraw a recorded completion without keeping the workspace.
        #[arg(long)]
        undo: bool,
        /// Leave the workspace's demos running until cleanup removes it.
        #[arg(long, conflicts_with = "undo")]
        keep_demos: bool,
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
    /// List ongoing work, or manage a workspace.
    #[command(args_conflicts_with_subcommands = true)]
    Work {
        #[command(subcommand)]
        command: Option<WorkCommand>,
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
    /// Enter a workspace, or use - for the one you were in before.
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
    /// Show leases and usage for every kind, or one.
    Leases {
        kind: Option<LeaseKind>,
        /// Workspace to show; defaults to the current workspace or picker.
        #[arg(long)]
        workspace: Option<String>,
        /// Show every managed workspace.
        #[arg(long, conflicts_with = "workspace")]
        all: bool,
    },
    /// Show configuration and available inventory for every kind, or one.
    Catalog {
        kind: Option<CatalogKind>,
        /// Workspace whose configuration to show; defaults to the current workspace or picker.
        #[arg(long)]
        workspace: Option<String>,
    },
    /// Show recorded requests for a kind.
    History {
        #[command(subcommand)]
        kind: HistoryKind,
    },
    /// Inspect approval requests and choose or submit a decision.
    #[command(args_conflicts_with_subcommands = true)]
    Approvals {
        #[command(subcommand)]
        command: Option<ApprovalsCommand>,
        #[command(flatten)]
        args: ApprovalsArgs,
    },
    /// Show resource conflicts, finished agents, and automatic cleanup.
    #[command(args_conflicts_with_subcommands = true)]
    Alerts {
        #[command(subcommand)]
        command: Option<AlertsCommand>,
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
    /// Stop tracked agents and commands, saving them for `shoal resume`; keep the workspaces.
    Stop {
        #[command(flatten)]
        targets: WorkspaceTargets,
        /// Treat executions Shoal cannot prove ended as stopped without asking.
        #[arg(short = 'y', long)]
        yes: bool,
    },
    /// Remove workspaces and release their resources.
    ///
    /// A branch goes too when a remote or the default branch has all its commits; otherwise it
    /// stays. Deleting uncommitted changes or commits on a detached HEAD needs confirmation,
    /// asked once for every workspace.
    Rm {
        #[arg(value_name = "WORKSPACE")]
        workspace: Vec<String>,
        #[command(flatten)]
        confirmation: ConfirmationArgs,
    },
    /// Remove what Shoal left behind: workspaces cleanup would remove, kept branches and caches.
    ///
    /// Without a kind, lists them; on a terminal, a picker offers them, where Tab selects several
    /// and nothing starts selected.
    Cleanup {
        #[command(subcommand)]
        kind: Option<CleanupKind>,
        #[command(flatten)]
        args: CleanupArgs,
    },
    /// Remove a workspace and write it to a file that shoal import opens on another machine.
    ///
    /// The file holds the removal's checkpoint, as a Git bundle without the commits of the
    /// default branch, and the workspace's base, links and stopped agents. A removed workspace
    /// is exported without another removal. `-` writes to standard output. Several workspaces,
    /// or with --all every workspace of the repository, go to a file each in the directory -o
    /// names, once each one can be exported.
    Export(ExportArgs),
    /// Open workspaces shoal export wrote, in the repository with the same origin.
    ///
    /// Fetches the default branch when a file builds on commits the repository lacks. On the
    /// machine that exported it, shoal resume continues the stopped agents' sessions; elsewhere,
    /// it starts new sessions, since the sessions stayed on the other machine. --keep-sessions
    /// and --new-sessions override that. `-` reads from standard input. A directory imports each
    /// .shoal file in it.
    Import(ImportArgs),
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
    /// a zmx session. A Happy chat starts detached, to continue in the Happy
    /// app. Chats are not tracked executions.
    Chat {
        /// Run in the current pane instead of opening a Herdr tab.
        #[arg(long)]
        here: bool,
        /// Registered repository; defaults to the current checkout or workspace, else a picker.
        repository: Option<String>,
        /// Agent to start; defaults to default_agent, otherwise a picker.
        #[arg(long, value_parser = AgentParser)]
        agent: Option<Agent>,
        /// Arguments forwarded to the agent.
        #[arg(last = true)]
        args: Vec<OsString>,
    },
    /// Settle an issue's plan with an agent in a chat before anyone implements it.
    ///
    /// Without an issue, offers the open issues labelled [issue] plan_label.
    /// The chat opens like `shoal chat`, with the issue as its first task.
    Plan {
        /// Issue number or URL; defaults to a picker.
        #[arg(value_parser = item_reference)]
        issue: Option<String>,
        /// Plan the oldest open issue labelled [issue] plan_label.
        #[arg(long, conflicts_with_all = ["issue", "newest"])]
        oldest: bool,
        /// Plan the newest open issue labelled [issue] plan_label.
        #[arg(long, conflicts_with = "issue")]
        newest: bool,
        /// Registered repository of the issue; defaults to the URL's repository or the current checkout/workspace.
        #[arg(long = "repo")]
        repository: Option<String>,
        /// Run in the current pane instead of opening a Herdr tab.
        #[arg(long)]
        here: bool,
        /// Agent to start; defaults to default_agent, otherwise a picker.
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
    /// Replace a downloaded shoal with the latest GitHub release.
    Update,
    /// Inspect and control the local daemon.
    Daemon {
        #[command(subcommand)]
        command: DaemonCommand,
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

// A description or comment, given inline or read from a file.
#[derive(Debug, Default, Args)]
pub struct BodyArgs {
    /// Text of the description or comment.
    #[arg(long)]
    pub body: Option<String>,
    /// Read the text from a file.
    #[arg(long, value_name = "PATH", conflicts_with = "body")]
    pub body_file: Option<PathBuf>,
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

// An optional item selection followed by its relation.
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
#[command(defer = true)]
pub enum AlertsCommand {
    /// Send the user a notification about a workspace without marking it done.
    Send {
        /// One line of text, such as "PR #12 is ready to merge".
        message: String,
        /// Workspace to use; defaults to the current workspace or picker.
        #[arg(long)]
        workspace: Option<String>,
    },
    /// Delete all notifications, including unread ones.
    Clear,
    /// Withdraw an unread message the workspace sent with shoal alerts send.
    Withdraw {
        /// Notification number, as shoal alerts send printed it.
        id: i64,
        /// Workspace to use; defaults to the current workspace or picker.
        #[arg(long)]
        workspace: Option<String>,
    },
}

#[derive(Debug, Default, Args)]
pub struct ApprovalsArgs {
    /// Request number; omit to choose a request.
    pub id: Option<i64>,
    /// List requests and grants for this workspace.
    #[arg(long, conflicts_with_all = ["id", "approval_action"])]
    pub workspace: Option<String>,
    /// Approve the recorded settings, or open a proposed issue or PR.
    #[arg(long, group = "approval_action")]
    pub approve: bool,
    /// Deny the request.
    #[arg(long, group = "approval_action")]
    pub deny: bool,
    /// Pass a request routed to this workspace's coordinator up to its parent or the user.
    #[arg(long, group = "approval_action", requires = "id")]
    pub escalate: bool,
    /// Withdraw a request no longer needed.
    #[arg(long, group = "approval_action", requires = "id")]
    pub withdraw: bool,
    /// Replace a proposed issue's or PR's title.
    #[arg(long, requires = "approve")]
    pub title: Option<String>,
    #[command(flatten)]
    pub body: BodyArgs,
    /// Tell the agent why its proposal was denied.
    #[arg(long, requires = "deny")]
    pub reason: Option<String>,
    /// Deny an issue proposal as a duplicate of this issue number or URL.
    #[arg(long, requires = "deny", conflicts_with = "duplicate_of_approval")]
    pub duplicate_of: Option<String>,
    /// Deny an issue proposal as a duplicate of another pending proposal.
    #[arg(long, requires = "deny")]
    pub duplicate_of_approval: Option<i64>,
}

#[derive(Debug, Subcommand)]
#[command(defer = true)]
pub enum ApprovalsCommand {
    /// Withdraw a pending request the workspace's agent no longer needs.
    Withdraw {
        /// Request number.
        id: i64,
        /// Workspace to use; defaults to the current workspace or picker.
        #[arg(long)]
        workspace: Option<String>,
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
pub enum DemoCommand {
    /// Start a demo and tell the user how to open it; replaces a running
    /// demo of the same name.
    Start {
        /// A configured demo, or the name for the command after `--`;
        /// defaults to the only configured demo, or `demo` for a command.
        name: Option<String>,
        /// How the demo reaches the user, replacing the configured preset.
        #[arg(long, value_enum)]
        preset: Option<crate::demos::Preset>,
        /// A web demo's URL scheme, replacing the configured one; http by default.
        #[arg(long, value_enum)]
        scheme: Option<crate::demos::Scheme>,
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
        #[arg(long)]
        workspace: Option<String>,
        /// The command to run instead of the configured one.
        #[arg(last = true)]
        command: Vec<OsString>,
    },
    /// Stop a demo, or every demo of the workspace.
    Stop {
        name: Option<String>,
        /// Workspace to use; defaults to the current workspace or picker.
        #[arg(long)]
        workspace: Option<String>,
    },
    /// List configured and running demos.
    Ls {
        /// Workspace to list; defaults to the current workspace, otherwise
        /// the running demos of every workspace.
        #[arg(long)]
        workspace: Option<String>,
    },
    /// Print what a running demo shows.
    Screen {
        name: String,
        /// Lines to print from the end.
        #[arg(long, default_value_t = 50)]
        lines: usize,
        /// Workspace to use; defaults to the current workspace or picker.
        #[arg(long)]
        workspace: Option<String>,
    },
    /// Type text into a running demo, such as $'q' or $'\r' for Enter.
    Send {
        name: String,
        keys: String,
        /// Workspace to use; defaults to the current workspace or picker.
        #[arg(long)]
        workspace: Option<String>,
    },
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

#[derive(Debug, Subcommand)]
#[command(defer = true)]
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

impl Cli {
    /// Parse the process arguments as `Cli::parse` does, recording what the
    /// command needs of its workspace and resolving its output format.
    pub fn from_args() -> Self {
        let mut matches = <Self as clap::CommandFactory>::command()
            .try_get_matches()
            .unwrap_or_else(|error| exit_parse(error));
        let mut path = vec![];
        let mut leaf = &matches;
        while let Some((name, child)) = leaf.subcommand() {
            path.push(name.to_owned());
            leaf = child;
        }
        let path: Vec<_> = path.iter().map(String::as_str).collect();
        let need = availability::Need::of_matches(&path, leaf).unwrap_or(availability::Need::Ready);
        let mut cli = <Self as clap::FromArgMatches>::from_arg_matches_mut(&mut matches)
            .unwrap_or_else(|error| {
                error
                    .format(&mut <Self as clap::CommandFactory>::command())
                    .exit()
            });
        cli.need = need;
        cli.resolve_output(std::env::var_os(crate::env::OUTPUT).as_deref())
            .unwrap_or_else(|error| {
                eprintln!(
                    "{} {error:#}",
                    output::Palette::stderr(false).paint(output::Style::Error, "error:")
                );
                std::process::exit(2)
            });
        cli
    }

    /// Let the `SHOAL_OUTPUT` `setting` choose JSON when the command line
    /// selects neither format, so every later step reads one resolved mode.
    fn resolve_output(&mut self, setting: Option<&std::ffi::OsStr>) -> anyhow::Result<()> {
        anyhow::ensure!(
            !(self.json && self.text),
            "--json and --text cannot be used together"
        );
        if !self.json && !self.text {
            self.json = crate::env::selects_json(setting)?;
        }
        Ok(())
    }
}

fn exit_parse(error: clap::Error) -> ! {
    if error.use_stderr() {
        error.exit()
    }
    output::print_clap_output(&error);
    std::process::exit(error.exit_code())
}

impl Command {
    /// The name under which the command remembers the workspaces it acts on,
    /// so `-` selects the previous one; `None` for commands that keep no history.
    pub fn recent_name(&self) -> Option<String> {
        Some(match self {
            Command::Cd { .. } => "cd".into(),
            Command::Attach { .. } => "attach".into(),
            Command::Demo { .. } => "demo".into(),
            Command::Edit { .. } => "edit".into(),
            Command::Diff { .. } => "diff".into(),
            Command::Status { .. } => "status".into(),
            Command::Run {
                name: None, args, ..
            } if !args.is_empty() => "run".into(),
            Command::Claude { .. } => "claude".into(),
            Command::Codex { .. } => "codex".into(),
            Command::T3 { .. } => "t3".into(),
            #[cfg(feature = "happy")]
            Command::Happy { .. } => "happy".into(),
            Command::Run {
                name: Some(name), ..
            } => name.clone(),
            Command::Custom(words) => words.first()?.to_str()?.to_owned(),
            _ => return None,
        })
    }

    /// Lifecycle and service administration is denied to scoped workspace
    /// processes; everything else is ordinary workspace work.
    pub fn is_administrative(&self) -> bool {
        matches!(
            self,
            Command::Doctor { .. }
                | Command::Contribute(_)
                | Command::Cleanup { .. }
                | Command::Update
                | Command::Config {
                    command: ConfigCommand::Install { .. }
                        | ConfigCommand::Reset
                        | ConfigCommand::Set { .. }
                        | ConfigCommand::Unset { .. }
                }
                | Command::Daemon {
                    command: DaemonCommand::Install { .. }
                        | DaemonCommand::Run { .. }
                        | DaemonCommand::Start
                        | DaemonCommand::Stop
                        | DaemonCommand::Restart
                        | DaemonCommand::Reload
                }
        )
    }
}

// Arguments of `shoal contribute`.
#[derive(Debug, Args)]
pub struct ContributeArgs {
    /// Run in the current pane instead of opening a Herdr tab.
    #[arg(long)]
    pub here: bool,
    /// Give the new tab or session the terminal, overriding the `focus` setting;
    /// its agent starts now, as with --now.
    #[arg(long, conflicts_with = "here")]
    pub focus: bool,
    /// Repository URL, or an issue, PR or branch URL.
    pub target: String,
    /// Git branch name; a portable workspace name is derived from it.
    pub branch: Option<String>,
    /// Start a built-in agent or a configured command after worktree creation.
    #[arg(long, value_parser = AgentParser)]
    pub agent: Option<Agent>,
    /// Start the agent now even when max_agents is reached, instead of queuing it.
    #[arg(long)]
    pub now: bool,
    /// Arguments forwarded to the agent.
    #[arg(last = true)]
    pub args: Vec<OsString>,
}

/// An issue or PR argument, where `#42` means `42`.
fn item_reference(input: &str) -> Result<String, std::convert::Infallible> {
    Ok(match input.strip_prefix('#') {
        Some(number) if IssueInput::parse(number) == IssueInput::Number => number.to_owned(),
        _ => input.to_owned(),
    })
}

fn workflow_input(input: &str) -> Result<(String, String), String> {
    match input.split_once('=') {
        Some((name, value)) if !name.trim().is_empty() => Ok((name.into(), value.into())),
        _ => Err("expected NAME=VALUE".into()),
    }
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
#[command(defer = true)]
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
#[command(defer = true)]
pub enum CleanupKind {
    /// Remove the workspaces idle or merged cleanup would remove, without waiting for their idle
    /// periods.
    ///
    /// Applies where those rules are disabled; held, busy, unpushed or unmerged dirty workspaces are
    /// kept, and the unsaved files of merged ones go into their checkpoints. Names limit it to those
    /// workspaces.
    Workspaces { names: Vec<String> },
    /// Delete branches Shoal kept when it removed their workspaces, with the commits only they have.
    ///
    /// Removal keeps a branch only while some of its commits are on no remote and not on the
    /// default branch, and deletes it once they are. Without names or --all, choose them in a
    /// picker, where Tab selects several.
    Branches {
        names: Vec<String>,
        /// Delete every kept branch.
        #[arg(long, conflicts_with = "names")]
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
    /// Only this repository's workspaces, kept branches and caches, by name or ID.
    #[arg(long, global = true)]
    pub repo: Option<String>,
    /// Delete kept branches without asking.
    #[arg(short = 'y', long, global = true)]
    pub yes: bool,
}

#[derive(Debug, Subcommand)]
#[command(defer = true)]
pub enum WorkCommand {
    /// Show, set or clear the workspace a stacked workspace builds on.
    #[command(args_conflicts_with_subcommands = true)]
    Base {
        #[command(subcommand)]
        command: Option<BaseCommand>,
        workspace: Option<String>,
    },
    /// Export the environment for processes started in a workspace.
    Env {
        workspace: Option<String>,
        /// Revoke a previously exported scope token.
        #[arg(long)]
        revoke: Option<String>,
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
    /// Keep a workspace's HEAD and unsaved files in a checkpoint that shoal work restore returns to.
    Checkpoint {
        workspace: Option<String>,
        /// Describe the checkpoint in shoal work restore's list, in at most 200 characters.
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
    /// Run or retry workspace setup and the post-setup hook.
    Setup {
        /// Workspace to use; defaults to the current workspace or picker.
        workspace: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
#[command(defer = true)]
pub enum ForgeCommand {
    /// List what the repository's forge supports.
    Capabilities(RepositoryArgs),
    /// Send a request to the repository's forge API and print its JSON response.
    ///
    /// Workspace processes send it as the agent account, and only GET unless
    /// `[api] agent_writes` lets agents send others; anyone else sends any
    /// request with your own logins.
    Api(ApiArgs),
    /// Start CI workflow runs on a branch.
    Ci {
        #[command(subcommand)]
        command: ForgeCiCommand,
    },
}

#[derive(Debug, Subcommand)]
#[command(defer = true)]
pub enum ForgeCiCommand {
    /// Start a workflow run on a branch.
    ///
    /// It is a new run, not a rerun of a PR's checks; on Forgejo a failed
    /// check stays failed.
    Run(RunArgs),
    /// Rerun a workflow run; GitLab retries only its failed jobs.
    Rerun {
        #[command(flatten)]
        target: RunTarget,
        /// Rerun only the run's failed jobs.
        #[arg(long)]
        failed: bool,
    },
    /// Cancel a running workflow run.
    Cancel {
        #[command(flatten)]
        target: RunTarget,
    },
    /// Show a workflow run's state, its jobs and the log tails of failed jobs.
    View {
        #[command(flatten)]
        target: RunTarget,
        /// Show only this job's log.
        #[arg(long, value_name = "NAME", conflicts_with = "job_id")]
        job: Option<String>,
        /// Show only the log of the job with this ID, for jobs that share a name.
        #[arg(long, value_name = "ID")]
        job_id: Option<u64>,
    },
    /// List the newest workflow runs, on the workspace's branch by default.
    List {
        /// Workspace to use; defaults to the current workspace or picker.
        #[arg(long)]
        workspace: Option<String>,
        /// Branch whose runs to list; defaults to the workspace's branch.
        #[arg(long)]
        branch: Option<String>,
        /// List only this workflow file's runs; GitLab has none.
        #[arg(long)]
        workflow: Option<String>,
        /// The most runs to list (1–100).
        #[arg(long, default_value_t = 10)]
        limit: usize,
    },
}

// A workflow run to act on.
#[derive(Debug, Args)]
pub struct RunTarget {
    /// The run's web URL, as `shoal forge ci run` or a check links it, or the number in it.
    pub run: String,
    /// Workspace to use; defaults to the current workspace or picker.
    #[arg(long)]
    pub workspace: Option<String>,
}

#[derive(Debug, Subcommand)]
#[command(defer = true)]
pub enum RepoCommand {
    /// List the repository's labels with their descriptions.
    Labels(RepositoryArgs),
    /// Fetch a repository's remote and fast-forward its default branch.
    ///
    /// Never pushes or changes workspace branches; rebase or merge onto the
    /// updated branch with Git.
    Sync {
        /// Registered repository; defaults to the current checkout or workspace.
        repository: Option<String>,
    },
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
    /// Move every repository directory, with its workspaces and clone, into root_dir.
    Migrate {
        /// New root directory, saved as root_dir first; omit to use the configured one.
        #[arg(value_hint = clap::ValueHint::DirPath)]
        root: Option<PathBuf>,
        #[command(flatten)]
        confirmation: ConfirmationArgs,
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
    Ls,
}

#[derive(Debug, Subcommand)]
#[command(defer = true)]
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
        /// Workspace whose updates to wait for; defaults to the agent's own.
        #[arg(long, hide = true)]
        workspace: Option<String>,
        /// Continue a wait across a daemon restart, as the installed release
        /// does after an upgrade restarted the daemon.
        #[arg(long, hide = true)]
        resume: bool,
    },
    /// Print which workspace a starting subagent works in as hook context;
    /// Shoal's Claude Code and Codex `SubagentStart` hooks run it.
    SubagentStart,
    /// Split a child workspace for the worktree Claude Code creates and print
    /// its path; Shoal's Claude Code `WorktreeCreate` hook runs it.
    WorktreeCreate,
    /// Mark the child workspace at the worktree Claude Code removes done;
    /// Shoal's Claude Code `WorktreeRemove` hook runs it.
    WorktreeRemove,
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
    /// A demo's wrapper, restarting its command until the demo is stopped.
    #[command(name = internal::DEMO)]
    Demo {
        workspace: String,
        #[arg(long)]
        launch: String,
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
#[command(defer = true)]
pub enum ShellCommand {
    /// Generate shell completion scripts.
    Completions {
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },
    Init,
    #[command(hide = true)]
    Recover {
        path: PathBuf,
    },
}

#[derive(Debug, Subcommand)]
#[command(defer = true)]
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
#[command(defer = true)]
pub enum DaemonCommand {
    /// Install and start the per-user daemon service.
    Install {
        /// Preview the service definition without changing anything.
        #[arg(long)]
        dry_run: bool,
        /// Executable path to register (preserves symlinks for package upgrades).
        #[arg(long)]
        executable: Option<PathBuf>,
    },
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

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum CatalogKind {
    /// Automatic range, named ports and conflict policy.
    Port,
    /// Device types, installed runtimes and machine profiles.
    Sim,
    /// Resource pools, their members and approval settings.
    Resource,
    /// Related repositories that can be borrowed.
    Repo,
    /// Agent secrets, the tools they serve and approval settings.
    Secret,
}

#[derive(Debug, Subcommand)]
#[command(defer = true)]
pub enum HistoryKind {
    /// Review clean-device requests, including failed and busy requests.
    Sim {
        /// Workspace to show; defaults to the current workspace or picker.
        #[arg(long)]
        workspace: Option<String>,
        /// Show every managed workspace.
        #[arg(long, conflicts_with = "workspace")]
        all: bool,
        #[arg(long, default_value_t = 20, value_parser = clap::value_parser!(u32).range(1..=50))]
        limit: u32,
        /// Show entries older than this audit ID.
        #[arg(long)]
        before: Option<i64>,
    },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum LeaseKind {
    Hold,
    Port,
    Sim,
    Resource,
    Repo,
}

// Options shared by leases that a workspace may hold several of.
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
#[command(defer = true)]
pub enum AcquireKind {
    /// Keep a workspace against automatic cleanup until released.
    Hold {
        #[arg(long)]
        name: String,
        #[arg(long)]
        reason: Option<String>,
    },
    /// Reserve a named TCP port, or return the existing reservation.
    Port {
        /// Port name; omit to choose a configured port.
        name: Option<String>,
        #[arg(long)]
        port: Option<u16>,
        /// Environment variable exported to subsequent workspace commands.
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

// A pool, optionally narrowed to one named lease.
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
#[command(defer = true)]
pub enum ReleaseKind {
    /// Release a named workspace hold; omit its name to choose one.
    Hold {
        #[arg(long)]
        name: Option<String>,
    },
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Whether `args` resolve to JSON output under the `SHOAL_OUTPUT` `setting`.
    fn resolves_json(args: &[&str], setting: Option<&str>) -> anyhow::Result<bool> {
        let mut cli = Cli::try_parse_from(["shoal"].iter().chain(args)).unwrap();
        cli.resolve_output(setting.map(std::ffi::OsStr::new))?;
        Ok(cli.json)
    }

    #[test]
    fn the_command_line_selects_output_before_the_environment() {
        for setting in [None, Some(""), Some("text")] {
            assert!(!resolves_json(&["status"], setting).unwrap());
        }
        assert!(resolves_json(&["status"], Some("json")).unwrap());
        for setting in [None, Some("text"), Some("json")] {
            assert!(resolves_json(&["--json", "status"], setting).unwrap());
            assert!(resolves_json(&["status", "--json"], setting).unwrap());
            assert!(!resolves_json(&["status", "--text"], setting).unwrap());
        }
        assert!(resolves_json(&["--json", "status", "--text"], None).is_err());
        assert!(resolves_json(&["--text", "status", "--json"], None).is_err());
    }

    #[test]
    fn an_invalid_output_setting_fails_unless_the_command_line_selects_one() {
        let error = resolves_json(&["status"], Some("JSON")).unwrap_err();
        assert_eq!(
            error.to_string(),
            "SHOAL_OUTPUT must be `json` or `text`, not \"JSON\""
        );
        assert!(resolves_json(&["--json", "status"], Some("yaml")).unwrap());
        assert!(!resolves_json(&["--text", "status"], Some("yaml")).unwrap());
    }

    /// Builds each command's deferred arguments and checks that its
    /// description stays the one its parent lists.
    fn assert_descriptions_survive_deferral(command: &clap::Command) {
        for listed in command.get_subcommands() {
            let mut built = listed.clone();
            // Builds this command alone, leaving its subcommands deferred.
            let _ = built.render_usage();
            let name = built.get_name();
            assert_eq!(built.get_about(), listed.get_about(), "{name}");
            assert_eq!(built.get_long_about(), listed.get_long_about(), "{name}");
            assert_descriptions_survive_deferral(&built);
        }
    }

    #[test]
    fn deferred_arguments_keep_each_command_description() {
        let mut root = <Cli as clap::CommandFactory>::command();
        let _ = root.render_usage();
        assert_descriptions_survive_deferral(&root);
    }

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
    fn work_setup_accepts_a_workspace_without_listing_filters() {
        for workspace in [None, Some("worker")] {
            let mut args = vec!["shoal", "work", "setup"];
            args.extend(workspace);
            let cli = Cli::try_parse_from(args).unwrap();
            assert!(matches!(
                cli.command,
                Some(Command::Work {
                    command: Some(WorkCommand::Setup { workspace: target }),
                    ..
                }) if target.as_deref() == workspace
            ));
        }
        for filters in [
            vec!["--queue"],
            vec!["--chats"],
            vec!["--repo", "project"],
            vec!["--status", "ready"],
            vec!["--print"],
        ] {
            let mut args = vec!["shoal", "work"];
            args.extend(filters);
            args.extend(["setup", "worker"]);
            assert!(Cli::try_parse_from(args).is_err());
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
            let Command::Done {
                cleanup,
                workspace,
                undo,
                keep_demos,
            } = command
            else {
                panic!("wrong command");
            };
            assert_eq!(cleanup, expected);
            assert!(!undo && !keep_demos);
            assert!(workspace.is_none());
        }
        assert!(Cli::try_parse_from(["shoal", "done", "--keep"]).is_err());
        assert!(Cli::try_parse_from(["shoal", "done", "--undo", "--cleanup"]).is_err());
        assert!(Cli::try_parse_from(["shoal", "done", "--undo", "--keep-demos"]).is_err());
        assert!(matches!(
            Cli::try_parse_from(["shoal", "done", "--keep-demos"])
                .unwrap()
                .command,
            Some(Command::Done {
                keep_demos: true,
                ..
            })
        ));
        assert!(matches!(
            Cli::try_parse_from(["shoal", "done", "--undo"])
                .unwrap()
                .command,
            Some(Command::Done { undo: true, .. })
        ));
        for flag in ["--draft", "--no-draft"] {
            assert!(Cli::try_parse_from(["shoal", "ready", flag]).is_err());
            assert!(Cli::try_parse_from(["shoal", "ready", "--undo", flag]).is_ok());
        }
        assert!(
            matches!(Cli::try_parse_from(["shoal", "done", "review", "--cleanup"]).unwrap().command,
            Some(Command::Done { workspace: Some(name), cleanup: true, undo: false, .. }) if name == "review")
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
    fn cleanup_commands_are_administrative() {
        for args in [
            vec!["shoal", "cleanup"],
            vec!["shoal", "cleanup", "--dry-run"],
            vec!["shoal", "cleanup", "workspaces", "--repo", "project"],
            vec!["shoal", "cleanup", "branches", "--all", "--yes"],
            vec!["shoal", "cleanup", "caches", "--dry-run"],
        ] {
            let command = Cli::try_parse_from(args).unwrap().command.unwrap();
            assert!(command.is_administrative());
        }
    }

    #[test]
    fn registering_a_contribution_is_administrative() {
        let args = ["shoal", "contribute", "https://github.com/owner/repo"];
        let command = Cli::try_parse_from(args).unwrap().command.unwrap();
        assert!(command.is_administrative());
    }

    #[test]
    fn update_is_administrative() {
        let update = Cli::try_parse_from(["shoal", "update"]).unwrap();
        assert!(update.command.unwrap().is_administrative());
    }

    #[test]
    fn numbered_requests_keep_inspection_and_actions_distinct() {
        for args in [
            vec!["shoal", "approvals", "42"],
            vec!["shoal", "approvals", "42", "--approve"],
            vec!["shoal", "questions", "17"],
            vec!["shoal", "questions", "17", "--answer", "SQLite"],
        ] {
            assert!(Cli::try_parse_from(args).is_ok());
        }
        for args in [
            vec!["shoal", "approvals", "42", "--approve", "--deny"],
            vec![
                "shoal",
                "questions",
                "17",
                "--answer",
                "SQLite",
                "--withdraw",
            ],
        ] {
            assert!(Cli::try_parse_from(&args).is_err(), "{args:?}");
        }
        // Removed built-ins remain available as user-configured command names.
        for name in ["approve", "deny", "escalate", "answer"] {
            assert!(matches!(
                Cli::try_parse_from(["shoal", name]).unwrap().command,
                Some(Command::Custom(_))
            ));
        }
    }
}
