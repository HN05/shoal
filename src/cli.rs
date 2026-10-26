pub(crate) mod agents;
pub mod args;
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

use clap::{Parser, Subcommand};

use crate::{
    agent::Agent,
    cli::args::{
        AgentParser, ConfirmationArgs, WorkspaceScope, WorkspaceTargets,
        approvals::{ApprovalsArgs, ApprovalsCommand},
        changes::{IssueCommand, PrCommand},
        coordination::{MessagesCommand, QuestionsArgs, QuestionsCommand},
        create::ContributeArgs,
        diagnostics::AlertsCommand,
        integration::{ShellCommand, SkillCommand, SkillName},
        internal::InternalCommand,
        item_reference,
        items::{ItemArgs, RelationArgs},
        lifecycle::{CleanupArgs, CleanupKind, ExportArgs, ImportArgs},
        repository::{ForgeCommand, RepoCommand},
        resources::{AcquireKind, CatalogKind, HistoryKind, LeaseKind, ReleaseKind},
        service::{ConfigCommand, DaemonCommand},
        sessions::DemoCommand,
        workspace::WorkCommand,
    },
};

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
    /// is exported without another removal. `-` writes to standard output. When -o names a
    /// directory, several workspaces, or with --all every workspace of the repository, go to a
    /// file each in it, once each one can be exported.
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
                | Command::Work {
                    command: Some(WorkCommand::Setup { skip: true, .. }),
                    ..
                }
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

#[cfg(test)]
mod tests;
