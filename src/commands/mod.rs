//! CLI dispatch. Domain handlers own requests, prompts, and rendering;
//! daemon modules own lifecycle and allocation policy.
mod issues;
mod menu;
mod ports;
mod recovery;
mod repositories;
mod resources;
mod service;
mod simulators;
mod skill;
mod workspaces;

use std::time::Duration;

use anyhow::{Result, ensure};
use clap::CommandFactory;
use serde_json::json;
use tokio::time::{Instant, sleep};

use crate::{
    cli::{Cli, Command, ShellCommand},
    context::Context,
    env,
    paths::Paths,
    shell,
};

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
            repository,
            name,
            branch,
            issue,
            base,
            agent,
            args,
        } => workspaces::add(&ctx, repository, (name, branch), base, issue, agent, args).await,
        Command::Issue {
            url,
            agent,
            base,
            args,
        } => issues::run(&ctx, url, agent, base, args).await,
        Command::Prepare { workspace } => workspaces::prepare(&ctx, workspace).await,
        Command::List => workspaces::list(&ctx).await,
        Command::Cd { workspace } => workspaces::cd(&ctx, workspace).await,
        Command::Diff { workspace } => workspaces::diff(&ctx, workspace).await,
        Command::Pull { workspace } => workspaces::pull(&ctx, workspace).await,
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
            clear,
        } => workspaces::pr(&ctx, workspace, url, clear).await,
        Command::Merged { workspace } => workspaces::pr(&ctx, workspace, None, false).await,
        Command::Inspect { workspace } => workspaces::inspect(&ctx, workspace).await,
        Command::Stop { workspace } => workspaces::stop(&ctx, workspace).await,
        Command::Rm {
            workspace,
            yes,
            keep_branch,
            delete_branch,
        } => workspaces::remove(&ctx, workspace, yes, keep_branch, delete_branch).await,
        Command::Exec { workspace, command } => workspaces::exec(&ctx, workspace, command).await,
        Command::Claude { workspace, args } => workspaces::claude(&ctx, workspace, args).await,
        Command::Codex {
            mode,
            workspace,
            args,
        } => workspaces::codex(&ctx, mode, workspace, args).await,
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
            command,
        } => crate::execution::run_detached_wrapper(&ctx.paths, workspace, log, command).await,
        Command::Port { command } => ports::run(&ctx, command).await,
        Command::Ports { workspace } => ports::overview(&ctx, workspace).await,
        Command::Resource { command } => resources::run(&ctx, command).await,
        Command::Resources { workspace } => resources::overview(&ctx, workspace).await,
        Command::Sim { command } => simulators::run(&ctx, command).await,
        Command::Reconcile {
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
        Command::Setup {
            dry_run,
            executable,
        } => service::setup(&ctx, dry_run, executable).await,
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
