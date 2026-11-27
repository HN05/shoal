//! A running execution's wrapper registers again after continuing from the
//! installed binary: it gets a new plan and records what its registration
//! records.
use anyhow::{Context, Result};
use shoal_ids::execution_ids::ExecutionId;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::scope_tokens::ScopeToken;

use crate::Agents;
use crate::daemon::Daemon;
use crate::executions::homes::Home;
use crate::executions::homes::Provisions;
use crate::model::ExecutionPlan;
use crate::places::ExecutionPlace;

impl Agents {
    /// The plan of execution `id` of the space `space_id` for its wrapper,
    /// which registers again where it runs now, at `place`. What the plan
    /// takes from the space is read again, as for a new execution.
    pub async fn reregister_execution(
        &self,
        daemon: &dyn Daemon,
        id: &ExecutionId,
        space_id: &SpaceId,
        place: Option<ExecutionPlace>,
    ) -> Result<ExecutionPlan> {
        let home = self.execution_home(daemon, space_id).await?;
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
            reports_signals: false,
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
    let scope_token =
        shoal_store::scope_token(tx, id)?.context("the execution cannot register again")?;
    if let Some(place) = &place {
        crate::places::record_execution_place(tx, id, home.id(), place)?;
    }
    Ok((scope_token, home.provide(tx, caches)?))
}
