mod cli;
mod client;
mod daemon;
mod paths;
mod protocol;
mod service;

use anyhow::{Result, ensure};
use clap::Parser;
use serde_json::json;

use cli::{Cli, Command, DaemonCommand};
use paths::Paths;

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
    let paths = Paths::new(cli.state_dir)?;
    match cli.command {
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
                json!({"running": true, "service_file": service::file(&paths, service::Platform::current()?)}),
            );
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
