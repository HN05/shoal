//! CLI OS-service administration, including offline/protocol-upgrade handling.
use std::path::PathBuf;

use anyhow::{Context as _, Result, ensure};
use serde_json::json;

use crate::{
    cli::{ConfigCommand, DaemonCommand},
    client,
    context::Context,
    daemon,
    output::Style,
    paths::Paths,
    protocol::Method,
    service::{self, Platform},
    shell,
};

pub(super) async fn setup(
    ctx: &Context,
    dry_run: bool,
    executable: Option<PathBuf>,
) -> Result<i32> {
    let executable = service::executable(executable)?;
    let platform = Platform::current()?;
    if dry_run {
        let definition = service::definition(&ctx.paths, &executable, platform)?;
        ctx.show(&definition, |definition| {
            println!("# {}\n{}", definition.path.display(), definition.content);
        })?;
        return Ok(0);
    }
    let preserve_running = match client::status(&ctx.paths).await {
        Ok(Some(status)) => {
            ensure!(
                status.managed,
                "a foreground daemon is running; stop it before setting up the service"
            );
            true
        }
        Ok(None) => false,
        Err(error) if error.is::<client::ProtocolMismatch>() => {
            // Use the service manager without speaking the incompatible protocol.
            // stop checks the installed state directory and waits for the socket
            // to become unreachable before setup can replace the definition.
            ensure!(
                service::file(&ctx.paths, platform).is_file(),
                "daemon protocol mismatch and no installed service; stop the foreground daemon, then rerun `shoal setup`"
            );
            if !ctx.json {
                eprintln!("Updating daemon service after a protocol change...");
            }
            stop(&ctx.paths).await.context(
                "could not stop the incompatible daemon service; stop any foreground daemon before rerunning `shoal setup`",
            )?;
            false
        }
        Err(error) => return Err(error),
    };
    let (config, config_created) = crate::config::Config::install(&ctx.paths)?;
    service::setup(&ctx.paths, &executable, preserve_running).await?;
    client::wait(&ctx.paths, true).await?;
    ctx.emit_styled(
        Style::Success,
        "Daemon service installed and running",
        json!({
            "running": true,
            "service_file": service::file(&ctx.paths, platform),
            "config": config,
            "config_created": config_created,
            "shell_init": shell::INIT_COMMAND,
        }),
    )?;
    if !ctx.json {
        if config_created {
            println!("Wrote the defaults to {}", config.display());
        }
        println!(
            "\nAdd this line to ~/.zshrc or ~/.bashrc for directory navigation and tab completion:\n\n{}",
            shell::INIT_COMMAND
        );
    }
    Ok(0)
}

pub(super) fn config(ctx: &Context, command: ConfigCommand) -> Result<i32> {
    let (name, (config, backup)) = match command {
        ConfigCommand::Reset => (
            "default".to_owned(),
            crate::config::Config::reset(&ctx.paths)?,
        ),
        ConfigCommand::Install { name } => {
            let installed = crate::config::Config::install_named(&ctx.paths, &name)?;
            (name, installed)
        }
    };
    let message = match &backup {
        Some(backup) => format!(
            "Moved your config to {}\nInstalled config {name} at {}",
            backup.display(),
            config.display()
        ),
        None => format!("Installed config {name} at {}", config.display()),
    };
    ctx.emit(
        &message,
        json!({"name": name, "config": config, "backup": backup}),
    )?;
    Ok(0)
}

pub(super) async fn run(ctx: Context, command: DaemonCommand) -> Result<i32> {
    match command {
        DaemonCommand::Run { managed } => daemon::run(ctx.paths, managed).await?,
        DaemonCommand::Status => {
            let status = client::status(&ctx.paths).await?;
            let running = status.is_some();
            let message = status
                .as_ref()
                .map(|s| {
                    format!(
                        "Daemon running (PID {}, version {}{})",
                        s.pid,
                        s.version,
                        match s.unread_notifications {
                            0 => String::new(),
                            n => format!(", {n} new notifications"),
                        }
                    )
                })
                .unwrap_or_else(|| "Daemon is not running".into());
            ctx.emit_styled(
                if running {
                    Style::Success
                } else {
                    Style::Warning
                },
                &message,
                json!({"running": running, "daemon": status, "socket": ctx.paths.socket}),
            )?;
            return Ok(if running { 0 } else { 1 });
        }
        DaemonCommand::Start => {
            service::start(&ctx.paths).await?;
            client::wait(&ctx.paths, true).await?;
            ctx.emit_styled(Style::Success, "Daemon started", json!({"running": true}))?;
        }
        DaemonCommand::Stop => {
            stop(&ctx.paths).await?;
            ctx.emit_styled(Style::Success, "Daemon stopped", json!({"running": false}))?;
        }
        DaemonCommand::Restart => {
            // Service administration must still work after a protocol upgrade.
            if let Ok(Some(status)) = client::status(&ctx.paths).await {
                ensure!(
                    status.managed,
                    "foreground daemon: stop it and run `shoal daemon run` again"
                );
            }
            stop(&ctx.paths).await?;
            service::start(&ctx.paths).await?;
            client::wait(&ctx.paths, true).await?;
            ctx.emit_styled(Style::Success, "Daemon restarted", json!({"running": true}))?;
        }
    }
    Ok(0)
}

/// Stop a foreground daemon over the socket, or a managed one via the OS.
async fn stop(paths: &Paths) -> Result<()> {
    match client::status(paths).await {
        Ok(Some(status)) if !status.managed => {
            client::call(paths, Method::Shutdown).await?;
        }
        Err(error) if !service::file(paths, Platform::current()?).exists() => {
            return Err(error);
        }
        _ => service::stop(paths).await?,
    }
    client::wait(paths, false).await
}
