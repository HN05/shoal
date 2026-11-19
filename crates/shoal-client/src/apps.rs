//! Apps built on Shoal: the settings the user gives each in its
//! `[apps.<name>]` tables.
use anyhow::Result;
use shoal_protocol::{
    apps::AppName,
    protocol::{AppMethod, ConfigTarget},
};

use crate::connection::{Daemon, request};

/// The table `[apps.<app>]` resolves to for `target` after every layer,
/// none where no layer sets it.
pub async fn config(
    daemon: &impl Daemon,
    app: AppName,
    target: ConfigTarget,
) -> Result<Option<toml::Table>> {
    request(daemon, AppMethod::ReadAppConfig { app, target }).await
}
