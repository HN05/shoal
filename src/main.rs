mod cleanup;
mod cli;
mod client;
mod config;
mod daemon;
mod execution;
mod model;
mod paths;
mod processes;
mod protocol;
mod removal;
mod repository;
mod service;
mod shell;
mod store;
mod ui;
mod workspace;
mod worktrunk;

use anyhow::{Result, ensure};
use clap::{CommandFactory, Parser};
use serde_json::json;

use cli::{Cli, Command, DaemonCommand, RepoCommand, ShellCommand};
use paths::Paths;
use protocol::{Body, Method};

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    let json_output = cli.json;
    match run(cli).await {
        Ok(code) => std::process::exit(code),
        Err(error) => {
            if json_output {
                eprintln!(
                    "{}",
                    json!({"error": {"code": "command_failed", "message": format!("{error:#}")}})
                );
            } else {
                eprintln!("error: {error:#}");
            }
            std::process::exit(1);
        }
    }
}

async fn run(cli: Cli) -> Result<i32> {
    if cli.command.is_none() && !ui::is_interactive(cli.json) {
        Cli::command().print_help()?;
        return Ok(0);
    }
    let paths = Paths::new(cli.state_dir)?;
    let command = match cli.command {
        Some(command) => command,
        None => ui::workspace_menu(&paths).await?,
    };
    match command {
        Command::Cd { workspace } => {
            let workspace = ui::workspace(&paths, workspace, true, cli.json).await?;
            let inspection = match client::call(&paths, Method::Inspect { workspace }).await? {
                Body::Inspection(inspection) => inspection,
                _ => anyhow::bail!("unexpected inspection response"),
            };
            ensure!(
                inspection.workspace.path.is_dir(),
                "workspace directory is missing"
            );
            output(
                cli.json,
                &inspection.workspace.path.display().to_string(),
                json!({"path": inspection.workspace.path}),
            );
            shell::navigate(&inspection.workspace.path, cli.json)?;
        }
        Command::Shell {
            command: ShellCommand::Init,
        } => {
            if cli.json {
                println!("{}", json!({"script": shell::INIT}));
            } else {
                print!("{}", shell::INIT);
            }
        }
        Command::Repo {
            command: RepoCommand::Add { source },
        } => {
            let source = ui::repository_selector(source)?;
            match client::call(&paths, Method::Register { source }).await? {
                Body::Repository(repo) => output(
                    cli.json,
                    &format!("Registered {}", ui::repository_label(&repo)),
                    serde_json::to_value(&repo)?,
                ),
                _ => anyhow::bail!("unexpected registration response"),
            }
        }
        Command::Repo {
            command: RepoCommand::List,
        } => {
            let repos = ui::repositories(&paths).await?;
            if cli.json {
                println!("{}", serde_json::to_string(&repos)?);
            } else {
                for repo in repos {
                    println!("{}", ui::repository_label(&repo));
                }
            }
        }
        Command::Add {
            repository,
            name,
            base,
        } => {
            let repository = match repository {
                Some(repo) => ui::repository_selector(repo)?,
                None => ui::pick(
                    "Repository> ",
                    ui::repository_choices(ui::repositories(&paths).await?).await?,
                    cli.json,
                )?,
            };
            let name = match name {
                Some(name) => name,
                None => ui::input("Workspace name", cli.json)?,
            };
            match client::call(
                &paths,
                Method::Add {
                    repository,
                    name,
                    base,
                },
            )
            .await?
            {
                Body::Workspace(workspace) => {
                    output(
                        cli.json,
                        &format!("Created {} at {}", workspace.name, workspace.path.display()),
                        serde_json::to_value(&workspace)?,
                    );
                    shell::navigate(&workspace.path, cli.json)?;
                }
                _ => anyhow::bail!("unexpected workspace response"),
            }
        }
        Command::List => {
            let workspaces = ui::workspaces(&paths).await?;
            if cli.json {
                println!("{}", serde_json::to_string(&workspaces)?);
            } else {
                for w in workspaces {
                    println!(
                        "{}  {}  {}  {}",
                        w.name,
                        w.state,
                        w.branch,
                        w.path.display()
                    );
                }
            }
        }
        Command::Inspect { workspace } => {
            let workspace = ui::workspace(&paths, workspace, false, cli.json).await?;
            match client::call(&paths, Method::Inspect { workspace }).await? {
                Body::Inspection(inspection) => {
                    if cli.json {
                        println!("{}", serde_json::to_string(&inspection)?);
                    } else {
                        println!("{}", serde_json::to_string_pretty(&inspection)?);
                    }
                }
                _ => anyhow::bail!("unexpected inspection response"),
            }
        }
        Command::Stop { workspace } => {
            let workspace = ui::workspace(&paths, workspace, false, cli.json).await?;
            client::call(&paths, Method::Stop { workspace }).await?;
            output(
                cli.json,
                "Workspace processes stopped",
                json!({"stopped": true}),
            );
        }
        Command::Rm {
            workspace,
            yes,
            keep_branch,
            delete_branch,
        } => {
            let workspace = ui::workspace(&paths, workspace, true, cli.json).await?;
            let caller_pid = std::process::id();
            let check = match client::call(
                &paths,
                Method::CheckRemoval {
                    workspace: workspace.clone(),
                    caller_pid,
                },
            )
            .await?
            {
                Body::RemovalCheck(check) => check,
                _ => anyhow::bail!("unexpected removal check response"),
            };
            let choice = if keep_branch {
                removal::Choice::KeepBranch
            } else if delete_branch {
                removal::Choice::DeleteBranch
            } else if !check.needs_choice() {
                removal::Choice::Auto
            } else {
                ensure!(
                    !yes,
                    "choose --keep-branch or --delete-branch with --yes for a dirty or differing workspace"
                );
                ui::choose_removal(&check, cli.json)?
            };
            let inspection = match client::call(
                &paths,
                Method::Inspect {
                    workspace: workspace.clone(),
                },
            )
            .await?
            {
                Body::Inspection(inspection) => inspection,
                _ => anyhow::bail!("unexpected inspection response"),
            };
            let cwd = std::env::current_dir()?;
            let inside = std::fs::canonicalize(&inspection.workspace.path)
                .is_ok_and(|root| cwd.starts_with(root));
            let destination = if inside {
                ui::repositories(&paths)
                    .await?
                    .into_iter()
                    .find(|r| r.id == inspection.workspace.repository_id)
                    .map(|r| r.path)
            } else {
                None
            };
            let result = client::call(
                &paths,
                Method::Remove {
                    workspace,
                    choice,
                    caller_pid,
                },
            )
            .await;
            if inside && (result.is_ok() || !cwd.exists()) {
                let destination = destination
                    .filter(|p| p.is_dir())
                    .unwrap_or_else(|| paths.home.clone());
                shell::navigate(&destination, cli.json)?;
            }
            let result = match result? {
                Body::RemovalResult(result) => result,
                _ => anyhow::bail!("unexpected removal response"),
            };
            let message = match (&result.branch, result.branch_deleted) {
                (Some(branch), true) => format!("Workspace and Git branch {branch} removed"),
                (Some(branch), false) => format!(
                    "Workspace removed; Git branch {branch} retained ({})",
                    result.branch_outcome
                ),
                (None, _) => "Workspace removed".into(),
            };
            output(cli.json, &message, serde_json::to_value(&result)?);
        }
        Command::Exec { workspace, command } => {
            let workspace = ui::workspace(&paths, workspace, true, cli.json).await?;
            return execution::run(&paths, workspace, command).await;
        }
        Command::Claude { workspace, args } => {
            let workspace = ui::workspace(&paths, workspace, true, cli.json).await?;
            return execution::run(
                &paths,
                workspace,
                std::iter::once("claude".into()).chain(args).collect(),
            )
            .await;
        }
        Command::Codex { workspace, args } => {
            let workspace = ui::workspace(&paths, workspace, true, cli.json).await?;
            return execution::run(
                &paths,
                workspace,
                std::iter::once("codex".into()).chain(args).collect(),
            )
            .await;
        }
        Command::Setup {
            dry_run,
            executable,
        } => {
            let executable = service::executable(executable)?;
            if dry_run {
                let definition =
                    service::definition(&paths, &executable, service::Platform::current()?)?;
                if cli.json {
                    println!("{}", serde_json::to_string(&definition)?);
                } else {
                    println!("# {}\n{}", definition.path.display(), definition.content);
                }
                return Ok(0);
            }
            if let Some(status) = client::status(&paths).await? {
                ensure!(
                    status.managed,
                    "a foreground daemon is running; stop it before setting up the service"
                );
            }
            service::setup(&paths, &executable).await?;
            client::wait(&paths, true).await?;
            output(
                cli.json,
                "Daemon service installed and running",
                json!({"running": true, "service_file": service::file(&paths, service::Platform::current()?), "shell_init": shell::INIT_COMMAND}),
            );
            if !cli.json {
                println!(
                    "\nAdd this line to ~/.zshrc or ~/.bashrc to navigate after add/rm:\n\n{}",
                    shell::INIT_COMMAND
                );
            }
        }
        Command::Daemon {
            command: DaemonCommand::Run { managed },
        } => daemon::run(paths, managed).await?,
        Command::Daemon {
            command: DaemonCommand::Status,
        } => {
            let status = client::status(&paths).await?;
            let running = status.is_some();
            let message = status
                .as_ref()
                .map(|s| format!("Daemon running (PID {}, version {})", s.pid, s.version))
                .unwrap_or_else(|| "Daemon is not running".into());
            output(
                cli.json,
                &message,
                json!({"running": running, "daemon": status, "socket": paths.socket}),
            );
            return Ok(if running { 0 } else { 1 });
        }
        Command::Daemon {
            command: DaemonCommand::Start,
        } => {
            service::start(&paths).await?;
            client::wait(&paths, true).await?;
            output(cli.json, "Daemon started", json!({"running": true}));
        }
        Command::Daemon {
            command: DaemonCommand::Stop,
        } => {
            stop(&paths).await?;
            output(cli.json, "Daemon stopped", json!({"running": false}));
        }
        Command::Daemon {
            command: DaemonCommand::Restart,
        } => {
            // Service administration must still work after a protocol upgrade.
            if let Ok(Some(status)) = client::status(&paths).await {
                ensure!(
                    status.managed,
                    "foreground daemon: stop it and run `shoal daemon run` again"
                );
            }
            stop(&paths).await?;
            service::start(&paths).await?;
            client::wait(&paths, true).await?;
            output(cli.json, "Daemon restarted", json!({"running": true}));
        }
    }
    Ok(0)
}

async fn stop(paths: &Paths) -> Result<()> {
    match client::status(paths).await {
        Ok(Some(status)) if !status.managed => {
            client::call(paths, protocol::Method::Shutdown).await?;
        }
        Err(error) if !service::file(paths, service::Platform::current()?).exists() => {
            return Err(error);
        }
        _ => service::stop(paths).await?,
    }
    client::wait(paths, false).await
}

fn output(json_output: bool, message: &str, value: serde_json::Value) {
    if json_output {
        println!("{value}");
    } else {
        println!("{message}");
    }
}
