//! Releasing a resource lease, after its release hook, and giving one to the
//! user.
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, Transaction, params};
use shoal_ids::space_ids::SpaceId;

use super::records::named_lease;
use crate::{
    Leases,
    daemon::{Daemon, Records},
    gates::GuardMode,
    hooks::{Hook, HookKind},
    model::Workspace,
    store,
};
use shoal_protocol::acquirers::Acquirer;
use shoal_protocol::approvals::Target;
pub use shoal_protocol::resources::{ResourceLease, Scope};
use shoal_protocol::space_names::SpaceSelector;

impl Leases {
    pub async fn release_resource(
        &self,
        daemon: &dyn Daemon,
        selector: &SpaceSelector,
        pool: String,
        name: String,
    ) -> Result<()> {
        self.release_resource_held_by(daemon, selector, pool, name, None)
            .await
            .map(drop)
    }

    /// Release the lease `name` of `pool` in `selector`'s workspace, unless
    /// `holder` names an acquirer it no longer has, as when the user took
    /// over an agent's lease since a release of it was decided. Only a
    /// release without `holder`, which someone asked for, is activity.
    /// Whether it released the lease.
    pub async fn release_resource_held_by(
        &self,
        daemon: &dyn Daemon,
        selector: &SpaceSelector,
        pool: String,
        name: String,
        holder: Option<&Acquirer>,
    ) -> Result<bool> {
        let workspace = daemon.workspace(selector).await?;
        if holder.is_none() {
            daemon.touch(&workspace.id).await;
        }
        let hook = daemon
            .hook(&workspace, HookKind::PreResourceRelease)
            .await?;
        let mode = if hook.is_some() {
            GuardMode::Exclusive
        } else {
            GuardMode::Shared
        };
        let _resources = self.gates.resource_guard(&workspace.id, mode)?;
        let (id, lookup_pool, lookup_name) = (workspace.id.clone(), pool.clone(), name.clone());
        let owned = self
            .store
            .run(move |db| releasable_lease(db, &id, &lookup_pool, &lookup_name))
            .await?;
        if let Some(holder) = holder
            && owned.as_ref().is_none_or(|lease| lease.acquirer != *holder)
        {
            return Ok(false);
        }
        if let Some(lease) = &owned {
            self.run_resource_release_hook(daemon, &workspace, lease, hook.as_deref())
                .await?;
        }
        let holder = holder.cloned();
        let records = self.records.clone();
        self.store
            .write(move |tx| {
                release_lease(tx, &*records, &workspace, &pool, &name, holder.as_ref())
            })
            .await
    }

    /// Make the lease `name` of `pool` in `workspace` the user's.
    pub async fn give_resource_lease_to_user(
        &self,
        workspace: &SpaceId,
        pool: &str,
        name: &str,
    ) -> Result<()> {
        let (workspace, pool, name) = (workspace.clone(), pool.to_owned(), name.to_owned());
        self.store
            .write(move |tx| {
                tx.execute(
                    "UPDATE resource_leases SET acquirer=?4,acquirer_id=NULL
                        WHERE workspace_id=?1 AND pool=?2 AND name=?3",
                    params![workspace, pool, name, Acquirer::User.kind()],
                )?;
                Ok(())
            })
            .await
    }

    /// Run the release hook `command` of `workspace` for `lease`, once its
    /// worktree verifies; a failure keeps the lease.
    pub async fn run_resource_release_hook(
        &self,
        daemon: &dyn Daemon,
        workspace: &Workspace,
        lease: &ResourceLease,
        command: Option<&std::path::Path>,
    ) -> Result<()> {
        if let Some(command) = command {
            daemon.verify_worktree(workspace).await?;
            (daemon.run_hook(workspace, Hook::PreResourceRelease(lease), command))
                .await
                .context("resource lease retained; retry release after fixing its hook")?;
        }
        Ok(())
    }
}

/// The workspace's lease a release would delete, once the workspace is ready.
pub(super) fn releasable_lease(
    db: &Connection,
    workspace: &SpaceId,
    pool: &str,
    name: &str,
) -> Result<Option<ResourceLease>> {
    store::require_ready(db, workspace)?;
    named_lease(db, workspace, pool, name)
}

/// Delete a lease and end its approval atomically, in either scope, so
/// release works even after the pool definition is edited or deleted;
/// nothing when `holder` names an acquirer the lease no longer has.
/// Whether it released them.
pub(super) fn release_lease(
    tx: &Transaction<'_>,
    records: &dyn Records,
    workspace: &Workspace,
    pool: &str,
    name: &str,
    holder: Option<&Acquirer>,
) -> Result<bool> {
    store::require_ready(tx, &workspace.id)?;
    if let Some(holder) = holder {
        let current = named_lease(tx, &workspace.id, pool, name)?;
        if current.is_none_or(|lease| lease.acquirer != *holder) {
            return Ok(false);
        }
    }
    let mut released = false;
    for scope in [Scope::Global, Scope::Repo(workspace.repository().clone())] {
        let target = Target::Resource {
            scope,
            pool: pool.to_owned(),
        };
        released |= records.release_approval(tx, &workspace.id, &target, name)?;
    }
    ensure!(
        tx.execute(
            "DELETE FROM resource_leases WHERE workspace_id=?1 AND pool=?2 AND name=?3",
            params![workspace.id, pool, name]
        )? == 1
            || released,
        "unknown resource lease or approval request"
    );
    Ok(true)
}
