//! Emptying a repository's shared tool caches, which only happens while
//! nothing Shoal started can be using them.
use anyhow::Result;
use rusqlite::TransactionBehavior;
use serde::{Deserialize, Serialize};

use crate::{caches::Layout, daemon::workspace::Manager, model::Repository};

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
        let caches = layout.list()?;
        if caches.is_empty() {
            return Ok(None);
        }
        let Some(_seed) = self.pause_idle_seed(&repo.id) else {
            return Ok(Some(Emptying::Busy { caches }));
        };
        let repository_id = repo.id.clone();
        let moving = layout.clone();
        // Registering an execution writes its row before preparing the caches,
        // so holding the write lock while moving them keeps new ones out.
        let moved = self
            .store
            .run(move |db| {
                let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
                let running: bool = tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM executions e JOIN workspaces w ON w.id=e.workspace_id WHERE w.repository_id=?1)",
                    [&repository_id],
                    |row| row.get(0),
                )?;
                if running {
                    return Ok(None);
                }
                if dry_run {
                    return Ok(Some(caches));
                }
                Ok(Some(moving.move_aside()?))
            })
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
    }
}
