//! A running execution's wrapper registers again after continuing from the
//! installed binary: it gets a new plan and records what its registration
//! records.
use anyhow::{Context, Result};
use rusqlite::{OptionalExtension, TransactionBehavior};
use std::collections::BTreeMap;

use super::{
    super::{Manager, places::ExecutionPlace},
    ProcessDirectories,
};
use crate::{
    config::repo::Cache,
    daemon::store,
    model::{ExecutionPlan, PortReservation, Repository},
};

/// What a registration reads from the execution's record and workspace.
struct Registration {
    scope_token: String,
    ports: Vec<PortReservation>,
    directories: ProcessDirectories,
}

impl Manager {
    /// The plan of execution `id` of workspace `workspace_id` for its wrapper,
    /// which registers again where it runs now, at `place`. The workspace's
    /// ports and environment are read again, as for a new execution.
    pub(crate) async fn reregister_execution(
        &self,
        id: &str,
        workspace_id: &str,
        place: Option<ExecutionPlace>,
    ) -> Result<ExecutionPlan> {
        let workspace = self.workspace(workspace_id).await?;
        let settings = self.workspace_settings(&workspace).await?;
        let repository = self.repository(&workspace.repository_id).await?;
        // A place's ownership moves only under the lock its closes hold.
        let _ownership = match place {
            Some(_) => Some(self.place_ownership.lock().await),
            None => None,
        };
        let registration = {
            let (id, workspace_id) = (id.to_owned(), workspace.id.clone());
            let (repository, caches) = (repository.clone(), settings.caches.clone());
            self.store
                .run(move |db| register_again(db, &id, &workspace_id, place, &repository, &caches))
                .await?
        };
        let environment = crate::env::render_configured_environment(
            &settings.env,
            &workspace,
            crate::repository::name(&repository),
            &registration.ports,
            &registration.directories.caches,
            registration.directories.tmp_dir.as_deref(),
        );
        Ok(ExecutionPlan {
            id: id.to_owned(),
            workspace,
            scope_token: registration.scope_token,
            setup_cmd: None,
            copy_ignored: false,
            ports: registration.ports,
            environment,
            land: None,
        })
    }
}

/// Record `place` for execution `id` and read what its plan needs, in one
/// transaction, as [`Manager::persist_execution`] does for a new one.
fn register_again(
    db: &mut rusqlite::Connection,
    id: &str,
    workspace_id: &str,
    place: Option<ExecutionPlace>,
    repository: &Repository,
    caches: &BTreeMap<String, Cache>,
) -> Result<Registration> {
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    // Only commands, which may reattach, keep their token.
    let scope_token = tx
        .query_row(
            "SELECT scope_token FROM executions WHERE id=?1",
            [id],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()?
        .flatten()
        .context("the execution cannot register again")?;
    if let Some(place) = &place {
        super::super::places::record_execution_place(&tx, id, workspace_id, place)?;
    }
    let ports = store::ports(&tx, Some(workspace_id))?;
    let directories = ProcessDirectories::prepare(repository, caches, workspace_id)?;
    tx.commit()?;
    Ok(Registration {
        scope_token,
        ports,
        directories,
    })
}
