//! A running execution's wrapper registers again after continuing from the
//! installed binary: it gets a new plan and records what its registration
//! records.
use crate::client::execution_ids::ExecutionId;
use crate::client::scope_tokens::ScopeToken;
use crate::client::space_ids::SpaceId;
use anyhow::{Context, Result};
use rusqlite::OptionalExtension;

use super::{
    super::{Manager, places::ExecutionPlace},
    homes::{Home, Provisions},
};
use crate::model::ExecutionPlan;

impl Manager {
    /// The plan of execution `id` of the space `space_id` for its wrapper,
    /// which registers again where it runs now, at `place`. What the plan
    /// takes from the space is read again, as for a new execution.
    pub(crate) async fn reregister_execution(
        &self,
        id: &ExecutionId,
        space_id: &SpaceId,
        place: Option<ExecutionPlace>,
    ) -> Result<ExecutionPlan> {
        let home = self.execution_home(space_id).await?;
        // A place's ownership moves only under the lock its closes hold.
        let _ownership = match place {
            Some(_) => Some(self.place_ownership.lock().await),
            None => None,
        };
        let (home, scope_token, provisions) = {
            let id = id.to_owned();
            let caches = home.caches();
            self.store
                .write(move |tx| {
                    let (scope_token, provisions) = register_again(tx, &id, &home, place, &caches)?;
                    Ok((home, scope_token, provisions))
                })
                .await?
        };
        let Provisions { ports, directories } = provisions;
        let environment = home.environment(&ports, &directories);
        Ok(ExecutionPlan {
            id: id.to_owned(),
            space: home.plan_space(),
            scope_token,
            setup_cmd: None,
            copy_ignored: None,
            ports,
            environment,
            merge: None,
            unmerge: None,
        })
    }
}

/// Record `place` for execution `id` and read its scope token and what its
/// plan takes from `home`, in one transaction, as
/// [`Manager::persist_execution`] does for a new one.
fn register_again(
    tx: &rusqlite::Transaction<'_>,
    id: &ExecutionId,
    home: &Home,
    place: Option<ExecutionPlace>,
    caches: &std::collections::BTreeMap<String, crate::config::repo::Cache>,
) -> Result<(ScopeToken, Provisions)> {
    // Only commands, which may reattach, keep their token.
    let scope_token = tx
        .query_row(
            "SELECT scope_token FROM executions WHERE id=?1",
            [id],
            |row| row.get::<_, Option<ScopeToken>>(0),
        )
        .optional()?
        .flatten()
        .context("the execution cannot register again")?;
    if let Some(place) = &place {
        super::super::places::record_execution_place(tx, id, home.id(), place)?;
    }
    Ok((scope_token, home.provide(tx, caches)?))
}
