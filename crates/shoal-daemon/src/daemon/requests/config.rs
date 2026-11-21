//! The daemon's answers to [`ConfigMethod`] requests, who may send them, and
//! what each requires of the space it acts on.
use std::sync::Arc;

use anyhow::Result;

use super::super::{
    answer::Answer,
    manager::Manager,
    scope::{Caller, Scoped},
    target_checks::{Checked, TargetCheck},
};
use crate::protocol::{Body, ConfigMethod, ConfigTarget, RepositoryLayers};
use shoal_protocol::space_names::SpaceSelector;

pub(super) async fn handle(manager: &Arc<Manager>, method: ConfigMethod) -> Result<Answer> {
    Ok(Answer::Body(match method {
        ConfigMethod::TrackedConfig { space } => Body::TrackedConfig(
            manager
                .repositories
                .space_tracked_config(&**manager, &space)
                .await?,
        ),
        ConfigMethod::LayeredConfig { target } => Body::LayeredConfig(RepositoryLayers::of(
            &manager
                .repositories
                .config_layers_for(&**manager, target)
                .await?,
        )?),
    }))
}

impl Checked for ConfigMethod {
    fn target_check(&self) -> Option<TargetCheck> {
        match self {
            Self::TrackedConfig { .. } => Some(TargetCheck::Space),
            Self::LayeredConfig { .. } => None,
        }
    }
}

impl Scoped for ConfigMethod {
    async fn scope(
        &mut self,
        manager: &Manager,
        caller: &Caller,
    ) -> Result<Option<&mut SpaceSelector>> {
        Ok(match self {
            // The settings of its own repository choose what it opens there.
            ConfigMethod::LayeredConfig {
                target: ConfigTarget::Repository(repository),
            } => {
                *repository = manager
                    .own_repository(
                        caller,
                        repository,
                        "scoped processes read only their own repository's config",
                    )
                    .await?;
                None
            }
            ConfigMethod::LayeredConfig {
                target: ConfigTarget::Global,
            } => None,
            ConfigMethod::LayeredConfig {
                target: ConfigTarget::Space(space),
            }
            | ConfigMethod::TrackedConfig { space } => Some(space),
        })
    }
}
