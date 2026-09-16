//! CLI OS-service administration, including offline/protocol-upgrade handling.
use super::output;
use crate::{cli::DaemonCommand, daemon, service, shell};
use crate::{client, paths::Paths, protocol::Method};
use anyhow::Result;
use anyhow::ensure;
use serde_json::json;
use std::path::PathBuf;

pub(super) async fn setup(
    paths: &Paths,
    dry_run: bool,
    executable: Option<PathBuf>,
    json_output: bool,
) -> Result<i32> {
    let executable = service::executable(executable)?;
    if dry_run {
        let definition = service::definition(paths, &executable, service::Platform::current()?)?;
        if json_output {
            println!("{}", serde_json::to_string(&definition)?);
        } else {
            println!("# {}\n{}", definition.path.display(), definition.content);
        }
        return Ok(0);
    }
    if let Some(status) = client::status(paths).await? {
        ensure!(
            status.managed,
            "a foreground daemon is running; stop it before setting up the service"
        );
    }
    service::setup(paths, &executable).await?;
    client::wait(paths, true).await?;
    output(
        json_output,
        "Daemon service installed and running",
        json!({"running": true, "service_file": service::file(paths, service::Platform::current()?), "shell_init": shell::INIT_COMMAND}),
    );
    if !json_output {
        println!(
            "\nAdd this line to ~/.zshrc or ~/.bashrc to navigate after add/rm:\n\n{}",
            shell::INIT_COMMAND
        );
    }
    Ok(0)
}

pub(super) async fn run(paths: Paths, command: DaemonCommand, json_output: bool) -> Result<i32> {
    match command {
        DaemonCommand::Run { managed } => daemon::run(paths, managed).await?,
        DaemonCommand::Status => {
            let status = client::status(&paths).await?;
            let running = status.is_some();
            let message = status
                .as_ref()
                .map(|s| format!("Daemon running (PID {}, version {})", s.pid, s.version))
                .unwrap_or_else(|| "Daemon is not running".into());
            output(
                json_output,
                &message,
                json!({"running": running, "daemon": status, "socket": paths.socket}),
            );
            return Ok(if running { 0 } else { 1 });
        }
        DaemonCommand::Start => {
            service::start(&paths).await?;
            client::wait(&paths, true).await?;
            output(json_output, "Daemon started", json!({"running": true}));
        }
        DaemonCommand::Stop => {
            stop(&paths).await?;
            output(json_output, "Daemon stopped", json!({"running": false}));
        }
        DaemonCommand::Restart => {
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
            output(json_output, "Daemon restarted", json!({"running": true}));
        }
    }
    Ok(0)
}
async fn stop(paths: &Paths) -> Result<()> {
    match client::status(paths).await {
        Ok(Some(status)) if !status.managed => {
            client::call(paths, Method::Shutdown).await?;
        }
        Err(error) if !service::file(paths, service::Platform::current()?).exists() => {
            return Err(error);
        }
        _ => service::stop(paths).await?,
    }
    client::wait(paths, false).await
}
