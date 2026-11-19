//! The daemon's answers to [`AppMethod`] requests, and who may send them:
//! a scoped caller reads the config of its own repository and of the
//! spaces its scope reaches, and the app store's entries, which belong to
//! no space, like any caller.
use std::sync::Arc;

use anyhow::Result;
use shoal_protocol::{
    apps::store::{AppScope, EntryPiece},
    scope_tokens::ScopeToken,
    space_names::SpaceSelector,
};

use super::super::{
    answer::Answer,
    apps::{Holder, Keeper, Socket, Watched, Writing},
    manager::Manager,
    scope::{Caller, Scoped},
};
use crate::protocol::{AppMethod, Body, ConfigTarget};

pub(super) async fn handle(
    manager: &Arc<Manager>,
    method: AppMethod,
    scope: Option<&ScopeToken>,
) -> Result<Answer> {
    let keeper = match method.kept_in() {
        Some(kept) => keeper(manager, kept).await?,
        None => Keeper::Here(Holder::Own),
    };
    if keeper == Keeper::Hub && !matches!(method, AppMethod::WriteAppEntry { .. }) {
        let (manager, scope) = (manager.clone(), scope.cloned());
        return Ok(Answer::connection(move |stream, id, startup| async move {
            // Each write takes an operation of its own.
            drop(startup);
            manager.relay_from_hub(stream, id, scope, method).await
        }));
    }
    let holder = match &keeper {
        Keeper::Here(holder) => holder.clone(),
        Keeper::Hub => Holder::Shared,
    };
    Ok(Answer::Body(match method {
        AppMethod::ReadAppConfig { app, target } => {
            Body::AppConfig(manager.app_config(&app, target).await?)
        }
        AppMethod::ReadAppEntry { app, key, .. } => {
            let stored = manager.read_kept_entry(holder, app, key).await?;
            Body::AppEntryPieces(stored.map(EntryPiece::split).unwrap_or_default())
        }
        AppMethod::ListAppEntries { app, prefix, .. } => {
            let listed = manager.list_kept_entries(holder, app, prefix);
            return Ok(Answer::pages(listed.await, Body::AppEntries));
        }
        AppMethod::DeleteAppEntry {
            app, key, replaces, ..
        } => {
            manager
                .delete_kept_entry(holder, app, key, replaces)
                .await?;
            Body::Ok
        }
        AppMethod::WriteAppEntry {
            app, write, size, ..
        } => {
            let manager = manager.clone();
            let writing = Writing {
                keeper,
                app,
                write,
                size,
                scope: scope.cloned(),
            };
            return Ok(Answer::connection(move |stream, id, _guard| async move {
                manager.answer_write(stream, id, writing).await
            }));
        }
        AppMethod::WatchAppEntries { app, prefix, .. } => {
            let manager = manager.clone();
            let scope = scope.cloned();
            let watched = Watched {
                holder,
                app,
                prefix,
            };
            return Ok(Answer::connection(move |stream, id, startup| async move {
                // Each write takes an operation of its own.
                drop(startup);
                let mut socket = Socket::new(manager.clone(), stream, id, scope);
                manager.watch_kept_entries(watched, &mut socket).await
            }));
        }
    }))
}

/// Who keeps the entries in `kept` this daemon's callers reach: the hub
/// the shared ones, once this machine joined one, and else this daemon.
async fn keeper(manager: &Manager, kept: AppScope) -> Result<Keeper> {
    Ok(match kept {
        AppScope::Shared if manager.shares_through_hub().await? => Keeper::Hub,
        kept => Keeper::Here(Holder::own(kept)),
    })
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
            | AppMethod::DeleteAppEntry { .. }
            | AppMethod::WatchAppEntries { .. } => None,
        })
    }
}
