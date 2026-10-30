//! CLI dispatch. Domain handlers own requests, prompts, and rendering;
//! daemon modules own lifecycle and allocation policy.
mod acquisition;
mod agent_queue;
mod agent_state;
mod api;
mod approvals;
mod base;
mod branches;
mod capabilities;
mod catalog;
mod chat;
mod ci;
mod cleanup;
mod configuration;
pub(crate) mod configured;
mod contribute;
mod demo;
mod dispatch;
mod events;
mod files;
mod holds;
mod inbox;
pub(super) mod issues;
mod items;
mod labels;
mod leases;
mod links;
mod listing;
pub(crate) mod menu;
mod messages;
mod notifications;
mod plan;
mod ports;
mod questions;
mod queue;
mod reactions;
mod repo_menu;
mod repo_setup;
mod repositories;
mod resources;
mod restart;
mod restore;
pub(crate) mod resume;
mod review;
mod secrets;
mod service;
pub(in crate::cli) mod sessions;
mod simulators;
mod skill;
mod split;
mod stop;
mod subagents;
mod transfer;
mod turns;
mod update;
mod view;
mod work;
pub(in crate::cli) mod workspaces;

pub(crate) use workspaces::queued_agent_plan;
mod worktree_hooks;

#[cfg(test)]
mod tests;

pub(in crate::cli) use approvals::summary as approval_summary;
pub(in crate::cli) use questions::summary as question_summary;

use anyhow::{Result, ensure};
use clap::CommandFactory;
use futures_util::{StreamExt, stream};
use serde::Serialize;
use serde_json::json;

use crate::{
    cli::{
        Cli, Command,
        args::{
            integration::{IntegrationCommand, ShellCommand},
            repository::RepositoryCommand,
            service::{DaemonCommand, ServiceCommand},
            workspace::WorkspaceCommand,
        },
        client,
        context::Context,
        output::{Palette, Style},
    },
    env,
    model::Workspace,
    paths::Paths,
    shell,
};

#[derive(Serialize)]
#[serde(untagged)]
enum WorkspaceOverviewResult<T> {
    Ready(T),
    Failed {
        workspace: Box<crate::model::Workspace>,
        error: String,
    },
}

impl<T> WorkspaceOverviewResult<T> {
    fn failed(workspace: crate::model::Workspace, error: impl std::fmt::Display) -> Self {
        Self::Failed {
            workspace: Box::new(workspace),
            error: error.to_string(),
        }
    }

    fn is_failed(&self) -> bool {
        matches!(self, Self::Failed { .. })
    }
}

/// Collect every workspace before reporting partial failures. Domains retain
/// their successful JSON payload and resource-specific text rendering.
async fn workspace_overviews<T: Serialize>(
    ctx: &Context,
    request: impl AsyncFn(&Workspace) -> Result<T>,
    render: impl Fn(&T, Palette),
) -> Result<i32> {
    let workspaces = client::workspaces(&ctx.paths).await?;
    let overviews = collect_workspace_overviews(&workspaces, request).await;
    let failed = overviews.iter().any(WorkspaceOverviewResult::is_failed);
    ctx.show(&overviews, |overviews| {
        let palette = Palette::stdout(ctx.json);
        for (workspace, overview) in workspaces.iter().zip(overviews) {
            let heading = palette.paint(Style::Heading, &workspace.name);
            match overview {
                WorkspaceOverviewResult::Ready(overview) => {
                    outln!("{heading}");
                    render(overview, palette);
                }
                WorkspaceOverviewResult::Failed { error, .. } => {
                    outln!("{heading}: {}", palette.paint(Style::Warning, error));
                }
            }
        }
    })?;
    Ok(i32::from(failed))
}

// Keep concurrent reads modest: each request opens several SQLite connections.
const WORKSPACE_OVERVIEW_CONCURRENCY: usize = 2;

async fn collect_workspace_overviews<T>(
    workspaces: &[Workspace],
    request: impl AsyncFn(&Workspace) -> Result<T>,
) -> Vec<WorkspaceOverviewResult<T>> {
    let mut pending = stream::iter(workspaces.iter().enumerate())
        .map(|(index, workspace)| {
            let request = &request;
            async move {
                let result = match request(workspace).await {
                    Ok(overview) => WorkspaceOverviewResult::Ready(overview),
                    Err(error) => {
                        WorkspaceOverviewResult::failed(workspace.clone(), format!("{error:#}"))
                    }
                };
                (index, result)
            }
        })
        .buffer_unordered(WORKSPACE_OVERVIEW_CONCURRENCY);
    let mut overviews: Vec<_> = std::iter::repeat_with(|| None)
        .take(workspaces.len())
        .collect();
    while let Some((index, overview)) = pending.next().await {
        overviews[index] = Some(overview);
    }
    overviews.into_iter().map(Option::unwrap).collect()
}

/// Subcommand path of an invocation that opens a menu; without a terminal its
/// help is printed instead.
/// The view the picker opens on for `command`: bare `shoal` lets it choose,
/// and a listing names its own unless it prints. `None` runs the command.
fn picker_start(ctx: &Context, command: Option<&Command>) -> Option<Option<menu::View>> {
    match command {
        None => Some(None),
        // Workspace processes, who cannot act on it, get the inbox printed.
        Some(Command::Workspace(WorkspaceCommand::Inbox {
            repository: None,
            print: false,
        })) if lists_interactively(ctx) && !env::inherits_scope(&ctx.paths.state) => {
            Some(Some(menu::View::Inbox))
        }
        Some(Command::Workspace(WorkspaceCommand::Work {
            command: None,
            chats: false,
            repository: None,
            status,
            print: false,
        })) if status.is_empty() && lists_interactively(ctx) => Some(Some(menu::View::Work)),
        _ => None,
    }
}

