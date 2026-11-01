//! CLI OS-service administration, including offline/protocol-upgrade handling.
use std::path::PathBuf;

use anyhow::{Context as _, Result, ensure};

use crate::client::output::daemon::{ConfigInstalled, Installed, Reloaded, Running, Status};
use crate::{
    cli::{args::service::DaemonCommand, client, context::Context, output::Style, terminal, ui},
    config::repo::ConfigLayers,
    daemon,
    paths::Paths,
    protocol::Exit,
    service::{self, Platform},
    sessions::HostName,
    shell,
};

pub(super) async fn install(
    ctx: &Context,
    dry_run: bool,
    executable: Option<PathBuf>,
) -> Result<i32> {
    let executable = service::executable(executable)?;
    let platform = Platform::current()?;
    if dry_run {
        let definition = service::definition(&ctx.paths, &executable, platform)?;
        ctx.show(&definition, |definition| {
            outln!("# {}\n{}", definition.path.display(), definition.content);
        })?;
        return Ok(0);
    }
    let preserve_running = match ctx
        .progress("Checking daemon", client::status(&ctx.paths))
        .await
    {
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
            // to become unreachable before install can replace the definition.
            ensure!(
                service::file(&ctx.paths, platform).is_file(),
                "daemon protocol mismatch and no installed service; stop the foreground daemon, then rerun `shoal daemon install`"
            );
            if !ctx.json {
                eprintln!("Updating daemon service after a protocol change...");
            }
            let stopping = stop(&ctx.paths, Exit::Restart);
            ctx.progress("Stopping incompatible daemon", stopping).await.context(
                "could not stop the incompatible daemon service; stop any foreground daemon before rerunning `shoal daemon install`",
            )?;
            false
        }
        Err(error) => return Err(error),
    };
    let (config, config_created) = crate::config::Config::install(&ctx.paths)?;
    crate::config::templates::install(&ctx.paths)?;
    ctx.progress(
        "Installing daemon service",
        service::setup(&ctx.paths, &executable, preserve_running),
    )
    .await?;
    ctx.progress("Waiting for daemon", client::wait(&ctx.paths, true))
        .await?;
    let hosts = TerminalHosts::find(&ctx.paths);
    ctx.emit_styled(
        Style::Success,
        "Daemon service installed and running",
        Installed {
            running: true,
            service_file: service::file(&ctx.paths, platform),
            config: config.clone(),
            config_created,
            shell_init: shell::INIT_COMMAND.to_owned(),
            installed_terminal_hosts: hosts.installed.clone(),
        },
    )?;
    if !ctx.json {
        if config_created {
            outln!("Wrote the defaults to {}", config.display());
        }
        if let Some(note) = hosts.missing_default(&config) {
            outln!("\n{note}");
        }
        let zsh_directory = std::env::var_os("ZDOTDIR").map(PathBuf::from);
        if !shell::init_configured(&ctx.paths.home, zsh_directory.as_deref()) {
            outln!(
                "\nAdd this line to ~/.zshrc or ~/.bashrc for directory navigation and tab completion:\n\n{}",
                shell::INIT_COMMAND
            );
        }
    }
    Ok(0)
}

/// The terminal hosts this machine has, against the default host the
/// global config names.
struct TerminalHosts {
    installed: Vec<HostName>,
    /// The global `terminal.default_host`, unknown while the config does not
    /// load.
    default: Option<HostName>,
}

impl TerminalHosts {
    fn find(paths: &Paths) -> Self {
        let default = crate::config::Config::load(paths)
            .and_then(|config| config.resolve(&ConfigLayers::default()))
            .ok()
            .map(|settings| settings.terminal.default_host);
        Self {
            installed: terminal::installed_hosts(),
            default,
        }
    }

    /// What to install, or which default host to set in `config`, when the
    /// default host is not installed.
    fn missing_default(&self, config: &std::path::Path) -> Option<String> {
        let default = self.default.filter(|host| !self.installed.contains(host))?;
        Some(match self.installed.first() {
            Some(installed) => format!(
                "{default}, the default terminal host, is not installed. Install it, or set terminal.default_host = \"{installed}\" in {}.",
                config.display()
            ),
            None => format!(
                "No terminal host is installed, so agents cannot start from a terminal. Install {default}, the default host, or install another host and set terminal.default_host in {}.",
                config.display()
            ),
        })
    }
}

/// The named packaged config, else one chosen from the packaged configs.
pub(super) fn packaged_config(ctx: &Context, name: Option<String>) -> Result<String> {
    match name {
        Some(name) => Ok(name),
        None => {
            ui::require_argument(ctx, "packaged config", "name")?;
            ui::pick(
                ctx,
                "Config> ",
                crate::config::PACKAGED
                    .iter()
                    .map(|(name, _)| ((*name).to_owned(), (*name).to_owned()))
                    .collect(),
            )
        }
    }
}

