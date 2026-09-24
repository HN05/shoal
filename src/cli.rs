mod agents;
pub mod client;
pub mod commands;
pub mod completion;
pub mod context;
pub mod output;
mod progress;
pub mod ui;
pub mod workspace_context;

use std::{ffi::OsString, path::PathBuf};

use clap::{Args, Parser, Subcommand, ValueEnum};

use crate::{happy::HappyAgent, state::states};

#[derive(Debug, Parser)]
#[command(
    version,
    about,
    after_help = "Configured commands: shoal run <name> [workspace] -- [args], or the shorthand shoal <name> [workspace] -- [args]. Run `shoal run` to list them."
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

#[derive(Debug, Subcommand)]
pub enum Command {
    /// List or run commands defined in [commands].
    Run {
        name: Option<String>,
        workspace: Option<String>,
        #[arg(last = true, requires = "name")]
        args: Vec<OsString>,
    },
    /// Run a command defined in [commands].
    #[command(external_subcommand)]
    Custom(Vec<OsString>),
    /// Print the bundled agent skill, or install it at user scope.
    Skill {
        #[command(subcommand)]
        command: Option<SkillCommand>,
    },
    /// Print shell completions for commands, flags, and live targets.
    Completions {
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },
    /// Print shell integration for Bash or Zsh.
    Shell {
        #[command(subcommand)]
        command: ShellCommand,
    },
    /// Register repositories and list recently used repositories.
    Repo {
        #[command(subcommand)]
        command: RepoCommand,
    },
    /// Create a named worktree from a registered repository.
    Add {
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
    /// Take ownership of an existing linked worktree, including normal automatic cleanup.
    Adopt {
        /// Registered repository that owns the linked worktree.
        repository: String,
        /// Existing worktree root; files and Git settings are preserved, setup is skipped.
        path: PathBuf,
    },
    /// Create a workspace for an issue number or URL and start an agent.
    Issue {
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
    /// Run or retry the configured workspace setup command.
    Setup { workspace: Option<String> },
    /// List managed workspaces.
    List,
    /// Summarize what the current or named workspace is doing.
    Status { workspace: Option<String> },
    /// Pick a workspace with fzf, enter a named workspace, or use - for the previous directory.
    Cd { workspace: Option<String> },
    /// Show your changes since the fork point using native Git diff configuration.
    Diff { workspace: Option<String> },
    /// Review changes since the fork point manually or with an agent.
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
    /// Merge a local or remote branch into this workspace's own branch.
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
    /// Merge this workspace's branch into the repository default branch locally, without pushing.
    Land { workspace: Option<String> },
    #[command(hide = true)]
    LandInternal { plan: String },
    /// Internal worker launched through the tracked execution wrapper.
    #[command(hide = true)]
    MergeInternal {
        branch: String,
        #[arg(long)]
        remote: Option<String>,
        #[arg(long)]
        local: bool,
    },
    /// Watch, acknowledge, or clear PR cleanup for a workspace.
    #[command(arg_required_else_help = true, args_conflicts_with_subcommands = true)]
    Pr {
        /// GitHub or Forgejo PR number or URL to watch.
        #[arg(value_name = "NUMBER_OR_URL")]
        url: Option<String>,
        workspace: Option<String>,
        #[command(subcommand)]
        command: Option<PrCommand>,
    },
    /// List, acquire, and release named TCP ports owned by a worktree.
    #[command(args_conflicts_with_subcommands = true)]
    Port {
        #[command(subcommand)]
        command: Option<PortCommand>,
        /// Show the effective configuration and reservations for this workspace.
        workspace: Option<String>,
        /// Show every managed workspace.
        #[arg(long, conflicts_with = "workspace")]
        all: bool,
    },
    /// Review, approve, or deny requests for resource access.
    Access {
        #[command(subcommand)]
        command: Option<AccessCommand>,
    },
    /// Acquire, list, and release cooperative resource permits.
    #[command(args_conflicts_with_subcommands = true)]
    Resource {
        #[command(subcommand)]
        command: Option<ResourceCommand>,
        /// Show effective capacity and leases for this workspace.
        workspace: Option<String>,
        /// Show every managed workspace.
        #[arg(long, conflicts_with = "workspace")]
        all: bool,
    },
    /// Share Shoal-managed Xcode simulators between worktrees.
    #[command(args_conflicts_with_subcommands = true)]
    Sim {
        #[command(subcommand)]
        command: Option<SimCommand>,
        /// Show configured profiles, capacity, and devices for this workspace.
        workspace: Option<String>,
        /// Show every managed device.
        #[arg(long, conflicts_with = "workspace")]
        all: bool,
    },
    /// Inspect a workspace and its executions.
    Inspect { workspace: Option<String> },
    /// Show what happened while you were away: conflicts, finished agents, removed workspaces.
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
        workspace: Option<String>,
        #[arg(long, conflicts_with = "workspace")]
        all: bool,
        /// Apply safe state repairs; preserve files, branches, and resource leases.
        #[arg(long)]
        repair: bool,
        /// Stop connected commands and identity-verified surviving processes.
        #[arg(long, requires = "repair")]
        stop: bool,
        /// Confirm untracked/legacy processes have stopped; visible survivors still block repair.
        #[arg(long, requires = "repair")]
        acknowledge_stopped: bool,
    },
    /// Stop managed commands and verified survivors, preserving the workspace.
    Stop { workspace: Option<String> },
    /// Remove a worktree and its redundant branch.
    ///
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
    /// Execute a command in a named or current workspace.
    Exec {
        workspace: Option<String>,
        #[arg(last = true, required = true)]
        command: Vec<OsString>,
    },
    /// Run Claude with remote control named after the workspace.
    Claude {
        workspace: Option<String>,
        #[arg(last = true)]
        args: Vec<OsString>,
    },
    /// Run the Codex CLI or open a workspace in the Codex app.
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
    /// Open a workspace in the running T3 Code desktop app.
    T3 {
        workspace: Option<String>,
        #[arg(last = true)]
        args: Vec<OsString>,
    },
    /// Start a detached Happy session (Claude Code or Codex) that appears in the Happy app.
    Happy {
        #[arg(value_enum)]
        agent: HappyAgent,
        workspace: Option<String>,
        /// First message for the session; delivered through Happy's server when the agent takes no prompt argument.
        #[arg(long)]
        prompt: Option<String>,
        #[arg(last = true)]
        args: Vec<OsString>,
    },
    /// Internal detached execution wrapper; reports the launch on stdout, then keeps tracking it.
    #[command(hide = true)]
    DetachedInternal {
        workspace: String,
        #[arg(long)]
        log: PathBuf,
        #[arg(long)]
        agent: Option<String>,
        #[arg(last = true, required = true)]
        command: Vec<OsString>,
    },
    /// Inspect effective settings and edit global or saved repository config.
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
pub enum PrCommand {
    /// Confirm this commit was merged; stop tracked commands and remove the workspace.
    Merged { workspace: Option<String> },
    /// Cancel PR cleanup for this workspace.
    Clear { workspace: Option<String> },
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
                }
        )
    }
}

