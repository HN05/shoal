//! What apps built on Shoal, such as Quay, keep in it: the settings the
//! user gives each in its `[apps.<name>]` tables, which the daemon resolves
//! and hands to the app unread.
use anyhow::Result;
use shoal_protocol::{apps::AppName, protocol::ConfigTarget};

use super::manager::Manager;

impl Manager {
    /// The table `[apps.<app>]` resolves to for `target` after every
    /// layer, none where no layer sets it.
    pub(crate) async fn app_config(
        &self,
        app: &AppName,
        target: ConfigTarget,
    ) -> Result<Option<toml::Table>> {
        let layers = self.config_layers_for(target).await?;
        let mut settings = self.config().resolve(&layers)?;
        Ok(settings.apps.remove(app.as_str()))
    }
}

#[cfg(test)]
mod tests;