/// Install the packaged config `name`, or the default one for `None`.
pub(super) async fn install_config(ctx: &Context, name: Option<String>) -> Result<i32> {
    let (name, (config, backup)) = match name {
        None => (
            "default".to_owned(),
            crate::config::Config::reset(&ctx.paths)?,
        ),
        Some(name) => {
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
    let reloaded = super::configuration::reload_daemon(ctx).await;
    ctx.emit(
        &super::configuration::reload_message(message, reloaded),
        ConfigInstalled {
            name: name.to_owned(),
            config,
            backup,
            daemon_reloaded: reloaded,
        },
    )?;
    Ok(0)
}

/// The running daemon and each directory whose file access stalls.
fn status_message(status: &crate::protocol::DaemonStatus) -> String {
    let mut message = format!(
        "Daemon running (PID {}, version {}{})",
        status.pid,
        status.version,
        match status.unread_notifications {
            0 => String::new(),
            n => format!(", {n} new notifications"),
        }
    );
    let now = crate::time::unix_seconds() as i64;
    for stall in &status.access_stalls {
        let since = crate::time::local_ago(stall.since as i64, now);
        message.push_str(&format!("\n{} (since {since})", stall.message()));
    }
    message
}

pub(super) async fn run(
    ctx: Context,
    command: DaemonCommand,
    handoff: Option<daemon::handoff::Handoff>,
) -> Result<i32> {
    match command {
        DaemonCommand::Install {
            dry_run,
            executable,
        } => return install(&ctx, dry_run, executable).await,
        DaemonCommand::Run { managed } => daemon::run(ctx.paths, managed, handoff).await?,
        DaemonCommand::Status => {
            let status = client::status(&ctx.paths).await?;
            let running = status.is_some();
            let message = status
                .as_ref()
                .map(status_message)
                .unwrap_or_else(|| "Daemon is not running".into());
            let healthy = status.as_ref().is_some_and(|s| s.access_stalls.is_empty());
            ctx.emit_styled(
                if healthy {
                    Style::Success
                } else {
                    Style::Warning
                },
                &message,
                Status {
                    running,
                    daemon: status,
                    socket: ctx.paths.socket.clone(),
                },
            )?;
            return Ok(if running { 0 } else { 1 });
        }
        DaemonCommand::Start => {
            if !service::file(&ctx.paths, Platform::current()?).is_file()
                && ui::offer(
                    &ctx,
                    "The daemon service is not installed. Install it with `shoal daemon install`?",
                )?
            {
                return install(&ctx, false, None).await;
            }
            start(&ctx).await?;
            let started = Running {
                running: true,
                stopped_work: None,
            };
            ctx.emit_styled(Style::Success, "Daemon started", started)?;
        }
        DaemonCommand::Stop => {
            ctx.progress("Stopping daemon", stop(&ctx.paths, Exit::Stop))
                .await?;
            emit_stopped_work(&ctx, "Daemon stopped", false)?;
        }
        DaemonCommand::Reload => {
            ensure!(
                client::reload_config(&ctx.paths).await?,
                "the daemon is not running; it reads the config when it starts"
            );
            ctx.emit_styled(
                Style::Success,
                "Daemon reloaded the global config",
                Reloaded { reloaded: true },
            )?;
        }
        DaemonCommand::Restart => {
            // Service administration must still work after a protocol upgrade.
            if let Ok(Some(status)) = ctx
                .progress("Checking daemon", client::status(&ctx.paths))
                .await
            {
                ensure!(
                    status.managed,
                    "foreground daemon: stop it and run `shoal daemon run` again"
                );
            }
            ctx.progress("Stopping daemon", stop(&ctx.paths, Exit::Restart))
                .await?;
            start(&ctx).await?;
            emit_stopped_work(&ctx, "Daemon restarted", true)?;
        }
    }
    Ok(0)
}

/// Stopping saves running agents and commands, as does a restart for those
/// that cannot reattach; point to restoring them.
fn emit_stopped_work(ctx: &Context, message: &str, running: bool) -> Result<()> {
    let stopped = crate::execution::recovery::any_pending(&ctx.paths);
    let message = if stopped {
        format!("{message}; restore stopped work with shoal resume --all")
    } else {
        message.to_owned()
    };
    ctx.emit_styled(
        Style::Success,
        &message,
        Running {
            running,
            stopped_work: Some(stopped),
        },
    )
}

async fn start(ctx: &Context) -> Result<()> {
    ctx.progress("Starting daemon service", service::start(&ctx.paths))
        .await?;
    ctx.progress("Waiting for daemon", client::wait(&ctx.paths, true))
        .await
}

/// Ask the daemon to exit with `exit`, then unload its service. A daemon that
/// does not answer gets SIGTERM, which stops its work saved first.
async fn stop(paths: &Paths, exit: Exit) -> Result<()> {
    if !client::exit(paths, exit).await? {
        return terminate(paths).await;
    }
    client::wait(paths, false).await?;
    // The service manager keeps a daemon's service loaded after it exits.
    if service::installed(paths) {
        service::stop(paths).await?;
    }
    Ok(())
}

/// Stop a daemon with SIGTERM through the OS service manager, or directly
/// when it runs in the foreground or does not answer status without an
/// installed service, as while it starts.
async fn terminate(paths: &Paths) -> Result<()> {
    match client::status(paths).await {
        Ok(Some(status)) if !status.managed => {
            signal_owner(
                paths,
                anyhow::anyhow!("the foreground daemon does not answer"),
            )?;
        }
        Err(error) if !service::file(paths, Platform::current()?).exists() => {
            signal_owner(paths, error)?;
        }
        _ => service::stop(paths).await?,
    }
    client::wait(paths, false).await
}

/// SIGTERM to the daemon owning the state directory; `missing` when none does.
fn signal_owner(paths: &Paths, missing: anyhow::Error) -> Result<()> {
    let owner = crate::daemon::ownership::owner(paths).ok_or(missing)?;
    crate::process::identity::signal(&owner, libc::SIGTERM)
}

#[cfg(test)]
mod tests;
