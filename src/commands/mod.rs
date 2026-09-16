//! CLI dispatch. Domain handlers own requests, prompts, and rendering;
//! daemon modules own lifecycle and allocation policy.
mod ports;
mod recovery;
mod repositories;
mod resources;
mod service;
mod simulators;
mod skill;
mod workspaces;

use crate::{
    cli::{Cli, Command, DaemonCommand, ShellCommand},
    paths::Paths,
    shell, ui,
};
use anyhow::{Result, ensure};
use clap::CommandFactory;
use serde_json::json;

pub(crate) async fn run(cli: Cli) -> Result<i32> {
    // Skill delivery is independent of daemon state and socket-path limits.
    if let Some(Command::Skill { command }) = &cli.command {
        return skill::run(command.as_ref(), cli.json);
    }
    if cli.command.is_none() && !ui::is_interactive(cli.json) {
        Cli::command().print_help()?;
        return Ok(0);
    }
    let paths = Paths::new(cli.state_dir)?;
    let command = match cli.command {
        Some(command) => command,
        None => ui::workspace_menu(&paths).await?,
    };
    if std::env::var_os("SHOAL_SCOPE_TOKEN").is_some() {
        ensure!(
            !matches!(
                &command,
                Command::Setup { .. }
                    | Command::Daemon {
                        command: DaemonCommand::Run { .. }
                            | DaemonCommand::Start
                            | DaemonCommand::Stop
                            | DaemonCommand::Restart
                    }
            ),
            "workspace processes cannot administer Shoal"
        );
    }
    match command {
        Command::Skill { command } => skill::run(command.as_ref(), cli.json),
        Command::Resource { command } => resources::run(&paths, command, cli.json).await,
        Command::Resources { workspace } => resources::overview(&paths, workspace, cli.json).await,
        Command::Sim { command } => simulators::run(&paths, command, cli.json).await,
        Command::Port { command } => ports::run(&paths, command, cli.json).await,
        Command::Ports { workspace } => ports::overview(&paths, workspace, cli.json).await,
        Command::Pull { workspace } => workspaces::pull(&paths, workspace, cli.json).await,
        Command::Merge {
            branch,
            workspace,
            remote,
        } => crate::merge::run(&paths, workspace, branch, remote, cli.json).await,
        Command::MergeInternal { branch, remote } => {
            crate::merge::worker(&paths, branch, remote, cli.json).await
        }
        Command::Diff { workspace } => workspaces::diff(&paths, workspace, cli.json).await,
        Command::Cd { workspace } => workspaces::cd(&paths, workspace, cli.json).await,
        Command::Repo { command } => repositories::run(&paths, command, cli.json).await,
        Command::Add {
            repository,
            name,
            base,
            agent,
            args,
        } => workspaces::add(&paths, repository, name, base, agent, args, cli.json).await,
        Command::Prepare { workspace } => workspaces::prepare(&paths, workspace, cli.json).await,
        Command::List => workspaces::list(&paths, cli.json).await,
        Command::Inspect { workspace } => workspaces::inspect(&paths, workspace, cli.json).await,
        Command::Stop { workspace } => workspaces::stop(&paths, workspace, cli.json).await,
        Command::Rm {
            workspace,
            yes,
            keep_branch,
            delete_branch,
        } => workspaces::remove(&paths, workspace, yes, keep_branch, delete_branch, cli.json).await,
        Command::Exec { workspace, command } => {
            workspaces::exec(&paths, workspace, command, cli.json).await
        }
        Command::Claude { workspace, args } => {
            workspaces::claude(&paths, workspace, args, cli.json).await
        }
        Command::Codex {
            mode,
            workspace,
            args,
        } => workspaces::codex(&paths, mode, workspace, args, cli.json).await,
        Command::T3 { workspace, args } => {
            workspaces::open_app(&paths, workspace, "t3", args, cli.json).await
        }
        Command::Completions { shell } => {
            let script = crate::shell::completions(shell)?;
            if cli.json {
                println!("{}", json!({"script": script}));
            } else {
                print!("{script}");
            }
            Ok(0)
        }
        Command::Setup {
            dry_run,
            executable,
        } => service::setup(&paths, dry_run, executable, cli.json).await,
        Command::Daemon { command } => service::run(paths, command, cli.json).await,
        Command::Reconcile {
            workspace,
            all,
            repair,
            stop,
            acknowledge_stopped,
        } => {
            recovery::run(
                &paths,
                workspace,
                all,
                crate::recovery::Options {
                    repair,
                    stop,
                    acknowledge_stopped,
                },
                cli.json,
            )
            .await
        }
        Command::Shell {
            command: ShellCommand::Init,
        } => {
            if cli.json {
                println!("{}", json!({"script": shell::INIT}));
            } else {
                print!("{}", shell::INIT);
            }
            Ok(0)
        }
    }
}

fn output(json_output: bool, message: &str, value: serde_json::Value) {
    if json_output {
        println!("{value}");
    } else {
        println!("{message}");
    }
}
