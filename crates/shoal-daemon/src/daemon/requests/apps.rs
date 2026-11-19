//! The daemon's answers to [`AppMethod`] requests, and who may send them:
//! a scoped caller reads the config of its own repository and of the
//! spaces its scope reaches.
use std::sync::Arc;

use anyhow::Result;
use shoal_protocol::space_names::SpaceSelector;

use super::super::{
    answer::Answer,
    manager::Manager,
    scope::{Caller, Scoped},
};
use crate::protocol::{AppMethod, Body, ConfigTarget};

pub(super) async fn handle(manager: &Arc<Manager>, method: AppMethod) -> Result<Answer> {
    Ok(Answer::Body(match method {
        AppMethod::ReadAppConfig { app, target } => {
            Body::AppConfig(manager.app_config(&app, target).await?)
        }
    }))
}

impl Scoped for AppMethod {
    async fn scope(
        &mut self,
        manager: &Manager,
        caller: &Caller,
    ) -> Result<Option<&mut SpaceSelector>> {
        Ok(match self {
            AppMethod::ReadAppConfig { target, .. } => match target {
                ConfigTarget::Space(space) => Some(space),
                ConfigTarget::Repository(repository) => {
                    *repository = manager
                        .own_repository(
                            caller,
                            repository,
                            "scoped processes read only their own repository's config",
                        )
                        .await?;
                    None
                }
                ConfigTarget::Global => None,
            },
        })
    }
}
