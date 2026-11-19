//! The daemon's answers to [`AppMethod`] requests, and who may send them:
//! a scoped caller reads the config of its own repository and of the
//! spaces its scope reaches, and the app store's entries, which belong to
//! no space, like any caller.
use std::sync::Arc;

use anyhow::Result;
use shoal_protocol::{apps::store::EntryPiece, space_names::SpaceSelector};

use super::super::{
    answer::Answer,
    apps::{Holder, Writing},
    manager::Manager,
    scope::{Caller, Scoped},
};
use crate::protocol::{AppMethod, Body, ConfigTarget};

pub(super) async fn handle(manager: &Arc<Manager>, method: AppMethod) -> Result<Answer> {
    Ok(Answer::Body(match method {
        AppMethod::ReadAppConfig { app, target } => {
            Body::AppConfig(manager.app_config(&app, target).await?)
        }
        AppMethod::ReadAppEntry { app, scope, key } => {
            let stored = (manager.read_kept_entry(Holder::own(scope), app, key)).await?;
            Body::AppEntryPieces(stored.map(EntryPiece::split).unwrap_or_default())
        }
        AppMethod::ListAppEntries { app, scope, prefix } => {
            let listed = manager.list_kept_entries(Holder::own(scope), app, prefix);
            return Ok(Answer::pages(listed.await, Body::AppEntries));
        }
        AppMethod::DeleteAppEntry {
            app,
            scope,
            key,
            replaces,
        } => {
            let holder = Holder::own(scope);
            manager
                .delete_kept_entry(holder, app, key, replaces)
                .await?;
            Body::Ok
        }
        AppMethod::WriteAppEntry {
            app,
            scope,
            write,
            size,
        } => {
            let manager = manager.clone();
            let writing = Writing {
                holder: Holder::own(scope),
                app,
                write,
                size,
            };
            return Ok(Answer::connection(move |stream, id, _guard| async move {
                manager.answer_write(stream, id, writing).await
            }));
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
            AppMethod::ReadAppEntry { .. }
            | AppMethod::ListAppEntries { .. }
            | AppMethod::WriteAppEntry { .. }
            | AppMethod::DeleteAppEntry { .. } => None,
        })
    }
}
