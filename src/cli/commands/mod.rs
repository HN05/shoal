//! CLI dispatch. Domain handlers own requests, prompts, and rendering;
//! daemon modules own lifecycle and allocation policy.
mod access;
mod acquisition;
mod configuration;
pub(super) mod issues;
mod menu;
mod notifications;
mod ports;
mod recovery;
mod repo_menu;
mod repositories;
mod resources;
pub(crate) mod resume;
mod review;
mod service;
mod simulators;
mod skill;
pub(in crate::cli) mod workspaces;

#[cfg(test)]
mod tests;

use anyhow::{Result, ensure};
use clap::CommandFactory;
use futures_util::{StreamExt, stream};
use serde::Serialize;
use serde_json::json;

use crate::{
    agent::CodexMode,
    cli::{
        Cli, Command, ConfigCommand, PrCommand, ShellCommand, agents, client,
        context::Context,
        output::{Palette, Style},
    },
    env,
    forge::pr::Action,
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
                    println!("{heading}");
                    render(overview, palette);
                }
                WorkspaceOverviewResult::Failed { error, .. } => {
                    println!("{heading}: {}", palette.paint(Style::Warning, error));
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
fn menu_path(command: Option<&Command>) -> Option<&'static [&'static str]> {
    match command {
        None => Some(&[]),
        Some(Command::Repo { command: None }) => Some(&["repo"]),
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
    command.print_help()?;
    Ok(())
}

pub(crate) async fn run(cli: Cli) -> Result<i32> {
    // Shell recovery must also work after cleanup, without daemon configuration.
    if let Some(Command::Shell {
        command: ShellCommand::Recover { path },
    }) = &cli.command
    {
        let destination = shell::recovery_directory(path)?;
        if cli.json {
            println!("{}", json!({"path": destination}));
        } else {
            println!("{}", destination.display());
        }
        return Ok(0);
    }
    // Skill delivery is independent of daemon state and socket-path limits.
    if let Some(Command::Skill { command }) = &cli.command {
        return skill::run(command.as_ref(), cli.json);
    }
    if let Some(path) = menu_path(cli.command.as_ref())
        && !Context::is_interactive(cli.json)
    {
        print_help(path)?;
        return Ok(0);
    }
    let ctx = Context::new(Paths::new(cli.state_dir)?, cli.json);
    let command = match cli.command {
        Some(Command::Repo { command: None }) => repo_menu::choose(&ctx).await?,
        Some(command) => command,
        None => menu::choose(&ctx).await?,
    };
    ensure!(
        !(env::is_scoped() && command.is_administrative()),
        "workspace processes cannot administer Shoal"
    );
    match command {
        Command::Run {
            name,
            workspace,
            args,
        } => match name {
            Some(name) if name == "claude" => agents::claude(&ctx, workspace, args).await,
            Some(name) if name == "codex" => agents::codex(&ctx, None, workspace, args).await,
            Some(name) => crate::config::named_commands::run(&ctx, &name, workspace, args).await,
            None => crate::config::named_commands::list(&ctx).await,
        },
        Command::Custom(args) => crate::config::named_commands::invoke(&ctx, args).await,
        Command::Skill { command } => skill::run(command.as_ref(), ctx.json),
        Command::Completions { shell } => {
            let script = shell::completions(shell)?;
            ctx.emit(&script, json!({"script": script}))?;
            Ok(0)
        }
        Command::Shell {
            command: ShellCommand::Init,
        } => {
            ctx.emit(shell::INIT, json!({"script": shell::INIT}))?;
            Ok(0)
        }
        Command::Shell {
            command: ShellCommand::Recover { .. },
        } => unreachable!("shell recovery is handled before loading paths"),
        Command::Repo {
            command: Some(command),
        } => repositories::run(&ctx, command).await,
        Command::Repo { command: None } => unreachable!("the repository menu returns a command"),
        Command::Add {
            here,
            path,
            repository,
            branch,
            existing,
            issue,
            base,
            git_profile,
            agent,
            args,
        } => {
            workspaces::add(
                &ctx,
                repository,
                workspaces::Creation {
                    path,
                    branch,
                    existing,
                    base,
                    git_profile,
                },
                issue,
                workspaces::AgentLaunch::Explicit(agent),
                args,
                here,
            )
            .await
        }
        Command::Adopt { repository, path } => workspaces::adopt(&ctx, repository, path).await,
        Command::Issue {
            here,
            issue,
            repository,
            agent,
            base,
            args,
        } => {
            workspaces::add(
                &ctx,
                repository,
                workspaces::Creation {
                    path: None,
                    branch: None,
                    existing: None,
                    base,
                    git_profile: None,
                },
                Some(issue),
                workspaces::AgentLaunch::IssueDefault(agent),
                args,
                here,
            )
            .await
        }
        Command::HerdrInternal {
            close_when_done,
            plan,
        } => super::herdr::worker(ctx, close_when_done, &plan).await,
        Command::Setup { workspace } => workspaces::setup(&ctx, workspace).await,
        Command::Ls => workspaces::list(&ctx).await,
        Command::Status { workspace } => workspaces::status(&ctx, workspace).await,
        Command::Cd { workspace } => workspaces::cd(&ctx, workspace).await,
        Command::Diff { workspace } => workspaces::diff(&ctx, workspace).await,
        Command::Review {
            workspace,
            manual,
            agent,
            args,
        } => {
            let reviewer = review::Reviewer::new(manual, agent);
            review::run(&ctx, workspace, reviewer, None, args).await
        }
        Command::Merge {
            branch,
            workspace,
            remote,
            local,
        } => crate::git::merge::run(&ctx, workspace, branch, remote, local).await,
        Command::Land { workspace } => workspaces::land(&ctx, workspace).await,
        Command::LandInternal { plan } => workspaces::land_worker(&ctx, plan).await,
        Command::MergeInternal {
            branch,
            remote,
            local,
        } => crate::git::merge::worker(&ctx, branch, remote, local).await,
        Command::Pr { command } => match command {
            PrCommand::Watch { workspace, url } => {
                workspaces::pr(&ctx, workspace, Action::Watch { url }).await
            }
            PrCommand::Unwatch { workspace, url } => {
                let action = url.map_or(Action::Clear, |url| Action::Unwatch { url });
                workspaces::pr(&ctx, workspace, action).await
            }
            PrCommand::Review {
                url,
                repository,
                manual,
                agent,
                args,
            } => {
                let reviewer = review::Reviewer::new(manual, agent);
                review::pull_request(&ctx, url, repository, reviewer, args).await
            }
        },
        Command::Continue { workspace } => workspaces::continue_work(&ctx, workspace).await,
        Command::Done {
            workspace,
            keep,
            cleanup,
        } => {
            let cleanup = if keep {
                Some(false)
            } else if cleanup {
                Some(true)
            } else {
                None
            };
            workspaces::done(&ctx, workspace, cleanup).await
        }
        Command::Inspect { workspace } => workspaces::inspect(&ctx, workspace).await,
        Command::Notifications { all, follow, limit } => {
            notifications::run(&ctx, all, follow, limit).await
        }
        Command::Resume {
            workspace,
            execution,
            discard,
        } => resume::run(&ctx, workspace, execution, discard).await,
        Command::Pause {
            workspace,
            execution,
        } => workspaces::pause(&ctx, workspace, execution).await,
        Command::Stop { workspace } => workspaces::stop(&ctx, workspace).await,
        Command::Rm {
            workspace,
            confirmation,
            keep_branch,
            delete_branch,
        } => {
            workspaces::remove(
                &ctx,
                workspace,
                confirmation.yes,
                keep_branch,
                delete_branch,
            )
            .await
        }
        Command::Exec { workspace, command } => workspaces::exec(&ctx, workspace, command).await,
        Command::Claude { workspace, args } => agents::claude(&ctx, workspace, args).await,
        Command::Codex {
            workspace,
            cli,
            app,
            args,
        } => {
            let mode = if cli {
                Some(CodexMode::Cli)
            } else if app {
                Some(CodexMode::App)
            } else {
                None
            };
            agents::codex(&ctx, mode, workspace, args).await
        }
        Command::T3 { workspace, args } => agents::open_app(&ctx, workspace, "t3", args).await,
        Command::Happy {
            agent,
            workspace,
            prompt,
            args,
        } => agents::happy(&ctx, agent, workspace, prompt, args).await,
        Command::DetachedInternal {
            workspace,
            log,
            agent,
            command,
        } => {
            crate::execution::run_detached_wrapper(&ctx.paths, workspace, log, command, agent).await
        }
        Command::Port { command, scope } => ports::run(&ctx, command, scope).await,
        Command::Access { command } => access::run(&ctx, command).await,
        Command::Resource { command, scope } => resources::run(&ctx, command, scope).await,
        Command::Sim { command, scope } => simulators::run(&ctx, command, scope).await,
        Command::Doctor {
            scope,
            repair,
            stop,
            acknowledge_stopped,
            reclaim,
        } => {
            let options = crate::daemon::recovery::ReconcileOptions {
                repair,
                stop,
                acknowledge_stopped,
                reclaim,
            };
            recovery::run(&ctx, scope, options).await
        }
        Command::Config { command } => match command {
            ConfigCommand::Set {
                assignments,
                repository,
            } => configuration::edit(&ctx, configuration::sets(assignments)?, repository).await,
            ConfigCommand::Unset { keys, repository } => {
                configuration::edit(&ctx, configuration::unsets(keys), repository).await
            }
            ConfigCommand::Show { workspace } => configuration::show(&ctx, workspace).await,
            ConfigCommand::Reset => service::install_config(&ctx, None).await,
            ConfigCommand::Install { name } => service::install_config(&ctx, Some(name)).await,
        },
        Command::Install {
            dry_run,
            executable,
        } => service::install(&ctx, dry_run, executable).await,
        Command::Daemon { command } => service::run(ctx, command).await,
    }
}

/// Exit status for a request the daemon declined because capacity is busy.
pub(crate) const EXIT_BUSY: i32 = 2;
