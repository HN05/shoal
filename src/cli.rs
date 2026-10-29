pub(crate) mod agents;
pub mod args;
pub mod availability;
pub mod client;
pub mod commands;
pub mod completion;
pub mod context;
pub mod forge_hosts;
mod help;
pub(crate) mod installed;
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

use crate::cli::args::{
    agents::AgentsCommand,
    approvals::{ApprovalsArgs, ApprovalsCommand},
    changes::ChangesCommand,
    coordination::CoordinationCommand,
    create::CreateCommand,
    diagnostics::DiagnosticsCommand,
    integration::IntegrationCommand,
    internal::InternalCommand,
    items::ItemsCommand,
    lifecycle::LifecycleCommand,
    repository::RepositoryCommand,
    resources::ResourcesCommand,
    service::{ConfigCommand, DaemonCommand, ServiceCommand},
    sessions::SessionsCommand,
    workspace::{WorkCommand, WorkspaceCommand},
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
//
// A domain's commands form a group enum flattened into this one, so they stay
// top-level commands. Completion offers commands in the order they are
// declared, so each group holds commands that are adjacent in that order.
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
    #[command(flatten)]
    Integration(IntegrationCommand),
    #[command(flatten)]
    Repository(RepositoryCommand),
    #[command(flatten)]
    Create(CreateCommand),
    #[command(flatten)]
    Items(ItemsCommand),
    #[command(flatten)]
    Sessions(SessionsCommand),
    #[command(flatten)]
    Coordination(CoordinationCommand),
    #[command(flatten)]
    Workspace(WorkspaceCommand),
    #[command(flatten)]
    Changes(ChangesCommand),
    /// Commands Shoal, its agent hooks and integrations run, not part of the
    /// interface for people.
    #[command(name = internal::GROUP, hide = true)]
    Internal {
        #[command(subcommand)]
        command: InternalCommand,
    },
    #[command(flatten)]
    Resources(ResourcesCommand),
    /// Inspect approval requests and choose or submit a decision.
    #[command(args_conflicts_with_subcommands = true)]
    Approvals {
        #[command(subcommand)]
        command: Option<ApprovalsCommand>,
        #[command(flatten)]
        args: ApprovalsArgs,
    },
    #[command(flatten)]
    Diagnostics(DiagnosticsCommand),
    #[command(flatten)]
    Lifecycle(LifecycleCommand),
    #[command(flatten)]
    Agents(AgentsCommand),
    #[command(flatten)]
    Service(ServiceCommand),
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
            Command::Workspace(WorkspaceCommand::Cd { .. }) => "cd".into(),
            Command::Sessions(SessionsCommand::Attach { .. }) => "attach".into(),
            Command::Sessions(SessionsCommand::Demo { .. }) => "demo".into(),
            Command::Workspace(WorkspaceCommand::Edit { .. }) => "edit".into(),
            Command::Changes(ChangesCommand::Diff { .. }) => "diff".into(),
            Command::Workspace(WorkspaceCommand::Status { .. }) => "status".into(),
            Command::Run {
                name: None, args, ..
            } if !args.is_empty() => "run".into(),
            Command::Agents(AgentsCommand::Claude { .. }) => "claude".into(),
            Command::Agents(AgentsCommand::Codex { .. }) => "codex".into(),
            Command::Agents(AgentsCommand::T3 { .. }) => "t3".into(),
            #[cfg(feature = "happy")]
            Command::Agents(AgentsCommand::Happy { .. }) => "happy".into(),
            Command::Run {
                name: Some(name), ..
            } => name.clone(),
            Command::Custom(words) => words.first()?.to_str()?.to_owned(),
            _ => return None,
        })
    }

    /// Whether the command runs an execution wrapper in a place a terminal
    /// host or the daemon started for it, which outlives what it runs and
    /// never replaces itself with another program.
    pub fn wraps_execution(&self) -> bool {
        matches!(
            self,
            Command::Internal {
                command: InternalCommand::Add { .. }
                    | InternalCommand::Session { .. }
                    | InternalCommand::Detached { .. }
                    | InternalCommand::Demo { .. }
            }
        )
    }

    /// Lifecycle and service administration is denied to scoped workspace
    /// processes; everything else is ordinary workspace work.
    pub fn is_administrative(&self) -> bool {
        matches!(
            self,
            Command::Create(CreateCommand::Contribute(_))
                | Command::Workspace(WorkspaceCommand::Work {
                    command: Some(WorkCommand::Setup { skip: true, .. }),
                    ..
                })
                | Command::Lifecycle(LifecycleCommand::Cleanup { .. })
                | Command::Service(
                    ServiceCommand::Update
                        | ServiceCommand::Config {
                            command: ConfigCommand::Install { .. }
                                | ConfigCommand::Reset
                                | ConfigCommand::Set { .. }
                                | ConfigCommand::Unset { .. }
                        }
                        | ServiceCommand::Daemon {
                            command: DaemonCommand::Install { .. }
                                | DaemonCommand::Run { .. }
                                | DaemonCommand::Start
                                | DaemonCommand::Stop
                                | DaemonCommand::Restart
                                | DaemonCommand::Reload
                        }
                )
        )
    }
}

#[cfg(test)]
mod tests;
