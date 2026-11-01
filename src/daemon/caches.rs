//! Emptying a repository's shared tool caches, which only happens while
//! nothing Shoal started can be using them.
use anyhow::Result;
use rusqlite::{Connection, TransactionBehavior};

pub(crate) use crate::client::cleanup::Emptying;
use crate::{
    caches::Layout,
    daemon::{store, workspace::Manager},
    model::Repository,
};

impl Manager {
    /// Empty the repository's shared caches unless a tracked execution in one
    /// of its workspaces or its seed build is running; `None` when it has none.
    pub(crate) async fn empty_caches(
        &self,
        repo: &Repository,
        dry_run: bool,
    ) -> Result<Option<Emptying>> {
        let Some(layout) = Layout::of(repo) else {
            return Ok(None);
        };
        if !dry_run {
            // An interrupted pass may have left emptied caches behind.
            let purging = layout.clone();
            tokio::task::spawn_blocking(move || purging.purge()).await??;
        }
        let caches = layout.list()?;
        if caches.is_empty() {
            return Ok(None);
        }
        let Some(_seed) = self.pause_idle_seed(&repo.id) else {
            return Ok(Some(Emptying::Busy { caches }));
        };
        let (repository_id, moving) = (repo.id.clone(), layout.clone());
        let moved = self
            .store
            .run(move |db| move_unless_running(db, &repository_id, &moving, dry_run))
            .await?;
        let Some(caches) = moved else {
            return Ok(Some(Emptying::Busy {
                caches: layout.list()?,
            }));
        };
        if !dry_run {
            tokio::task::spawn_blocking(move || layout.purge()).await??;
        }
        Ok(Some(Emptying::Emptied { caches }))
    }
}

/// Move the repository's caches aside, or with `dry_run` list them, unless
/// one of its tracked executions is running; `None` when one is. Registering
/// an execution prepares the caches under the same write lock, so none can
/// start in between.
fn move_unless_running(
    db: &mut Connection,
    repository_id: &str,
    layout: &Layout,
    dry_run: bool,
) -> Result<Option<Vec<String>>> {
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    if store::repository_has_executions(&tx, repository_id)? {
        return Ok(None);
    }
    let caches = if dry_run {
        layout.list()?
    } else {
        layout.move_aside()?
    };
    tx.commit()?;
    Ok(Some(caches))
}

#[cfg(test)]
mod tests;