/// Whether a listing opens the picker: on a terminal, unless its output is
/// redirected to keep the list.
fn lists_interactively(ctx: &Context) -> bool {
    ctx.interactive() && std::io::IsTerminal::is_terminal(&std::io::stdout())
}

fn menu_path(command: Option<&Command>) -> Option<&'static [&'static str]> {
    match command {
        None => Some(&[]),
        Some(Command::Repository(RepositoryCommand::Repo { command: None })) => Some(&["repo"]),
        Some(_) => None,
    }
}

fn print_help(path: &[&str]) -> Result<()> {
    let mut root = Cli::command();
    root.build();
    let mut command = &mut root;
    for name in path {
        command = command
            .find_subcommand_mut(name)
            .expect("menu subcommands are defined");
    }
    out!("{}", Palette::stdout(false).styled(&command.render_help()));
    Ok(())
}

pub(crate) async fn run(cli: Cli) -> Result<i32> {
    // The service runs without the user's tool configuration environment, and
    // skill installation does this work itself.
    if !matches!(
        cli.command,
        Some(
            Command::Service(ServiceCommand::Daemon {
                command: DaemonCommand::Run { .. }
            }) | Command::Integration(IntegrationCommand::Skill {
                command: Some(_),
                ..
            })
        )
    ) && let Ok(state) = Paths::state_dir(cli.state_dir.clone())
    {
        skill::refresh_installed(&state, cli.json);
    }
    // Skill delivery is independent of daemon state and socket-path limits.
    if let Some(Command::Integration(IntegrationCommand::Skill { name, command })) = &cli.command {
        return skill::run(*name, command.as_ref(), cli.json, cli.state_dir.clone());
    }
    // Shell recovery must also work after cleanup, without daemon configuration.
    if let Some(Command::Integration(IntegrationCommand::Shell {
        command: ShellCommand::Recover { path },
    })) = &cli.command
    {
        let state = crate::paths::Paths::state_dir(cli.state_dir.clone())?;
        let destination = shell::recovery_directory(path, &state)?;
        if cli.json {
            outln!("{}", json!({"path": destination}));
        } else {
            outln!("{}", destination.display());
        }
        return Ok(0);
    }
    if let Some(path) = menu_path(cli.command.as_ref())
        && !Context::is_interactive(cli.json)
    {
        print_help(path)?;
        return Ok(0);
    }
    let mut ctx = Context::new(Paths::new(cli.state_dir)?, cli.json);
    ctx.need = cli.need;
    ctx.command = cli.path;
    // The daemon pins its own config's forges and keeps them across a
    // reload that fails, which a loader reading the file later would not.
    if !matches!(
        cli.command,
        Some(Command::Service(ServiceCommand::Daemon {
            command: DaemonCommand::Run { .. }
        }))
    ) {
        ctx.learn_pinned_forges();
    }
    let command = match picker_start(&ctx, cli.command.as_ref()) {
        Some(start) => match menu::choose(&ctx, start).await? {
            menu::Choice::Run(command) => *command,
            menu::Choice::Done(code) => return Ok(code),
        },
        None => match cli.command {
            Some(Command::Repository(RepositoryCommand::Repo { command: None })) => {
                repo_menu::choose(&ctx).await?
            }
            Some(command) => command,
            None => unreachable!("bare shoal opens the picker"),
        },
    };
    ctx.recent = command.recent_name();
    ensure!(
        !(command.is_administrative() && env::inherits_scope(&ctx.paths.state)),
        "workspace processes cannot administer Shoal; ask the user to run this command"
    );
    match command {
        Command::Run {
            name,
            workspace,
            args,
        } => dispatch::run::run(ctx, name, workspace, args).await,
        Command::Custom(words) => dispatch::run::custom(ctx, words).await,
        Command::Internal { command } => dispatch::internal::run(ctx, command).await,
        Command::Approvals { command, args } => dispatch::approvals::run(ctx, command, args).await,
        Command::Integration(command) => dispatch::integration::run(ctx, command).await,
        Command::Repository(command) => dispatch::repository::run(ctx, command).await,
        Command::Create(command) => dispatch::create::run(ctx, command).await,
        Command::Items(command) => dispatch::items::run(ctx, command).await,
        Command::Sessions(command) => dispatch::sessions::run(ctx, command).await,
        Command::Coordination(command) => dispatch::coordination::run(ctx, command).await,
        Command::Workspace(command) => dispatch::workspace::run(ctx, command).await,
        Command::Changes(command) => dispatch::changes::run(ctx, command).await,
        Command::Resources(command) => dispatch::resources::run(ctx, command).await,
        Command::Diagnostics(command) => dispatch::diagnostics::run(ctx, command).await,
        Command::Lifecycle(command) => dispatch::lifecycle::run(ctx, command).await,
        Command::Agents(command) => dispatch::agents::run(ctx, command).await,
        Command::Service(command) => dispatch::service::run(ctx, command, cli.daemon_handoff).await,
    }
}

/// Exit status for a request the daemon declined because capacity is busy.
pub(crate) const EXIT_BUSY: i32 = 2;