states!(
    #[derive(Default)]
    CodexMode {
        #[default]
        Cli => "cli",
        App => "app",
    }
);

/// What `add --agent` starts: a terminal agent, or a detached Happy session
/// running one of Happy's agents, or a user-configured command.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(try_from = "String", into = "String")]
pub enum Agent {
    Codex,
    Claude,
    Happy(HappyAgent),
    Custom(String),
}

impl Agent {
    /// Built-in agent spellings, in help and completion order.
    pub fn possible_values() -> Vec<String> {
        let mut values = vec!["codex".to_owned(), "claude".to_owned()];
        values.extend(
            HappyAgent::value_variants()
                .iter()
                .filter_map(|agent| agent.to_possible_value())
                .map(|value| format!("{}{}", HAPPY_PREFIX, value.get_name())),
        );
        values
    }
}

const HAPPY_PREFIX: &str = "happy-";

impl std::str::FromStr for Agent {
    type Err = ();

    fn from_str(value: &str) -> Result<Self, ()> {
        match value {
            "codex" => Ok(Agent::Codex),
            "claude" => Ok(Agent::Claude),
            _ if value.starts_with(HAPPY_PREFIX) => value
                .strip_prefix(HAPPY_PREFIX)
                .and_then(|agent| HappyAgent::from_str(agent, false).ok())
                .map(Agent::Happy)
                .ok_or(()),
            _ if value != "happy" && crate::validate::lowercase_name("agent", value).is_ok() => {
                Ok(Agent::Custom(value.to_owned()))
            }
            _ => Err(()),
        }
    }
}

