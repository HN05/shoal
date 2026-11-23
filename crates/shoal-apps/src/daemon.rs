//! What apps need of the rest of the daemon, which implements it: the
//! config layers of the repository or space an app reads its settings for.
use anyhow::Result;
use futures_util::future::BoxFuture;
use shoal_protocol::protocol::ConfigTarget;

use crate::config::repo::ConfigLayers;

/// The repositories whose config apps read.
pub trait Repositories: Send + Sync {
    /// The layers of config over the global config for `target`: none for
    /// the global config itself.
    fn config_layers(&self, target: ConfigTarget) -> BoxFuture<'_, Result<ConfigLayers>>;
}
