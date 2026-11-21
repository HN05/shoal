//! Contribution repositories, which Shoal removes once nothing of them is
//! left: no work, nothing waiting to start, and nothing `shoal resume`
//! could still bring back.
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, params};
use shoal_ids::repository_ids::RepositoryId;

use super::{leases::DaemonRecords, manager::Manager, resources};
use shoal_daemon_base::ownership::paths::real_directory_identity;

use crate::{
    daemon::{notifications::AlertKind, store},
    git,
    model::Repository,
    time::UnixTime,
};

impl Manager {
    /// Remove every contribution repository nothing has been left of for a
    /// grace period. A failure in one does not skip the others.
    pub(crate) async fn sweep_contributions(&self) -> Result<()> {
        let mut failed = Vec::new();
        for repo in self.repositories.list().await? {
            if repo.contribution
                && let Err(error) = self.sweep_contribution(&repo).await
            {
                failed.push(format!("{}: {error:#}", repo.display_name()));
            }
        }
        ensure!(failed.is_empty(), "{}", failed.join("; "));
        Ok(())
    }

    /// Start or continue the contribution's grace period while nothing of it
    /// is left, and remove it once the period ends. Checking before removal
    /// takes its gates leaves the seed build of a contribution in use alone.
    async fn sweep_contribution(&self, repo: &Repository) -> Result<()> {
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
        if UnixTime::now()
            < self
                .claim_contribution_grace(repo.id.as_str(), grace)
                .await?
        {
            return Ok(());
        }
        // Read before the removal deletes the checkout with its remotes.
        let fork = fork_url(&repo.path).await?;
        if let Some(removal) = self.remove_finished_contribution(&repo.id).await? {
            let message = removal_notice(repo, &removal.path, fork.as_deref());
            self.notify(None, AlertKind::RepositoryRemoved, message)
                .await;
        }
        Ok(())
    }

    /// Whether the contribution's checkout holds work no remote keeps now,
    /// such as what a chat in it left; a missing checkout holds none. Remotes
    /// are asked which branches they have, since tracking refs can be stale.
    pub(super) async fn checkout_has_work(&self, repo: &Repository) -> Result<bool> {
        if !repo.path.is_dir() {
            return Ok(false);
        }
        let advertised = advertised_refs(&repo.path).await?;
        let kept = git::present_heads(&repo.path, &advertised).await?;
        git::status::unretained_work(&repo.path, &kept).await
    }

    async fn contribution_in_use(&self, id: &RepositoryId) -> Result<bool> {
        if self.repositories.contribution_holds().held(id) {
            return Ok(true);
        }
        let id = id.to_owned();
        self.store.run(move |db| in_use(db, &id)).await
    }

    /// Record the removal of a contribution nothing is left of, in the
    /// transaction that checks it, so new work is either seen here or refused
    /// afterwards; `false` when something is left. The caller holds the
    /// registry gate, under which a running `shoal contribute` holds it.
    pub(super) async fn begin_contribution_removal(&self, repo: &Repository) -> Result<bool> {
        if self.repositories.contribution_holds().held(&repo.id) {
            return Ok(false);
        }
        let id = repo.id.clone();
        let directory = real_directory_identity(&repo.path)?;
        self.store
            .write(move |tx| {
                if in_use(tx, &id)? {
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
}

/// Fetch every remote of the checkout, pruning branches they deleted; a
/// missing checkout has nothing to fetch.
async fn fetch_remotes(checkout: &std::path::Path) -> Result<()> {
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
async fn fork_url(checkout: &std::path::Path) -> Result<Option<String>> {
    if !checkout.is_dir() {
        return Ok(None);
    }
    git::configured_remote_url(checkout, git::FORK_REMOTE).await
}

/// The notice of a removed contribution, naming the fork it leaves on the
/// forge, which a later `shoal contribute` reuses.
fn removal_notice(repo: &Repository, path: &std::path::Path, fork: Option<&str>) -> String {
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
async fn advertised_refs(checkout: &std::path::Path) -> Result<Vec<git::RemoteHead>> {
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

/// Whether the repository still has a workspace, a queued agent, a chat, a
/// pending issue proposal, a kept branch, an unexpired checkpoint, removal
/// record or removed space's files, or a resource another repository's
/// workspace borrows.
fn in_use(db: &Connection, id: &RepositoryId) -> Result<bool> {
    Ok(recorded_use(db, id)? || resources::borrowed(db, &DaemonRecords, id)?)
}

fn recorded_use(db: &Connection, id: &RepositoryId) -> Result<bool> {
    store::exists(
        db,
        "SELECT 1 FROM spaces WHERE repository_id=?1
        UNION ALL SELECT 1 FROM agent_queue WHERE repository_id=?1
        UNION ALL SELECT 1 FROM access_requests WHERE repository_id=?1 AND active=1
        UNION ALL SELECT 1 FROM kept_branches WHERE repository_id=?1
        UNION ALL SELECT 1 FROM ci_job_reruns WHERE repository_id=?1
        UNION ALL SELECT 1 FROM checkpoints
            WHERE repository_id=?1 AND expires_at > unixepoch()
        UNION ALL SELECT 1 FROM removed_spaces
            WHERE repository_id=?1 AND expires_at > unixepoch()
        UNION ALL SELECT 1 FROM removed_space_files JOIN space_files USING(space_id)
            WHERE repository_id=?1 AND expires_at > unixepoch()",
        [id],
    )
}

#[cfg(test)]
mod tests;