/// Config spelling: the same values `--agent` accepts.
impl TryFrom<String> for Agent {
    type Error = String;

    fn try_from(value: String) -> Result<Self, String> {
        value.parse().map_err(|()| {
            format!(
                "invalid agent {value:?}; use a configured command name or one of {}",
                Agent::possible_values().join(", ")
            )
        })
    }
}

impl From<Agent> for String {
    fn from(agent: Agent) -> Self {
        match agent {
            Agent::Codex => "codex".into(),
            Agent::Claude => "claude".into(),
            Agent::Happy(agent) => format!("{HAPPY_PREFIX}{}", agent.name()),
            Agent::Custom(name) => name,
        }
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
        workspace: Option<String>,
        #[arg(long, conflicts_with = "workspace")]
        all: bool,
    },
    Release {
        name: String,
        workspace: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
pub enum ShellCommand {
    Init,
}

#[derive(Debug, Subcommand)]
pub enum ConfigCommand {
    /// Set a config value using a TOML dotted key (global by default).
    Set {
        key: String,
        /// A TOML value, or an unquoted string.
        value: String,
        /// Edit this repository's saved config instead of the global file.
        #[arg(long = "repo")]
        repository: Option<String>,
    },
    /// Remove a config key or table, falling back to lower layers or defaults.
    Unset {
        key: String,
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
        workspace: Option<String>,
        #[arg(long, conflicts_with = "workspace")]
        all: bool,
    },
    /// Acquire exclusive use; reuse the same named lease on repeated requests.
    Acquire {
        workspace: Option<String>,
        #[arg(long, default_value = "default")]
        name: String,
        #[arg(long, conflicts_with_all = ["device", "runtime"])]
        profile: Option<String>,
        #[arg(long, requires = "runtime")]
        device: Option<String>,
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
        workspace: Option<String>,
        #[arg(long, conflicts_with = "workspace")]
        all: bool,
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
    /// Acquire a permit or reader/writer lock for an available or specific member.
    Acquire {
        pool: String,
        workspace: Option<String>,
        #[arg(long)]
        resource: Option<String>,
        /// Lock mode (defaults to permit for semaphores, write for rwlocks).
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
        workspace: Option<String>,
        #[arg(long, conflicts_with = "workspace")]
        all: bool,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn doctor_replaces_reconcile_without_an_alias() {
        use clap::CommandFactory;
        let command = Cli::command();
        assert!(command.find_subcommand("doctor").is_some());
        assert!(command.find_subcommand("reconcile").is_none());
        for flag in ["--stop", "--acknowledge-stopped"] {
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
                    command: RepoCommand::Rm {
                        confirmation: ConfirmationArgs { yes: true },
                        ..
                    }
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
    fn pr_workspace_names_can_match_action_names() {
        for workspace_name in ["merged", "clear", "review"] {
            let cli = Cli::try_parse_from([
                "shoal",
                "pr",
                "https://example.test/owner/repo/pulls/1",
                workspace_name,
            ])
            .unwrap();
            let Some(Command::Pr {
                url,
                workspace,
                command,
            }) = cli.command
            else {
                panic!("wrong command")
            };
            assert_eq!(
                url.as_deref(),
                Some("https://example.test/owner/repo/pulls/1")
            );
            assert_eq!(workspace.as_deref(), Some(workspace_name));
            assert!(command.is_none());
        }
    }

    #[test]
    fn pr_actions_remain_subcommands() {
        let merged = Cli::try_parse_from(["shoal", "pr", "merged", "workspace"]).unwrap();
        assert!(matches!(
            merged.command,
            Some(Command::Pr {
                command: Some(PrCommand::Merged { workspace }),
                ..
            }) if workspace.as_deref() == Some("workspace")
        ));

        let clear = Cli::try_parse_from(["shoal", "pr", "clear", "workspace"]).unwrap();
        assert!(matches!(
            clear.command,
            Some(Command::Pr {
                command: Some(PrCommand::Clear { workspace }),
                ..
            }) if workspace.as_deref() == Some("workspace")
        ));

        let review = Cli::try_parse_from(["shoal", "pr", "review", "7", "--agent", "claude"]);
        assert!(matches!(
            review.unwrap().command,
            Some(Command::Pr {
                command: Some(PrCommand::Review { url, agent: Some(Agent::Claude), .. }),
                ..
            }) if url == "7"
        ));
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
