//! `shoal daemon install`: setting up the OS service, and noting a missing
//! terminal host or shell setup.
use crate::config_file::ConfigExt;
use std::path::PathBuf;

use anyhow::{Context as _, Result, ensure};

use super::daemon::stop_daemon;
use crate::client::output::daemon::Installed;
use crate::{
    cli::{client, context::Context, output::Style, shell},
    config::repo::ConfigLayers,
    paths::Paths,
    protocol::Exit,
    service::{self, Platform},
    sessions::HostName,
    terminal,
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
            let stopping = stop_daemon(&ctx.paths, Exit::Restart);
            ctx.progress("Stopping incompatible daemon", stopping).await.context(
                "could not stop the incompatible daemon service; stop any foreground daemon before rerunning `shoal daemon install`",
            )?;
            false
        }
        Err(error) => return Err(error),
    };
    let (config, config_created) = crate::config::Config::install(&ctx.paths)?;
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

#[cfg(test)]
mod tests;
