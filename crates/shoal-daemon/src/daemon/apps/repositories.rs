//! The config layers apps read their settings in.
use anyhow::Result;
use futures_util::future::BoxFuture;
use shoal_apps::daemon::Repositories;
use shoal_protocol::protocol::ConfigTarget;

use crate::{config::repo::ConfigLayers, daemon::manager::Manager};

impl Repositories for Manager {
    fn config_layers(&self, target: ConfigTarget) -> BoxFuture<'_, Result<ConfigLayers>> {
        Box::pin(self.repositories.config_layers_for(self, target))
    }
}
