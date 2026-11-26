//! How long a carried commit's ref stays in the repository that received
//! it: while its launch is on its way, or a workspace here needs the
//! commit, as the plan of its pending worktree or as a base no branch
//! holds. Then the ref goes, and Git may collect what only it kept. The
//! cleanup sweep reclaims the refs of launches that never came once their
//! holds lapse, those a restart left without holds, and those a failed
//! release left.
use crate::Spaces;
use crate::daemon::Daemon;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::Result;
use shoal_ids::repository_ids::RepositoryId;
use shoal_refs::CommitId;

/// How long a hold keeps a carried commit for a launch that never came, as
/// when the other machine lost its link before it forwarded the launch.
const UNCLAIMED: Duration = Duration::from_secs(60 * 60);

#[derive(Default)]
pub struct CarriedHolds {
    held: std::sync::Mutex<Vec<Hold>>,
    /// Held while a hold is taken, and while a release reads what needs
    /// carried refs and deletes the rest, so it never deletes one a hold
    /// taken meanwhile keeps.
    releasing: tokio::sync::Mutex<()>,
    /// Whether a sweep released every repository's carried refs since the
    /// daemon started.
    swept: AtomicBool,
    /// The next sweep releases these again.
    failed: std::sync::Mutex<Vec<RepositoryId>>,
}

/// A carried commit a launch is to start from, in the repository its origin
/// registers here.
struct Hold {
    origin: String,
    repository: RepositoryId,
    commit: CommitId,
    since: Instant,
}

impl Hold {
    fn lapsed(&self) -> bool {
        self.since.elapsed() >= UNCLAIMED
    }
}

/// By the URL of the origin its repository is registered from.
pub type Carried<'a> = (&'a str, &'a CommitId);

impl CarriedHolds {
    pub async fn take(&self, repository: &RepositoryId, (origin, commit): Carried<'_>) {
        let _releasing = self.releasing.lock().await;
        self.held.lock().unwrap().push(Hold {
            origin: origin.to_owned(),
            repository: repository.clone(),
            commit: commit.clone(),
            since: Instant::now(),
        });
    }

    /// None where nothing holds it.
    fn give_up(&self, (origin, commit): Carried<'_>) -> Option<RepositoryId> {
        let mut held = self.held.lock().unwrap();
        let position =
            (held.iter()).position(|hold| hold.origin == origin && hold.commit == *commit)?;
        Some(held.remove(position).repository)
    }

    fn kept(&self, repository: &RepositoryId) -> Vec<CommitId> {
        let held = self.held.lock().unwrap();
        (held.iter())
            .filter(|hold| hold.repository == *repository && !hold.lapsed())
            .map(|hold| hold.commit.clone())
            .collect()
    }

    fn retry(&self, repository: &RepositoryId) {
        let mut failed = self.failed.lock().unwrap();
        if !failed.contains(repository) {
            failed.push(repository.clone());
        }
    }

    fn take_failed(&self) -> Vec<RepositoryId> {
        std::mem::take(&mut *self.failed.lock().unwrap())
    }

    /// Returns their repositories.
    fn give_up_lapsed(&self) -> Vec<RepositoryId> {
        let mut held = self.held.lock().unwrap();
        let (lapsed, kept) = std::mem::take(&mut *held)
            .into_iter()
            .partition(Hold::lapsed);
        *held = kept;
        lapsed
            .into_iter()
            .map(|hold: Hold| hold.repository)
            .collect()
    }
}

struct Needs {
    held: Vec<CommitId>,
    /// The revisions pending worktrees start from or compare with.
    pending: Vec<String>,
    bases: Vec<CommitId>,
}

impl Needs {
    /// A recorded base needs the ref only while no branch holds it.
    async fn need(&self, path: &std::path::Path, commit: &CommitId) -> Result<bool> {
        if self.held.contains(commit) || self.pending.iter().any(|rev| *commit == **rev) {
            return Ok(true);
        }
        if !self.bases.contains(commit) {
            return Ok(false);
        }
        Ok(!crate::git::clone_base::held_by_branch(path, commit).await?)
    }
}

impl Spaces {
    /// Its launch ended or will never come.
    pub async fn release_carried_hold(&self, daemon: &dyn Daemon, carried: Carried<'_>) {
        if let Some(repository) = self.carried_holds.give_up(carried) {
            self.release_carried(daemon, &repository).await;
        }
    }

    /// On the first pass since the daemon started, releases every
    /// repository's, which a restart left without the holds that kept them.
    pub async fn release_unclaimed_carried(&self, daemon: &dyn Daemon) -> Result<()> {
        let mut repositories = match self.carried_holds.swept.swap(true, Ordering::Relaxed) {
            true => self.carried_holds.give_up_lapsed(),
            false => (daemon.repositories().await?.into_iter())
                .map(|repository| repository.id)
                .collect(),
        };
        for failed in self.carried_holds.take_failed() {
            if !repositories.contains(&failed) {
                repositories.push(failed);
            }
        }
        for repository in repositories {
            self.release_carried(daemon, &repository).await;
        }
        Ok(())
    }

    /// A failure is logged and leaves them to the next sweep.
    pub async fn release_carried(&self, daemon: &dyn Daemon, repository: &RepositoryId) {
        if let Err(error) = self.release_unneeded(daemon, repository).await {
            crate::log!("carried commits of repository {repository} kept: {error:#}");
            self.carried_holds.retry(repository);
        }
    }

    async fn release_unneeded(&self, daemon: &dyn Daemon, id: &RepositoryId) -> Result<()> {
        let _releasing = self.carried_holds.releasing.lock().await;
        let repositories = daemon.repositories().await?;
        let Some(repository) = repositories.iter().find(|repository| repository.id == *id) else {
            return Ok(());
        };
        let path = &repository.path;
        let carried = crate::git::clone_base::carried(path).await?;
        if carried.is_empty() {
            return Ok(());
        }
        let needs = self.carried_needs(id).await?;
        for commit in carried {
            if !needs.need(path, &commit).await? {
                crate::git::clone_base::drop_carried(path, &commit).await?;
            }
        }
        Ok(())
    }

    async fn carried_needs(&self, id: &RepositoryId) -> Result<Needs> {
        let held = self.carried_holds.kept(id);
        let id = id.clone();
        let (pending, bases) = (self.store)
            .read(move |tx| {
                let pending = crate::workspace::pending_revisions(tx, &id)?;
                Ok((pending, recorded_bases(tx, &id)?))
            })
            .await?;
        Ok(Needs {
            held,
            pending,
            bases,
        })
    }
}

fn recorded_bases(
    tx: &rusqlite::Transaction<'_>,
    repository: &RepositoryId,
) -> Result<Vec<CommitId>> {
    let mut bases = tx.prepare(
        "SELECT base_commit FROM spaces JOIN workspaces USING(id)
         WHERE repository_id=?1 AND base_commit IS NOT NULL",
    )?;
    let bases = bases.query_map([repository], |row| row.get(0))?;
    Ok(bases.collect::<rusqlite::Result<_>>()?)
}

#[cfg(test)]
mod tests;
