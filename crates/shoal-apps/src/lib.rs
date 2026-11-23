//! What apps built on Shoal, such as Quay, keep in it: the settings the
//! user gives each in its `[apps.<name>]` tables, which the daemon resolves
//! and hands to the app unread, and the app store, the entries each app
//! writes under keys of its own, which the daemon keeps in its store, reads
//! out and watches for the app, never reading their bytes. [`Apps`] holds
//! what the app store keeps in memory and offers its operations; what it
//! needs of the rest of the daemon is the interface in [`daemon`]. The
//! daemon answers its requests on its socket and the hub link.
use std::sync::Arc;

use anyhow::Result;
use shoal_core::{config, refusal};
use shoal_daemon_base::config_state::ConfigState;
use shoal_protocol::{apps::AppName, protocol::ConfigTarget, time};
use shoal_store::Store;

pub mod daemon;
mod entries;
mod kept;
mod link;
mod watch;
mod writes;

pub use entries::Holder;
pub use link::frames;
pub use watch::{Watched, Watcher};
pub use writes::{Writing, ask_bytes};

/// The apps' service, which `Manager` holds.
pub struct Apps {
    store: Store,
    config: Arc<ConfigState>,
    /// Counts the changes of the entries this daemon keeps, which wakes
    /// their watches.
    changes: tokio::sync::watch::Sender<u64>,
}

impl Apps {
    pub fn new(store: Store, config: Arc<ConfigState>) -> Self {
        Self {
            store,
            config,
            changes: tokio::sync::watch::Sender::new(0),
        }
    }

    /// The table `[apps.<app>]` resolves to for `target` after every
    /// layer, none where no layer sets it.
    pub async fn app_config(
        &self,
        daemon: &dyn daemon::Repositories,
        app: &AppName,
        target: ConfigTarget,
    ) -> Result<Option<toml::Table>> {
        let layers = daemon.config_layers(target).await?;
        let mut settings = self.config.current().resolve(&layers)?;
        Ok(settings.apps.remove(app.as_str()))
    }

    /// Wake the watches after a change of the entries.
    fn changed(&self) {
        self.changes.send_modify(|changes| *changes += 1);
    }

    /// What wakes a watch after each change of the entries.
    fn changes(&self) -> tokio::sync::watch::Receiver<u64> {
        self.changes.subscribe()
    }
}

#[cfg(test)]
mod stand_in;
#[cfg(test)]
mod tests;
