//! Config: the repository layers of the config of a space or a repository,
//! which the caller resolves under the global files it reads.
use anyhow::Result;
use serde::de::DeserializeOwned;

use crate::connection::{Daemon, Prepared};
use shoal_protocol::protocol::{ConfigMethod, ConfigTarget, RepositoryLayers};

/// The repository configuration layers that apply to `target`, below the
/// global configuration, read as `L`.
pub async fn config_layers<L: DeserializeOwned>(
    daemon: &impl Daemon,
    target: ConfigTarget,
) -> Result<L> {
    let layers = layered_config(target).send(daemon).await?;
    layers.read()
}

/// What [`config_layers`] asks for `target`, as the picker that chooses the
/// space whose layers it reads judges it.
pub fn layered_config(target: ConfigTarget) -> Prepared<RepositoryLayers> {
    Prepared::new(ConfigMethod::LayeredConfig { target })
}
