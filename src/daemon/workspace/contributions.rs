//! Contribution repositories, which Shoal removes once nothing of them is
//! left: no work, nothing waiting to start, and nothing `shoal restore`
//! could still bring back.
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, params};

use super::{Manager, paths::real_directory_identity};
use crate::{
    daemon::{log, notifications::NotificationKind, store},
    git,
    model::Repository,
    time::unix_seconds,
};

impl Manager {
    /// Remove every contribution repository nothing has been left of for a
    /// grace period. A failure in one does not skip the others.
    pub(crate) async fn sweep_contributions(&self) -> Result<()> {
        let mut failed = Vec::new();
        for repo in self.repositories().await? {
            if repo.contribution
                && let Err(error) = self.sweep_contribution(&repo).await
            {
                failed.push(format!("{}: {error:#}", crate::repository::name(&repo)));
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
        if unix_seconds() < self.claim_contribution_grace(&repo.id, grace).await? {
            return Ok(());
        }
        if let Some(removal) = self.remove_finished_contribution(&repo.id).await? {
            self.forget_trust(repo, &removal.workspace_paths).await;
            let message = format!(
                "Removed contribution repository {} at {}",
                crate::repository::name(repo),
                removal.path.display()
            );
            self.notify(None, NotificationKind::RepositoryRemoved, message)
                .await;
        }
        Ok(())
    }

    /// Remove the agents' trust entries for the removed repository, as
    /// `repo rm` does: in the configs the daemon's environment names and in
    /// those its tracked agents' `[env]` put them, which it then forgets. A
    /// development build leaves them to the installed Shoal.
    async fn forget_trust(&self, repo: &Repository, workspace_paths: &[std::path::PathBuf]) {
        if !crate::build::changes_user_files() {
            return;
        }
        let name = crate::repository::name(repo);
        let environments = self.agent_environments(&repo.id).await;
        if let Err(error) = &environments {
            log!("contribution {name}: agent configs unknown: {error:#}");
        }
        let configured = environments.as_deref().unwrap_or_default();
        let home = &self.paths.home;
        let warnings =
            crate::cli::agents::forget_repository_trust(home, repo, workspace_paths, configured);
        for warning in warnings {
            log!("contribution {name}: {warning}");
        }
        if environments.is_ok()
            && let Err(error) = self.forget_agent_environments(&repo.id).await
        {
            log!("contribution {name}: agent configs kept: {error:#}");
        }
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

    async fn contribution_in_use(&self, id: &str) -> Result<bool> {
        let id = id.to_owned();
        self.store.run(move |db| in_use(db, &id)).await
    }

    /// Record the removal of a contribution nothing is left of, in the
    /// transaction that checks it, so new work is either seen here or refused
    /// afterwards; `false` when something is left.
    pub(super) async fn begin_contribution_removal(&self, repo: &Repository) -> Result<bool> {
        let id = repo.id.clone();
        let directory = real_directory_identity(&repo.path)?;
        self.store
            .run(move |db| {
                let tx = db.transaction()?;
                if in_use(&tx, &id)? {
                    return Ok(false);
                }
                tx.execute(
                    "INSERT INTO repository_removals(repository_id,directory_id,workspace_paths)
                     VALUES (?1,?2,'[]') ON CONFLICT(repository_id) DO NOTHING",
                    params![id, directory],
                )?;
                tx.commit()?;
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
    for remote in git::remotes(checkout, git::run_isolated).await?.lines() {
        git::fetch::remote(checkout, remote)
            .await
            .with_context(|| format!("could not fetch {remote}"))?;
    }
    Ok(())
}

/// The branches and tags every remote of the checkout advertises now.
async fn advertised_refs(checkout: &std::path::Path) -> Result<Vec<git::RemoteHead>> {
    let mut advertised = Vec::new();
    for remote in git::remotes(checkout, git::run_isolated).await?.lines() {
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
/// pending issue proposal, a kept branch, an unexpired checkpoint or removal
/// record, or a resource another repository's workspace borrows.
fn in_use(db: &Connection, id: &str) -> Result<bool> {
    Ok(recorded_use(db, id)? || crate::daemon::resources::borrowed(db, id)?)
}

fn recorded_use(db: &Connection, id: &str) -> Result<bool> {
    store::exists(
        db,
        "SELECT 1 FROM workspaces WHERE repository_id=?1
        UNION ALL SELECT 1 FROM agent_queue WHERE repository_id=?1
        UNION ALL SELECT 1 FROM chats WHERE repository_id=?1
        UNION ALL SELECT 1 FROM access_requests WHERE repository_id=?1 AND active=1
        UNION ALL SELECT 1 FROM kept_branches WHERE repository_id=?1
        UNION ALL SELECT 1 FROM checkpoints
            WHERE repository_id=?1 AND expires_at > unixepoch()
        UNION ALL SELECT 1 FROM removed_workspaces
            WHERE repository_id=?1 AND expires_at > unixepoch()",
        [id],
    )
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::{
        daemon::{notifications::NotificationKind, workspace::Manager},
        model::Repository,
        removal::{InspectionPolicy, UnsavedWork},
        test_support::{commit, git, manager, repository},
    };

    /// A clone registered in place and then marked as a contribution. Its
    /// path is canonical, as a clone's under `root_dir` is.
    async fn contribution(manager: &Arc<Manager>, root: &std::path::Path) -> Repository {
        let upstream = repository(root, "upstream");
        let clone = root.join("repo");
        git(
            root,
            &[
                "clone",
                "-q",
                upstream.to_str().unwrap(),
                clone.to_str().unwrap(),
            ],
        );
        let checkout = std::fs::canonicalize(clone).unwrap();
        let repo = manager
            .register_repository(checkout.to_str().unwrap().into(), None, None)
            .await
            .unwrap();
        let id = repo.id.clone();
        manager
            .store
            .run(move |db| {
                db.execute("UPDATE repositories SET contribution=1 WHERE id=?1", [id])?;
                Ok(())
            })
            .await
            .unwrap();
        manager.repository(repo.id.as_str()).await.unwrap()
    }

    async fn registered(manager: &Manager, repo: &Repository) -> bool {
        let repos = manager.repositories().await.unwrap();
        repos.iter().any(|other| other.id == repo.id)
    }

    #[tokio::test]
    async fn a_contribution_waits_out_the_grace_period() {
        let (root, manager) = manager().await;
        let repo = contribution(&manager, root.path()).await;
        manager
            .set_repository_config(&repo.id, Some("[cleanup]\ngrace_minutes = 10\n".into()))
            .await
            .unwrap();
        manager.sweep_contributions().await.unwrap();
        assert!(registered(&manager, &repo).await);
        let id = repo.id.clone();
        manager
            .store
            .run(move |db| {
                db.execute(
                    "UPDATE contribution_grace SET cleanup_at=0 WHERE repository_id=?1",
                    [id],
                )?;
                Ok(())
            })
            .await
            .unwrap();
        manager.sweep_contributions().await.unwrap();
        assert!(!registered(&manager, &repo).await);
    }

    #[tokio::test]
    async fn a_borrowed_contribution_stays_available() {
        let (root, manager) = manager().await;
        let repo = contribution(&manager, root.path()).await;
        let other = repository(root.path(), "other");
        let other = manager
            .register_repository(other.to_str().unwrap().into(), None, None)
            .await
            .unwrap();
        let borrower = manager
            .create_workspace(&other.id, "borrower".into(), None, None, None)
            .await
            .unwrap();
        let (id, path) = (repo.id.clone(), repo.path.display().to_string());
        manager
            .store
            .run(move |db| {
                db.execute_batch(
                    "INSERT INTO resource_pools VALUES ('global','pool','{}');",
                )?;
                db.execute(
                    "INSERT INTO resource_leases(id,workspace_id,scope,pool,name,resource,created_at)
                     VALUES ('lease',?1,'global','pool','default','one',0)",
                    [borrower.id],
                )?;
                db.execute(
                    "INSERT INTO repository_resource_leases(lease_id,repository_id,path)
                     VALUES ('lease',?1,?2)",
                    [id, path],
                )?;
                Ok(())
            })
            .await
            .unwrap();
        manager.sweep_contributions().await.unwrap();
        assert!(registered(&manager, &repo).await);
        // No removal began, so new work in it is still accepted.
        manager.ensure_repository_available(&repo.id).await.unwrap();
        manager
            .store
            .run(|db| {
                db.execute("DELETE FROM resource_leases WHERE id='lease'", [])?;
                Ok(())
            })
            .await
            .unwrap();
        manager.sweep_contributions().await.unwrap();
        assert!(!registered(&manager, &repo).await);
    }

    #[tokio::test]
    async fn new_work_is_refused_once_a_contribution_removal_begins() {
        let (root, manager) = manager().await;
        let repo = contribution(&manager, root.path()).await;
        assert!(manager.begin_contribution_removal(&repo).await.unwrap());
        let chat = manager
            .start_chat(repo.id.as_str().into(), "claude".into(), std::process::id())
            .await
            .unwrap_err();
        assert!(chat.to_string().contains("being removed"), "{chat:#}");
        let workspace = manager
            .create_workspace(&repo.id, "topic".into(), None, None, None)
            .await
            .unwrap_err();
        assert!(
            format!("{workspace:#}").contains("removal is incomplete"),
            "{workspace:#}"
        );
        // The interrupted removal finishes on the next sweep.
        manager.sweep_contributions().await.unwrap();
        assert!(!registered(&manager, &repo).await);
    }

    #[tokio::test]
    async fn a_contribution_goes_once_its_workspaces_and_checkpoints_are_gone() {
        let (root, manager) = manager().await;
        let repo = contribution(&manager, root.path()).await;
        let checkout = repo.path.clone();
        // Removal forgets the checkout's trust as `repo rm` does.
        let claude = manager.paths.home.join(".claude.json");
        let trusted = checkout.clone();
        let projects = serde_json::json!({"projects": {trusted.to_str().unwrap(): {"hasTrustDialogAccepted": true}}});
        std::fs::write(&claude, projects.to_string()).unwrap();
        let workspace = manager
            .create_workspace(&repo.id, "topic".into(), None, None, None)
            .await
            .unwrap();
        manager.sweep_contributions().await.unwrap();
        assert!(
            registered(&manager, &repo).await,
            "a workspace keeps the contribution"
        );

        manager
            .remove_workspace(&workspace.id, UnsavedWork::All, InspectionPolicy::GitOnly)
            .await
            .unwrap();
        manager.sweep_contributions().await.unwrap();
        assert!(
            registered(&manager, &repo).await,
            "a removal's checkpoint keeps it"
        );

        manager
            .store
            .run(|db| {
                db.execute_batch(
                    "UPDATE checkpoints SET expires_at=0; UPDATE removed_workspaces SET expires_at=0;",
                )?;
                Ok(())
            })
            .await
            .unwrap();
        manager.expire_checkpoints().await.unwrap();
        // Upstream moving on after the last workspace leaves nothing here unkept.
        let upstream = root.path().join("upstream");
        std::fs::write(upstream.join("later"), "later\n").unwrap();
        commit(&upstream, "later");
        // Work left in the checkout, such as a chat's, keeps it too.
        let left = checkout.join("notes");
        std::fs::write(&left, "unsaved\n").unwrap();
        manager.sweep_contributions().await.unwrap();
        assert!(registered(&manager, &repo).await, "checkout work keeps it");
        std::fs::remove_file(&left).unwrap();
        manager.sweep_contributions().await.unwrap();
        assert!(!registered(&manager, &repo).await);
        assert!(!checkout.exists());
        // Debug builds, such as this test's, leave user files alone unless told.
        let config = std::fs::read_to_string(&claude).unwrap();
        let kept = config.contains(trusted.to_str().unwrap());
        assert_eq!(kept, !crate::build::changes_user_files(), "{config}");
        let notifications = manager.notifications(false, 10).await.unwrap();
        assert!(
            notifications
                .iter()
                .any(|notification| notification.kind == NotificationKind::RepositoryRemoved)
        );
    }
}
