//! What apps built on Shoal, such as Quay, keep in it: the settings the
//! user gives each in its `[apps.<name>]` tables, which the daemon resolves
//! and hands to the app unread, and the app store, the entries each app
//! writes under keys of its own, which the daemon keeps in its store, reads
//! out and watches for the app, never reading their bytes.
use anyhow::Result;
use shoal_protocol::{apps::AppName, protocol::ConfigTarget};

use super::manager::Manager;

mod entries;
mod kept;
mod watch;
mod writes;

pub(crate) use entries::Holder;
pub(crate) use watch::{Socket, Watched, Watcher};
pub(crate) use writes::{Keeper, Writing};

/// What the app store keeps while the daemon runs.
pub(crate) struct Apps {
    /// Counts the changes of the entries this daemon keeps, which wakes
    /// their watches.
    changes: tokio::sync::watch::Sender<u64>,
}

impl Default for Apps {
    fn default() -> Self {
        Self {
            changes: tokio::sync::watch::Sender::new(0),
        }
    }
}

impl Apps {
    /// Wake the watches after a change of the entries.
    fn changed(&self) {
        self.changes.send_modify(|changes| *changes += 1);
    }

    /// What wakes a watch after each change of the entries.
    fn changes(&self) -> tokio::sync::watch::Receiver<u64> {
        self.changes.subscribe()
    }
}

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
