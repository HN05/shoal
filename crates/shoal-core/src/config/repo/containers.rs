//! The `[containers]` section: a workspace's Compose project.
use crate::config::options::Options;

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
pub(crate) use shoal_protocol::containers::ContainerStart;

use super::setup::inside_worktree;

#[derive(Debug, Default, Clone, Deserialize, Serialize, Options)]
#[serde(default, deny_unknown_fields)]
#[options(resolved = Containers)]
pub struct ContainersConfig {
    pub compose: Option<String>,
    #[option(list)]
    pub ports: Vec<String>,
    #[option(default = ContainerStart::default())]
    pub start: Option<ContainerStart>,
}

impl ContainersConfig {
    pub(super) fn validate(&self) -> Result<()> {
        if let Some(compose) = &self.compose {
            ensure!(
                inside_worktree(compose),
                "containers.compose must be a relative path inside the worktree: {compose}"
            );
        }
        for name in &self.ports {
            crate::validate::lowercase_name("port", name)
                .with_context(|| format!("invalid containers.ports entry: {name}"))?;
        }
        Ok(())
    }
}
