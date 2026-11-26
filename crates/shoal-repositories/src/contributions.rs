//! Contribution repositories, which Shoal removes once nothing of them is
//! left.
use std::{path::Path, time::Duration};

use anyhow::{Context, Result, ensure};
use rusqlite::params;
use shoal_core::git;
use shoal_daemon_base::{
    alerts::NewAlert,
    notifications::{About, AlertKind},
    ownership::paths::real_directory_identity,
};
use shoal_ids::repository_ids::RepositoryId;
use shoal_protocol::{model::Repository, time::UnixTime};

use crate::{Repositories, daemon::Daemon};

pub mod holds;

impl Repositories {
    /// Remove every contribution repository nothing has been left of for a
    /// grace period. A failure in one does not skip the others; the grace
    /// period of one no longer left alone ends.
    pub async fn sweep_contributions(&self, daemon: &dyn Daemon) -> Result<()> {
        let repositories = self.list().await?;
        self.begin_grace_pass().await?;
        let mut failed = Vec::new();
        for repo in repositories {
            if repo.contribution
                && let Err(error) = self.sweep_contribution(daemon, &repo).await
            {
                failed.push(format!("{}: {error:#}", repo.display_name()));
            }
        }
        self.end_unclaimed_grace().await?;
        ensure!(failed.is_empty(), "{}", failed.join("; "));
        Ok(())
    }

    /// Start or continue the contribution's grace period while nothing of it
    /// is left, and remove it once the period ends. Checking before removal
    /// takes its gates leaves the seed build of a contribution in use alone.
    async fn sweep_contribution(&self, daemon: &dyn Daemon, repo: &Repository) -> Result<()> {
        if self.contribution_in_use(&repo.id).await? {
            return Ok(());
        }
        // Nothing else fetches a contribution without workspaces, and its
        // remotes' new commits must be here to keep the checkout's.
        fetch_remotes(&repo.path).await?;
        if self.checkout_has_work(repo).await? {
            return Ok(());
        }
        let grace = self.repository_settings(&repo.id).await?.cleanup.grace();
        if UnixTime::now() < self.claim_grace(&repo.id, grace).await? {
            return Ok(());
        }
        // Read before the removal deletes the checkout with its remotes.
        let fork = fork_url(&repo.path).await?;
        if let Some(removal) = self.remove_finished_contribution(daemon, &repo.id).await? {
            let message = removal_notice(repo, &removal.path, fork.as_deref());
            let alert = NewAlert::shoal(AlertKind::RepositoryRemoved, About::named(None), message);
            self.notifications.notify(daemon, alert).await;
        }
        Ok(())
    }

    /// Whether the contribution's checkout holds work no remote keeps now,
    /// such as what a chat in it left; a missing checkout holds none. Remotes
    /// are asked which branches they have, since tracking refs can be stale.
    pub(crate) async fn checkout_has_work(&self, repo: &Repository) -> Result<bool> {
        if !repo.path.is_dir() {
            return Ok(false);
        }
        let advertised = advertised_refs(&repo.path).await?;
        let kept = git::present_heads(&repo.path, &advertised).await?;
        git::status::unretained_work(&repo.path, &kept).await
    }

    async fn contribution_in_use(&self, id: &RepositoryId) -> Result<bool> {
        if self.contribution_holds.held(id) {
            return Ok(true);
        }
        let (id, records) = (id.to_owned(), self.records.clone());
        self.store
            .run(move |db| records.repository_in_use(db, &id))
            .await
    }

    /// Record the removal of a contribution nothing is left of, in the
    /// transaction that checks it, so new work is either seen here or refused
    /// afterwards; `false` when something is left. The caller holds the
    /// registry gate, under which a running `shoal contribute` holds it.
    pub async fn begin_contribution_removal(&self, repo: &Repository) -> Result<bool> {
        if self.contribution_holds.held(&repo.id) {
            return Ok(false);
        }
        let (id, records) = (repo.id.clone(), self.records.clone());
        let directory = real_directory_identity(&repo.path)?;
        self.store
            .write(move |tx| {
                if records.repository_in_use(tx, &id)? {
                    return Ok(false);
                }
                tx.execute(
                    "INSERT INTO repository_removals(repository_id,directory_id,workspace_paths)
                     VALUES (?1,?2,'[]') ON CONFLICT(repository_id) DO NOTHING",
                    params![id, directory],
                )?;
                Ok(true)
            })
            .await
    }

    /// When the contribution `id`'s grace period ends, starting it now when
    /// none runs, and recording that this pass still wants to remove it.
    async fn claim_grace(&self, id: &RepositoryId, grace: Duration) -> Result<UnixTime> {
        let id = id.to_owned();
        let cleanup_at = UnixTime::now() + grace;
        self.store
            .run(move |db| {
                Ok(db.query_row(
                    "INSERT INTO contribution_grace(repository_id, cleanup_at, claimed) VALUES (?1, ?2, 1)
                     ON CONFLICT(repository_id) DO UPDATE SET claimed=1
                     RETURNING cleanup_at",
                    params![id, cleanup_at],
                    |row| row.get(0),
                )?)
            })
            .await
    }

    /// Start a sweep pass: each contribution in it claims its grace period
    /// again.
    async fn begin_grace_pass(&self) -> Result<()> {
        self.store
            .run(|db| {
                db.execute("UPDATE contribution_grace SET claimed=0", [])?;
                Ok(())
            })
            .await
    }

    /// End the grace periods no contribution in this pass claimed: those
    /// stopped qualifying, so a later pass starts a new one.
    async fn end_unclaimed_grace(&self) -> Result<()> {
        self.store
            .run(|db| {
                db.execute("DELETE FROM contribution_grace WHERE claimed=0", [])?;
                Ok(())
            })
            .await
    }
}

/// Fetch every remote of the checkout, pruning branches they deleted; a
/// missing checkout has nothing to fetch.
async fn fetch_remotes(checkout: &Path) -> Result<()> {
    if !checkout.is_dir() {
        return Ok(());
    }
    for remote in &git::remote_names(checkout).await? {
        git::fetch::remote(checkout, remote)
            .await
            .with_context(|| format!("could not fetch {remote}"))?;
    }
    Ok(())
}

/// The URL of the user's fork the checkout pushes to, if it has one; a
/// missing checkout has none.
async fn fork_url(checkout: &Path) -> Result<Option<String>> {
    if !checkout.is_dir() {
        return Ok(None);
    }
    git::configured_remote_url(checkout, git::FORK_REMOTE).await
}

/// The notice of a removed contribution, naming the fork it leaves on the
/// forge, which a later `shoal contribute` reuses.
fn removal_notice(repo: &Repository, path: &Path, fork: Option<&str>) -> String {
    let removed = format!(
        "Removed contribution repository {} at {}",
        repo.display_name(),
        path.display()
    );
    match fork {
        Some(fork) => format!(
            "{removed}. Your fork {fork} stays on the forge; delete it there when you no longer need it"
        ),
        None => removed,
    }
}

/// The branches and tags every remote of the checkout advertises now.
async fn advertised_refs(checkout: &Path) -> Result<Vec<git::RemoteHead>> {
    let mut advertised = Vec::new();
    for remote in &git::remote_names(checkout).await? {
        let listed = async {
            let mut refs = git::remote_heads(checkout, remote).await?;
            refs.extend(git::remote_tags(checkout, remote).await?);
            anyhow::Ok(refs)
        };
        advertised.extend(
            listed
                .await
                .with_context(|| format!("could not list the refs of {remote}"))?,
        );
    }
    Ok(advertised)
}

#[cfg(test)]
mod tests;
