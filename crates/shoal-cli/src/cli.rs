pub(crate) mod agents;
pub mod args;
pub mod client;
pub mod commands;
pub mod completion;
pub mod context;
mod desktop;
pub(crate) mod entry;
mod everywhere;
mod help;
pub mod internal;
pub mod list_status;
pub mod output;
mod progress;
pub mod recent;
pub mod shell;
mod supervision;
pub mod ui;
pub mod usage;

use std::{ffi::OsString, path::PathBuf};

use clap::{Parser, Subcommand};

use crate::cli::args::{
    approvals::{ApprovalsArgs, ApprovalsCommand},
    changes::ChangesCommand,
    coordination::CoordinationCommand,
    create::CreateCommand,
    hub::HubCommand,
    integration::IntegrationCommand,
    internal::InternalCommand,
    items::ItemsCommand,
    lifecycle::LifecycleCommand,
    repository::{RepoCommand, RepositoryCommand},
    resources::ResourcesCommand,
    service::{ConfigCommand, DaemonCommand, ServiceCommand},
    sessions::SessionsCommand,
    space::SpaceCommand,
    workspace::WorkspaceCommand,
};
use crate::config::permissions::PermissionKey;
use shoal_protocol::space_names::SpaceSelector;

#[derive(Debug, Parser)]
#[command(
    name = "shoal",
    version,
    about,
    disable_help_subcommand = true,
    help_template = help::template(),
    after_help = "Start here (with a registered repository):\n  shoal start my-project --branch fix-login\n  shoal run --workspace 3 -- cargo test\n  shoal status 3\n\nRun `shoal run` to list configured commands.\nUse `shoal <command> --help` for details."
)]
pub struct Cli {
    #[arg(skip)]
    pub(crate) daemon_handoff: Option<crate::daemon_handoff::Handoff>,
    /// The subcommand path, such as `["repo", "rm"]`.
    #[arg(skip)]
    pub(crate) path: Vec<String>,
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
// them: the whole tree takes megabytes that would stay resident in each
// long-lived process, such as an agent's wrapper. A command's attributes
// apply before its deferred arguments, so they cannot change them. An
// argument struct's doc comment would replace the command's description.
// Code that reads arguments from the tree builds it first.
//
// Completion offers commands in the order they are declared, so each
// flattened group holds commands that are adjacent in that order.
#[derive(Debug, Subcommand)]
#[command(defer = true)]
pub enum Command {
    /// List configured commands, run one by name, or run an arbitrary command after --.
    Run {
        /// Configured command; omit it to execute the arguments after -- directly.
        name: Option<String>,
        /// Workspace to use; defaults to the current workspace or picker.
        #[arg(short = 'w', long)]
        workspace: Option<SpaceSelector>,
        #[arg(last = true)]
        args: Vec<OsString>,
    },
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
    Space(SpaceCommand),
    #[command(flatten)]
    Workspace(WorkspaceCommand),
    #[command(flatten)]
    Changes(ChangesCommand),
    /// Commands Shoal, its agent hooks and integrations run, not part of the
    /// interface for people.
    #[command(name = crate::workers::GROUP, hide = true)]
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
    Lifecycle(LifecycleCommand),
    #[command(flatten)]
    Service(ServiceCommand),
}

impl Cli {
    /// Parse the process arguments as `Cli::parse` does, and also record the
    /// subcommand path and resolve the output format.
    pub fn from_args() -> Self {
        let mut matches = <Self as clap::CommandFactory>::command()
            .try_get_matches()
            .unwrap_or_else(|error| exit_parse(error, std::env::args_os()));
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
        cli.path = path;
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

fn exit_parse(error: clap::Error, args: impl IntoIterator<Item = OsString>) -> ! {
    if !error.use_stderr() {
        output::print_clap_output(&error);
        std::process::exit(error.exit_code())
    }
    if !names_unknown_command(&error, args) {
        error.exit()
    }
    // A failed print leaves nothing else to report it to.
    let _ = error.print();
    eprintln!("{CONFIGURED_COMMANDS}");
    std::process::exit(error.exit_code())
}

const CONFIGURED_COMMANDS: &str = "configured commands run with `shoal run <name>`";

fn names_unknown_command(error: &clap::Error, args: impl IntoIterator<Item = OsString>) -> bool {
    // With any name accepted at the top level, only a deeper name stays
    // refused.
    error.kind() == clap::error::ErrorKind::InvalidSubcommand
        && <Cli as clap::CommandFactory>::command()
            .allow_external_subcommands(true)
            .try_get_matches_from(args)
            .is_ok()
}

impl Command {
    /// The name under which the command records the spaces it acts on, so that
    /// `-` selects the previous one.
    pub fn recent_name(&self) -> Option<String> {
        Some(match self {
            Command::Workspace(WorkspaceCommand::Cd { .. }) => "cd".into(),
            Command::Sessions(SessionsCommand::Attach { .. }) => {
                shoal_workers::history::ATTACH.into()
            }
            Command::Sessions(SessionsCommand::Demo { .. }) => "demo".into(),
            Command::Workspace(WorkspaceCommand::Edit { .. }) => "edit".into(),
            Command::Space(SpaceCommand::Open { .. }) => "open".into(),
            Command::Changes(ChangesCommand::Diff { .. }) => "diff".into(),
            Command::Space(SpaceCommand::Status { .. }) => "status".into(),
            Command::Run {
                name: None, args, ..
            } if !args.is_empty() => "run".into(),
            Command::Run {
                name: Some(name), ..
            } => name.clone(),
            _ => return None,
        })
    }

    /// Whether the command runs an execution wrapper, which outlives what it
    /// runs and never replaces itself with another program.
    pub fn wraps_execution(&self) -> bool {
        matches!(
            self,
            Command::Internal {
                command: InternalCommand::Place { .. }
                    | InternalCommand::Session { .. }
                    | InternalCommand::Detached { .. }
                    | InternalCommand::Demo { .. }
            }
        )
    }

    pub fn runs_daemon(&self) -> bool {
        matches!(
            self,
            Command::Service(ServiceCommand::Daemon {
                command: DaemonCommand::Run { .. }
            })
        )
    }

    /// Whether the command replaces a config file. A scoped process cannot
    /// check that the new file narrows its permissions, so it never runs one.
    pub fn rewrites_config(&self) -> bool {
        matches!(
            self,
            Command::Service(ServiceCommand::Config {
                command: ConfigCommand::Install { .. } | ConfigCommand::Reset,
            })
        )
    }

    pub fn belongs_to_user(&self) -> bool {
        matches!(
            self,
            Command::Service(
                ServiceCommand::Config {
                    command: ConfigCommand::Set { shared: true, .. }
                        | ConfigCommand::Unset { shared: true, .. },
                } | ServiceCommand::Hub {
                    command: HubCommand::Pair
                        | HubCommand::Join(_)
                        | HubCommand::Leave
                        | HubCommand::Revoke { .. },
                } | ServiceCommand::Daemon {
                    command: DaemonCommand::Connect,
                }
            )
        )
    }

    /// The registered repository the command names. Its config resolves the
    /// permission of a chat without a repository.
    pub fn repository(&self) -> Option<&str> {
        match self {
            Command::Repository(RepositoryCommand::Repo {
                command:
                    Some(
                        RepoCommand::Config { repository, .. }
                        | RepoCommand::Rename { repository, .. }
                        | RepoCommand::Setup { repository, .. }
                        | RepoCommand::Rm { repository, .. },
                    ),
            }) => repository.as_deref(),
            Command::Lifecycle(LifecycleCommand::Cleanup { args, .. }) => args.repo.as_deref(),
            _ => None,
        }
    }

    pub fn permission(&self) -> Option<PermissionKey> {
        match self {
            Command::Service(
                ServiceCommand::Update { .. }
                | ServiceCommand::Daemon { .. }
                | ServiceCommand::Hub {
                    command: HubCommand::Update,
                },
            ) => Some(PermissionKey::Daemon),
            Command::Create(CreateCommand::Contribute(_))
            | Command::Repository(RepositoryCommand::Repo {
                command:
                    Some(
                        RepoCommand::Setup { .. }
                        | RepoCommand::Add { .. }
                        | RepoCommand::Rename { .. }
                        | RepoCommand::Rm { .. }
                        | RepoCommand::Move(_)
                        | RepoCommand::Config { .. },
                    ),
            })
            | Command::Workspace(WorkspaceCommand::Setup { skip: true, .. })
            | Command::Lifecycle(LifecycleCommand::Cleanup { .. })
            | Command::Service(ServiceCommand::Config {
                command:
                    ConfigCommand::Install { .. } | ConfigCommand::Reset | ConfigCommand::Setup(_),
            }) => Some(PermissionKey::Admin),
            Command::Sessions(SessionsCommand::Restore { import, .. })
                if !import.from.is_empty() =>
            {
                Some(PermissionKey::Admin)
            }
            // A move to a joined machine stays within the hub, so it takes
            // no permission, as a rename does.
            Command::Lifecycle(LifecycleCommand::Move(args)) if args.machine.is_none() => {
                Some(PermissionKey::Admin)
            }
            Command::Create(CreateCommand::Start(args)) if args.now => Some(PermissionKey::Limits),
            Command::Sessions(SessionsCommand::Restore { now, focus, .. }) if *now || *focus => {
                Some(PermissionKey::Limits)
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests;
