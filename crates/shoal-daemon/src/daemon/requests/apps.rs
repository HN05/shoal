//! The daemon's answers to [`AppMethod`] requests, and who may send them:
//! a scoped caller reads the config of its own repository and of the
//! spaces its scope reaches, and the app store's entries, which belong to
//! no space, like any caller.
use std::sync::Arc;

use anyhow::Result;
use shoal_apps::{Holder, Watched, Writing};
use shoal_protocol::{
    apps::store::{AppScope, EntryPiece},
    scope_tokens::ScopeToken,
    space_names::SpaceSelector,
};

use super::super::{
    answer::Answer,
    apps::{Keeper, Socket},
    manager::Manager,
    scope::{Caller, Scoped},
    target_checks::{Checked, TargetCheck},
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
            // A watch goes on, and each of its writes takes an operation of
            // its own; anything else holds its operation until answered.
            let operation =
                (!matches!(method, AppMethod::WatchAppEntries { .. })).then_some(startup);
            let held = operation.is_some();
            manager
                .relay_from_hub(stream, id, scope, method, held)
                .await
        }));
    }
    let holder = match &keeper {
        Keeper::Here(holder) => holder.clone(),
        Keeper::Hub => Holder::Shared,
    };
    Ok(Answer::Body(match method {
        AppMethod::ReadAppConfig { app, target } => {
            Body::AppConfig(manager.apps.app_config(&**manager, &app, target).await?)
        }
        AppMethod::ReadAppEntry { app, key, .. } => {
            let stored = manager.apps.read_entry(holder, app, key).await?;
            Body::AppEntryPieces(stored.map(EntryPiece::split).unwrap_or_default())
        }
        AppMethod::ListAppEntries { app, prefix, .. } => {
            let listed = manager.apps.list_entries(holder, app, prefix);
            return Ok(Answer::pages(listed.await, Body::AppEntries));
        }
        AppMethod::DeleteAppEntry {
            app, key, replaces, ..
        } => {
            manager
                .apps
                .delete_entry(holder, app, key, replaces)
                .await?;
            Body::Ok
        }
        AppMethod::WriteAppEntry {
            app, write, size, ..
        } => {
            let manager = manager.clone();
            let writing = Writing { app, write, size };
            let scope = scope.cloned();
            return Ok(Answer::connection(
                move |stream, id, operation| async move {
                    // Held until the entry is written and answered.
                    let _operation = operation;
                    manager
                        .answer_write(stream, id, keeper, writing, scope)
                        .await
                },
            ));
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
                manager.apps.watch_entries(watched, &mut socket).await
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

impl Checked for AppMethod {
    fn target_check(&self) -> Option<TargetCheck> {
        match self {
            Self::ReadAppConfig { .. }
            | Self::ReadAppEntry { .. }
            | Self::ListAppEntries { .. }
            | Self::WriteAppEntry { .. }
            | Self::DeleteAppEntry { .. }
            | Self::WatchAppEntries { .. } => None,
        }
    }
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
                        .coordination
                        .own_repository(
                            manager,
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
