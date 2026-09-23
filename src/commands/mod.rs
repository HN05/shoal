//! CLI dispatch. Domain handlers own requests, prompts, and rendering;
//! daemon modules own lifecycle and allocation policy.
mod access;
mod configuration;
mod issues;
mod menu;
mod notifications;
mod ports;
mod recovery;
mod repositories;
mod resources;
mod review;
mod service;
mod simulators;
mod skill;
mod workspaces;

use std::time::Duration;

use anyhow::{Result, ensure};
use clap::CommandFactory;
use serde::Serialize;
use serde_json::json;
use tokio::time::{Instant, sleep};

use crate::{
    cli::{Cli, CodexMode, Command, ConfigCommand, PrCommand, ShellCommand},
    context::Context,
    env,
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

pub(crate) async fn run(cli: Cli) -> Result<i32> {
    // Skill delivery is independent of daemon state and socket-path limits.
    if let Some(Command::Skill { command }) = &cli.command {
        return skill::run(command.as_ref(), cli.json);
    }
    if cli.command.is_none() && !Context::is_interactive(cli.json) {
        Cli::command().print_help()?;
        return Ok(0);
    }
    let ctx = Context::new(Paths::new(cli.state_dir)?, cli.json);
    let command = match cli.command {
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
            Some(name) if name == "claude" => workspaces::claude(&ctx, workspace, args).await,
            Some(name) if name == "codex" => workspaces::codex(&ctx, None, workspace, args).await,
            Some(name) => crate::named_commands::run(&ctx, &name, workspace, args).await,
            None => crate::named_commands::list(&ctx).await,
        },
        Command::Custom(args) => crate::named_commands::invoke(&ctx, args).await,
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
        Command::Repo { command } => repositories::run(&ctx, command).await,
        Command::Add {
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
            )
            .await
        }
        Command::Adopt { repository, path } => workspaces::adopt(&ctx, repository, path).await,
        Command::Issue {
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
            )
            .await
        }
        Command::Setup { workspace } => workspaces::setup(&ctx, workspace).await,
        Command::List => workspaces::list(&ctx).await,
        Command::Status { workspace } => workspaces::status(&ctx, workspace).await,
        Command::Cd { workspace } => workspaces::cd(&ctx, workspace).await,
        Command::Diff { workspace } => workspaces::diff(&ctx, workspace).await,
        Command::Review {
            workspace,
            manual,
            agent,
            args,
        } => {
            let reviewer = match (manual, agent) {
                (true, _) => review::Reviewer::Manual,
                (false, Some(agent)) => review::Reviewer::Agent(Some(agent)),
                (false, None) => review::Reviewer::Ask,
            };
            review::run(&ctx, workspace, reviewer, None, args).await
        }
        Command::Merge {
            branch,
            workspace,
            remote,
            local,
        } => crate::merge::run(&ctx, workspace, branch, remote, local).await,
        Command::Land { workspace } => workspaces::land(&ctx, workspace).await,
        Command::LandInternal { plan } => workspaces::land_worker(&ctx, plan).await,
        Command::MergeInternal {
            branch,
            remote,
            local,
        } => crate::merge::worker(&ctx, branch, remote, local).await,
        Command::Pr {
            workspace,
            url,
            command,
        } => match command {
            None => workspaces::pr(&ctx, workspace, url, false).await,
            Some(PrCommand::Merged { workspace }) => {
                workspaces::pr(&ctx, workspace, None, false).await
            }
            Some(PrCommand::Clear { workspace }) => {
                workspaces::pr(&ctx, workspace, None, true).await
            }
        },
        Command::Inspect { workspace } => workspaces::inspect(&ctx, workspace).await,
        Command::Notifications { all, follow, limit } => {
            notifications::run(&ctx, all, follow, limit).await
        }
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
        Command::Claude { workspace, args } => workspaces::claude(&ctx, workspace, args).await,
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
            workspaces::codex(&ctx, mode, workspace, args).await
        }
        Command::T3 { workspace, args } => workspaces::open_app(&ctx, workspace, "t3", args).await,
        Command::Happy {
            agent,
            workspace,
            prompt,
            args,
        } => workspaces::happy(&ctx, agent, workspace, prompt, args).await,
        Command::DetachedInternal {
            workspace,
            log,
            agent,
            command,
        } => {
            crate::execution::run_detached_wrapper(&ctx.paths, workspace, log, command, agent).await
        }
        Command::Port {
            command,
            workspace,
            all,
        } => ports::run(&ctx, command, workspace, all).await,
        Command::Access { command } => access::run(&ctx, command).await,
        Command::Resource {
            command,
            workspace,
            all,
        } => resources::run(&ctx, command, workspace, all).await,
        Command::Sim {
            command,
            workspace,
            all,
        } => simulators::run(&ctx, command, workspace, all).await,
        Command::Doctor {
            workspace,
            all,
            repair,
            stop,
            acknowledge_stopped,
        } => {
            let options = crate::recovery::ReconcileOptions {
                repair,
                stop,
                acknowledge_stopped,
            };
            recovery::run(&ctx, workspace, all, options).await
        }
        Command::Config { command } => match command {
            ConfigCommand::Set {
                key,
                value,
                repository,
            } => configuration::edit(&ctx, key, Some(value), repository).await,
            ConfigCommand::Unset { key, repository } => {
                configuration::edit(&ctx, key, None, repository).await
            }
            ConfigCommand::Show { workspace } => configuration::show(&ctx, workspace).await,
            ConfigCommand::Reset => service::install_config(&ctx, None),
            ConfigCommand::Install { name } => service::install_config(&ctx, Some(name)),
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

/// One attempt at an operation the daemon may report as temporarily busy.
pub(crate) enum Attempt<T> {
    Ready(T),
    Busy(String),
}

/// Retry `attempt` about once a second until it succeeds or `wait_seconds`
/// pass. Returns the last busy message on timeout.
pub(crate) async fn retry_while_busy<T>(
    wait_seconds: u64,
    mut attempt: impl AsyncFnMut() -> Result<Attempt<T>>,
) -> Result<Result<T, String>> {
    let deadline = Instant::now() + Duration::from_secs(wait_seconds);
    loop {
        match attempt().await? {
            Attempt::Ready(value) => return Ok(Ok(value)),
            Attempt::Busy(message) if Instant::now() >= deadline => return Ok(Err(message)),
            Attempt::Busy(_) => {
                let remaining = deadline.saturating_duration_since(Instant::now());
                sleep(Duration::from_secs(1).min(remaining)).await;
            }
        }
    }
}
