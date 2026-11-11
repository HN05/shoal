//! `shoal config install` and `shoal config reset`: installing a packaged
//! config.
use crate::config_file::ConfigExt;
use anyhow::Result;

use crate::cli::{commands::configuration, context::Context, ui};
use crate::client::output::daemon::ConfigInstalled;

/// The named packaged config, else one chosen from the packaged configs.
pub(in crate::cli::commands) fn packaged_config(
    ctx: &Context,
    name: Option<String>,
) -> Result<String> {
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
pub(in crate::cli::commands) async fn install_config(
    ctx: &Context,
    name: Option<String>,
) -> Result<i32> {
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
    let reloaded = configuration::reload_daemon(ctx).await;
    ctx.emit(
        &configuration::reload_message(message, reloaded),
        ConfigInstalled {
            name: name.to_owned(),
            config,
            backup,
            daemon_reloaded: reloaded,
        },
    )?;
    Ok(0)
}
