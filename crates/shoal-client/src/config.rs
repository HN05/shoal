//! The repository config layers of a space or a repository, which the
//! caller resolves itself, and the values it cannot read itself.
use anyhow::Result;
use serde::de::DeserializeOwned;

use crate::connection::{Daemon, Prepared, request};
use shoal_protocol::{
    config::value::{Resolved, ValueKey},
    protocol::{ConfigMethod, ConfigTarget, RepositoryLayers},
    space_names::SpaceSelector,
};

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

/// The value of `key` for `space`, which the caller cannot read itself.
pub async fn config_value(
    daemon: &impl Daemon,
    space: SpaceSelector,
    key: ValueKey,
) -> Result<String> {
    let Resolved(value) = request(daemon, ConfigMethod::ConfigValue { space, key }).await?;
    Ok(value)
}
