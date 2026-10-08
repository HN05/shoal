mod agents;
pub mod client;
pub mod commands;
pub mod completion;
pub mod context;
mod help;
pub(crate) mod herdr;
pub mod internal;
pub mod output;
mod progress;
pub mod ui;
pub mod workspace_context;

use std::{ffi::OsString, path::PathBuf};

use clap::{Args, Parser, Subcommand, ValueEnum};

use crate::agent::{Agent, BuiltinAgent};

#[derive(Debug, Parser)]
#[command(
    version,
    about,
    help_template = help::template(),
    after_help = "Start here (with a registered repository):\n  shoal add my-project fix-login\n  shoal exec fix-login -- cargo test\n  shoal status fix-login\n\nRun `shoal run` to list configured commands.\nUse `shoal <command> --help` for details."
)]
pub struct Cli {
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
    #[arg(long, conflicts_with = "workspace")]
    pub all: bool,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// List or run configured commands.
    Run {
        name: Option<String>,
        workspace: Option<String>,
        #[arg(last = true, requires = "name")]
        args: Vec<OsString>,
    },
    /// Run a command defined in [commands].
    #[command(external_subcommand)]
    Custom(Vec<OsString>),
    /// Show or install Shoal instructions for agents.
    Skill {
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
        /// Registered repository; may be omitted when --issue is a URL.
        repository: Option<String>,
        /// Git branch name; a portable workspace name is derived from it.
        branch: Option<String>,
        /// Use an existing local branch or remote/branch without creating a new branch.
        #[arg(long, conflicts_with_all = ["branch", "issue"])]
        existing: Option<String>,
        /// Derive a name and agent prompt from a forge issue number or URL.
        #[arg(long)]
        issue: Option<String>,
        /// Create this worktree at an exact new directory instead of the repository default.
        #[arg(long)]
        path: Option<PathBuf>,
        /// Starting Git ref (defaults to the repository's default branch, refreshed from its upstream);
        /// with --existing, the ref its changes are compared against.
        #[arg(long, value_name = "REF")]
        base: Option<String>,
        /// Apply a named Git profile to the new worktree, overriding repository defaults.
        #[arg(long)]
        git_profile: Option<String>,
        /// Start a built-in agent or a configured command after worktree creation.
        #[arg(long, value_parser = AgentParser)]
        agent: Option<Agent>,
        /// Arguments forwarded to the agent.
        #[arg(last = true, requires = "agent")]
        args: Vec<OsString>,
    },
    /// Bring an existing worktree under Shoal management, including cleanup.
    Adopt {
        /// Registered repository that owns the linked worktree.
        repository: String,
        /// Existing worktree root; files and Git settings are preserved, setup is skipped.
        path: PathBuf,
    },
    /// Rename a workspace's Git branch and derived workspace name.
    #[command(allow_missing_positional = true)]
    Rename {
        workspace: Option<String>,
        /// New literal Git branch name.
        branch: String,
    },
    /// Open an issue workspace and start an agent.
    Issue {
        /// Run in the current pane instead of opening a Herdr tab.
        #[arg(long)]
        here: bool,
        /// Forge issue number or URL.
        issue: String,
        /// Registered repository; defaults to the URL's repository or the current checkout/workspace.
        #[arg(long = "repo")]
        repository: Option<String>,
        /// Agent to start; defaults to the repository or global `default_agent`.
        #[arg(long, value_parser = AgentParser)]
        agent: Option<Agent>,
        /// Starting Git ref (defaults to the repository's default branch, refreshed from its upstream).
        #[arg(long, value_name = "REF")]
        base: Option<String>,
        /// Arguments forwarded to the agent.
        #[arg(last = true)]
        args: Vec<OsString>,
    },
    /// Stop tracked agents and save their sessions for manual resume.
    Pause {
        workspace: Option<String>,
        /// Pause only this tracked execution; defaults to all connected agents.
        #[arg(long)]
        execution: Option<String>,
    },
    /// Restore a paused agent or one stopped by overload protection.
    Resume {
        workspace: Option<String>,
        /// Select a stopped execution when the workspace has several records.
        #[arg(long)]
        execution: Option<String>,
        /// Discard a stopped agent's saved recovery record.
        #[arg(long)]
        discard: bool,
    },
    /// Run or retry workspace setup.
    Setup { workspace: Option<String> },
    /// Continue working until explicit done, deferring issue, PR and idle cleanup.
    Continue { workspace: Option<String> },
    /// Mark the assignment finished; by default stop tracked commands and clean up safely.
    Done {
        workspace: Option<String>,
        /// Keep the workspace for review, overriding [done] cleanup and automatic cleanup.
        #[arg(long, conflicts_with = "cleanup")]
        keep: bool,
        /// Request cleanup even when [done] cleanup is false; preserve dirty or unpushed work.
        #[arg(long)]
        cleanup: bool,
    },
    /// Hold a workspace against automatic cleanup.
    #[command(args_conflicts_with_subcommands = true)]
    Hold {
        #[command(subcommand)]
        command: Option<HoldCommand>,
        #[command(flatten)]
        scope: WorkspaceScope,
    },
    /// List workspaces.
    Ls,
    /// Show workspace activity, changes, and resources.
    Status { workspace: Option<String> },
    /// Enter a workspace, or use - for the previous directory.
    ///
    /// Omit the workspace to open the picker, even inside a workspace.
    Cd { workspace: Option<String> },
    /// Show changes since the branch's fork point.
    Diff { workspace: Option<String> },
    /// Start a review tool or agent to review workspace changes.
    Review {
        workspace: Option<String>,
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
    /// Merge another branch into this workspace.
    Merge {
        branch: String,
        workspace: Option<String>,
        /// Fetch this branch from a specific configured remote, even if it exists locally.
        #[arg(long)]
        remote: Option<String>,
        /// Merge the local branch as it is instead of fast-forwarding it from its upstream first.
        #[arg(long, conflicts_with = "remote")]
        local: bool,
    },
    /// Merge this workspace into the default branch locally, without pushing.
    Land { workspace: Option<String> },
    #[command(name = internal::LAND, hide = true)]
    LandInternal { plan: String },
    #[command(name = internal::HERDR, hide = true)]
    HerdrInternal {
        #[arg(long)]
        close_when_done: bool,
        #[arg(env = crate::env::HERDR_PLAN, hide_env_values = true)]
        plan: String,
    },
    #[command(name = internal::HERDR_WATCH, hide = true)]
    HerdrWatchInternal { workspace: String, tab: String },
    /// Internal worker launched through the tracked execution wrapper.
    #[command(name = internal::MERGE, hide = true)]
    MergeInternal {
        branch: String,
        #[arg(long)]
        remote: Option<String>,
        #[arg(long)]
        local: bool,
    },
    /// Watch PRs, wait for their updates, cancel watches, or review a PR.
    Pr {
        #[command(subcommand)]
        command: PrCommand,
    },
    /// Reserve and release workspace TCP ports.
    #[command(
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
    /// Review, approve, or deny requests for resource access.
    Access {
        #[command(subcommand)]
        command: Option<AccessCommand>,
    },
    /// Acquire and release shared resource permits.
    #[command(
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
    /// Acquire and release Xcode simulators.
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
    /// Stream durable workspace lifecycle events without consuming notifications.
    Events {
        /// Keep the stream open as the daemon records new events.
        #[arg(long)]
        follow: bool,
        /// Replay events after this event ID.
        #[arg(long, value_parser = clap::value_parser!(i64).range(0..))]
        since: Option<i64>,
    },
    /// Show resource conflicts, finished agents, and automatic cleanup.
    Notifications {
        /// Include notifications already shown.
        #[arg(long, conflicts_with = "follow")]
        all: bool,
        /// Keep printing new notifications as the daemon records them.
        #[arg(long)]
        follow: bool,
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
    /// Stop managed commands and keep the workspace.
    Stop { workspace: Option<String> },
    /// Remove a workspace and release its resources.
    ///
    /// Redundant branches are removed; the default branch is retained unless explicitly deleted.
    /// Differing or dirty work still needs a branch choice when confirmation is skipped.
    Rm {
        workspace: Option<String>,
        #[command(flatten)]
        confirmation: ConfirmationArgs,
        /// Remove the worktree but retain its branch, including when it contains work.
        #[arg(long, conflicts_with = "delete_branch")]
        keep_branch: bool,
        /// Remove both worktree and branch, including uncommitted and differing work.
        #[arg(long, conflicts_with = "keep_branch")]
        delete_branch: bool,
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
    /// Start a detached agent session in Happy.
    Happy {
        #[arg(value_enum)]
        agent: BuiltinAgent,
        workspace: Option<String>,
        /// First message for the session; delivered through Happy's server when the agent takes no prompt argument.
        #[arg(long)]
        prompt: Option<String>,
        #[arg(last = true)]
        args: Vec<OsString>,
    },
    /// Internal detached execution wrapper; reports the launch on stdout, then keeps tracking it.
    #[command(name = internal::DETACHED, hide = true)]
    DetachedInternal {
        workspace: String,
        #[arg(long)]
        log: PathBuf,
        #[arg(long)]
        agent: Option<String>,
        #[arg(last = true, required = true)]
        command: Vec<OsString>,
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
    /// Inspect and control the local daemon.
    Daemon {
        #[command(subcommand)]
        command: DaemonCommand,
    },
}

#[derive(Debug, Subcommand)]
pub enum HoldCommand {
    /// Keep a workspace while an external session is open.
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
        #[arg(long)]
        name: String,
    },
    /// List holds on a workspace, or every workspace with --all.
    List {
        #[command(flatten)]
        scope: WorkspaceScope,
    },
}

#[derive(Debug, Subcommand)]
pub enum PrCommand {
    /// Watch a PR; cleanup after done waits until every watched PR merges.
    Watch {
        /// GitHub or Forgejo PR number or URL; repeated watches accumulate.
        #[arg(value_name = "NUMBER_OR_URL")]
        url: String,
        workspace: Option<String>,
    },
    /// Cancel all PR watches, or only the selected PR with --pr.
    Unwatch {
        workspace: Option<String>,
        /// Cancel only this PR; the remaining watches stay active.
        #[arg(long = "pr", value_name = "NUMBER_OR_URL")]
        url: Option<String>,
    },
    /// Wait for the next comment, completed CI check, or merge conflict update.
    Wait {
        workspace: Option<String>,
        /// Stop waiting after this many seconds and return no updates.
        #[arg(long, default_value_t = 3600)]
        timeout: u64,
    },
    /// Open a PR's branch in a workspace and review it manually or with an agent.
    Review {
        /// GitHub or Forgejo PR number or URL.
        #[arg(value_name = "NUMBER_OR_URL")]
        url: String,
        /// Registered repository; defaults to the URL's repository or the current checkout/workspace.
        #[arg(long = "repo")]
        repository: Option<String>,
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
}

impl Command {
    /// Lifecycle and service administration is denied to scoped workspace
    /// processes; everything else is ordinary workspace work.
    pub fn is_administrative(&self) -> bool {
        matches!(
            self,
            Command::Doctor { .. }
                | Command::Install { .. }
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

impl ValueEnum for BuiltinAgent {
    fn value_variants<'a>() -> &'a [Self] {
        Self::ALL
    }

    fn to_possible_value(&self) -> Option<clap::builder::PossibleValue> {
        Some(clap::builder::PossibleValue::new(self.as_str()))
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
        value.parse().map_err(|()| {
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
                clap::error::ContextValue::Strings(Agent::possible_values()),
            );
            error
        })
    }

    fn possible_values(
        &self,
    ) -> Option<Box<dyn Iterator<Item = clap::builder::PossibleValue> + '_>> {
        Some(Box::new(
            Agent::possible_values()
                .into_iter()
                .map(clap::builder::PossibleValue::new),
        ))
    }
}

#[derive(Debug, Subcommand)]
pub enum SkillCommand {
    /// Install or refresh the bundled skill for configured AI tools (no daemon needed).
    Install {
        /// Install for one AI tool, or all configured tools by default.
        #[arg(default_value = "all")]
        agent: String,
    },
}

#[derive(Debug, Subcommand)]
pub enum RepoCommand {
    /// Show or replace repository configuration stored locally by Shoal.
    Config {
        repository: String,
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
        repository: String,
        name: String,
    },
    /// Delete a repository checkout and all its Shoal workspaces and resources.
    ///
    /// Uncommitted and unpushed work is permanently lost.
    #[command(alias = "remove")]
    Rm {
        repository: String,
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
        #[arg(value_parser = clap::builder::PossibleValuesParser::new(
            crate::config::PACKAGED.iter().map(|(name, _)| *name)
        ))]
        name: String,
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
    List {
        #[command(flatten)]
        scope: WorkspaceScope,
    },
    /// Acquire exclusive use; reuse the same named lease on repeated requests.
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
        /// Wait this many seconds for capacity (0 returns busy immediately).
        #[arg(long, default_value_t = 0, value_parser = clap::value_parser!(u64).range(0..=3600))]
        wait: u64,
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
    Release {
        #[arg(default_value = "default")]
        name: String,
        workspace: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
pub enum AccessCommand {
    /// List requests and retained grants (scoped callers see their own workspace).
    List { workspace: Option<String> },
    /// Approve the exact settings recorded in a request; does not allocate capacity.
    Approve { id: String },
    /// Deny a pending request.
    Deny { id: String },
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
        #[arg(long, default_value_t = 0, value_parser = clap::value_parser!(u64).range(0..=3600))]
        wait: u64,
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
        for (spelling, expected) in [
            ("codex", Agent::Codex),
            ("claude", Agent::Claude),
            ("happy-claude", Agent::Happy(BuiltinAgent::Claude)),
            ("happy-codex", Agent::Happy(BuiltinAgent::Codex)),
            ("my-agent_2", Agent::Custom("my-agent_2".into())),
            ("t3", Agent::Custom("t3".into())),
        ] {
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
        for invalid in [
            "happy",
            "happy-other",
            "happy-",
            "Codex",
            "happy-Codex",
            "",
            "two words",
        ] {
            assert!(Cli::try_parse_from(["shoal", "add", "--agent", invalid]).is_err());
            let text = format!("default_agent = {invalid:?}");
            assert!(toml::from_str::<crate::config::Config>(&text).is_err());
            assert!(toml::from_str::<crate::config::repo::RepoConfig>(&text).is_err());
            assert!(serde_json::from_value::<Agent>(invalid.into()).is_err());
        }
    }

    #[test]
    fn agent_parsers_preserve_completion_order() {
        use clap::builder::TypedValueParser;
        let names: Vec<_> = AgentParser
            .possible_values()
            .unwrap()
            .map(|value| value.get_name().to_owned())
            .collect();
        assert_eq!(names, ["codex", "claude", "happy-claude", "happy-codex"]);
        let names: Vec<_> = BuiltinAgent::value_variants()
            .iter()
            .map(|agent| agent.to_possible_value().unwrap().get_name().to_owned())
            .collect();
        assert_eq!(names, ["claude", "codex"]);
        for (name, expected) in [
            ("claude", BuiltinAgent::Claude),
            ("codex", BuiltinAgent::Codex),
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
            vec!["shoal", "issue", "122", "--base", "HEAD~1"],
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
                Command::Add { base, .. } | Command::Issue { base, .. } => base,
                _ => panic!("wrong command"),
            };
            assert_eq!(base.as_deref(), Some(expected));
        }
    }

    #[test]
    fn done_flags_override_the_default_and_remain_available_to_agents() {
        for (args, expected) in [
            (vec!["shoal", "done"], (false, false)),
            (vec!["shoal", "done", "--keep"], (true, false)),
            (vec!["shoal", "done", "--cleanup"], (false, true)),
        ] {
            let command = Cli::try_parse_from(args).unwrap().command.unwrap();
            assert!(!command.is_administrative());
            let Command::Done {
                keep,
                cleanup,
                workspace,
            } = command
            else {
                panic!("wrong command");
            };
            assert_eq!((keep, cleanup), expected);
            assert!(workspace.is_none());
        }
        assert!(Cli::try_parse_from(["shoal", "done", "--keep", "--cleanup"]).is_err());
        assert!(
            matches!(Cli::try_parse_from(["shoal", "done", "review", "--keep"]).unwrap().command,
            Some(Command::Done { workspace: Some(name), keep: true, .. }) if name == "review")
        );
    }

    #[test]
    fn pr_actions_are_explicit_and_workspace_names_remain_literal() {
        for workspace_name in ["watch", "unwatch", "review", "merged", "clear"] {
            let cli = Cli::try_parse_from(["shoal", "pr", "watch", "7", workspace_name]).unwrap();
            assert!(matches!(cli.command,
                Some(Command::Pr { command: PrCommand::Watch { url, workspace } })
                if url == "7" && workspace.as_deref() == Some(workspace_name)));
        }
        for args in [
            vec!["shoal", "pr", "7"],
            vec!["shoal", "pr", "clear"],
            vec!["shoal", "pr", "merged"],
            vec!["shoal", "pr", "watch"],
        ] {
            assert!(Cli::try_parse_from(args).is_err());
        }
        for (args, expected) in [
            (vec!["shoal", "pr", "unwatch", "workspace"], None),
            (
                vec!["shoal", "pr", "unwatch", "workspace", "--pr", "7"],
                Some("7"),
            ),
        ] {
            assert!(matches!(Cli::try_parse_from(args).unwrap().command,
                Some(Command::Pr { command: PrCommand::Unwatch { workspace, url } })
                if workspace.as_deref() == Some("workspace") && url.as_deref() == expected));
        }
        let review = Cli::try_parse_from(["shoal", "pr", "review", "7", "--agent", "claude"]);
        assert!(matches!(review.unwrap().command,
            Some(Command::Pr { command: PrCommand::Review { url, agent: Some(Agent::Claude), .. } }) if url == "7"));
        assert!(
            Cli::try_parse_from([
                "shoal", "pr", "review", "7", "--manual", "--agent", "claude"
            ])
            .is_err()
        );
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
}
