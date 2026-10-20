//! Emptying a repository's shared tool caches, which only happens while
//! nothing Shoal started can be using them.
use anyhow::Result;
use rusqlite::{Connection, TransactionBehavior};
use serde::{Deserialize, Serialize};

use crate::{
    caches::Layout,
    daemon::{store, workspace::Manager},
    model::Repository,
};

/// What happened to one repository's shared caches.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum Emptying {
    /// The caches emptied, or that a dry run would empty.
    Emptied {
        caches: Vec<String>,
    },
    /// A tracked execution or the seed build of the repository is running.
    Busy {
        caches: Vec<String>,
    },
    Failed {
        error: String,
    },
}

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
mod tests {
    use super::*;
    use crate::{
        config::repo::{Cache, CacheScope},
        daemon::workspace::ExecutionKind,
        test_support::{manager, repository},
    };
    use std::collections::BTreeMap;

    #[tokio::test]
    async fn caches_empty_only_while_nothing_uses_them() {
        let (root, manager) = manager().await;
        let checkout = repository(root.path(), "repo");
        let repo = manager
            .register_repository(checkout.to_str().unwrap().into(), None, None)
            .await
            .unwrap();
        let workspace = manager
            .create_workspace(&repo.id, "agent".into(), None, None, None)
            .await
            .unwrap();
        let repo = manager.repository(&repo.id).await.unwrap();
        assert_eq!(manager.empty_caches(&repo, false).await.unwrap(), None);

        let configured = BTreeMap::from([(
            "go".to_owned(),
            Cache {
                scope: CacheScope::Shared,
            },
        )]);
        let go = crate::caches::directories(&repo, &configured).unwrap()["go"].clone();
        std::fs::write(go.join("entry"), "cached").unwrap();
        let emptied = Emptying::Emptied {
            caches: vec!["go".into()],
        };
        let busy = Emptying::Busy {
            caches: vec!["go".into()],
        };

        let execution = manager
            .begin_execution(&workspace.id, None, ExecutionKind::Command, None)
            .await
            .unwrap();
        assert_eq!(
            manager.empty_caches(&repo, false).await.unwrap(),
            Some(busy.clone())
        );
        manager
            .finish_execution(execution.plan.id, ExecutionKind::Command, Some(0))
            .await
            .unwrap();

        let seed = manager.pause_idle_seed(&repo.id).unwrap();
        assert_eq!(
            manager.empty_caches(&repo, false).await.unwrap(),
            Some(busy)
        );
        drop(seed);

        assert_eq!(
            manager.empty_caches(&repo, true).await.unwrap(),
            Some(emptied.clone())
        );
        assert!(go.join("entry").exists());
        assert_eq!(
            manager.empty_caches(&repo, false).await.unwrap(),
            Some(emptied)
        );
        assert!(!go.exists());
        assert_eq!(manager.empty_caches(&repo, false).await.unwrap(), None);

        // A pass interrupted before purging leaves nothing behind for long.
        let layout = Layout::of(&repo).unwrap();
        crate::caches::directories(&repo, &configured).unwrap();
        assert_eq!(layout.move_aside().unwrap(), ["go"]);
        let trash = layout.root().join("trash");
        assert!(trash.is_dir());
        assert_eq!(manager.empty_caches(&repo, false).await.unwrap(), None);
        assert!(!trash.exists());
    }

    #[tokio::test]
    async fn executions_do_not_start_without_their_caches() {
        let (root, manager) = manager().await;
        let checkout = repository(root.path(), "repo");
        let repo = manager
            .register_repository(checkout.to_str().unwrap().into(), None, None)
            .await
            .unwrap();
        let workspace = manager
            .create_workspace(&repo.id, "agent".into(), None, None, None)
            .await
            .unwrap();
        let repo = manager.repository(&repo.id).await.unwrap();
        std::fs::write(
            workspace.path.join(".shoal.toml"),
            "[caches.go]\nscope = 'shared'\n",
        )
        .unwrap();
        let caches = Layout::of(&repo).unwrap().root().to_owned();
        std::fs::create_dir_all(&caches).unwrap();
        std::fs::write(caches.join("owner"), "someone else\n").unwrap();
        let Err(error) = manager
            .begin_execution(&workspace.id, None, ExecutionKind::Command, None)
            .await
        else {
            panic!("an unmarked cache directory refuses the execution");
        };
        assert!(
            format!("{error:#}").contains("is not a cache directory Shoal created"),
            "{error:#}"
        );
        // No execution stayed recorded: once the directory is the
        // repository's, the caches are not busy.
        std::fs::write(caches.join("owner"), format!("{}\n", repo.id)).unwrap();
        let configured = BTreeMap::from([(
            "go".to_owned(),
            Cache {
                scope: CacheScope::Shared,
            },
        )]);
        crate::caches::directories(&repo, &configured).unwrap();
        assert_eq!(
            manager.empty_caches(&repo, false).await.unwrap(),
            Some(Emptying::Emptied {
                caches: vec!["go".into()]
            })
        );
    }

    #[tokio::test]
    async fn hooks_the_cli_runs_find_their_caches_prepared() {
        let (root, manager) = manager().await;
        let checkout = repository(root.path(), "repo");
        let repo = manager
            .register_repository(checkout.to_str().unwrap().into(), None, None)
            .await
            .unwrap();
        let workspace = manager
            .create_workspace(&repo.id, "agent".into(), None, None, None)
            .await
            .unwrap();
        let repo = manager.repository(&repo.id).await.unwrap();
        std::fs::write(
            workspace.path.join(".shoal.toml"),
            "post_setup_cmd = 'attach.sh'\n[caches.go]\nscope = 'shared'\n",
        )
        .unwrap();
        manager
            .workspace_hook(&workspace, crate::hooks::HookKind::PostSetup)
            .await
            .unwrap()
            .unwrap();
        let configured = BTreeMap::from([(
            "go".to_owned(),
            Cache {
                scope: CacheScope::Shared,
            },
        )]);
        let go = Layout::of(&repo).unwrap().locate(&configured)["go"].clone();
        assert!(go.is_dir());
        assert_eq!(
            crate::caches::directories(&repo, &configured).unwrap()["go"],
            go
        );
    }
}
