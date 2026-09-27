//! Assignment completion is a signal; the cleanup sweep owns removal.
use anyhow::Result;
use rusqlite::OptionalExtension;

use super::Manager;
use crate::{
    daemon::{notifications::NotificationKind, store},
    model::Completion,
};

impl Manager {
    pub async fn completion(&self, id: &str) -> Result<Option<Completion>> {
        let id = id.to_owned();
        self.store
            .run(move |db| {
                let record: Option<String> = db
                    .query_row(
                        "SELECT record FROM workspace_completion WHERE workspace_id=?1",
                        [id],
                        |row| row.get(0),
                    )
                    .optional()?;
                record
                    .map(|record| serde_json::from_str(&record).map_err(Into::into))
                    .transpose()
            })
            .await
    }

    pub async fn mark_done(&self, selector: &str, cleanup: Option<bool>) -> Result<Completion> {
        // Serialize keep/cleanup choices with completion and PR sweeps.
        let _guard = self.pr_gate.lock().await;
        let workspace = self.workspace(selector).await?;
        self.verify_worktree(&workspace).await?;
        let cleanup = match cleanup {
            Some(cleanup) => cleanup,
            None => self.workspace_settings(&workspace).await?.done.cleanup,
        };
        let completion = Completion {
            head: crate::forge::pr::current_head(&workspace).await?,
            cleanup,
            error: None,
        };
        let id = workspace.id;
        let record = serde_json::to_string(&completion)?;
        self.store
            .run(move |db| {
                let tx = db.transaction()?;
                store::require_ready(&tx, &id)?;
                tx.execute(
                    "INSERT INTO workspace_completion(workspace_id,record) VALUES (?1,?2)
                 ON CONFLICT(workspace_id) DO UPDATE SET record=excluded.record",
                    rusqlite::params![id, record],
                )?;
                tx.commit()?;
                Ok(())
            })
            .await?;
        self.notify(
            Some(&workspace.name),
            NotificationKind::WorkspaceDone,
            if cleanup {
                "assignment finished; cleanup requested"
            } else {
                "assignment finished; workspace kept for review"
            },
        )
        .await;
        self.cleanup_notify.notify_one();
        Ok(completion)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{manager, repository};

    #[tokio::test]
    async fn completion_persists_and_explicit_choices_override_the_repository_default() {
        let (root, manager) = manager().await;
        let path = repository(root.path(), "repo");
        let repo = manager
            .register_repository(path.to_str().unwrap().into(), None, None)
            .await
            .unwrap();
        let workspace = manager
            .create_workspace(&repo.id, "finished".into(), None, None, None)
            .await
            .unwrap();
        assert!(
            manager
                .mark_done(&workspace.id, None)
                .await
                .unwrap()
                .cleanup
        );
        manager
            .set_repository_config(&repo.id, Some("[done]\ncleanup=false\n".into()))
            .await
            .unwrap();
        assert!(
            !manager
                .mark_done(&workspace.id, None)
                .await
                .unwrap()
                .cleanup
        );
        assert!(
            manager
                .mark_done(&workspace.id, Some(true))
                .await
                .unwrap()
                .cleanup
        );
        manager
            .set_repository_config(&repo.id, Some("[done]\ncleanup=true\n".into()))
            .await
            .unwrap();
        let kept = manager.mark_done(&workspace.id, Some(false)).await.unwrap();
        assert!(!kept.cleanup);
        let reopened = Manager::open(manager.paths.clone()).await.unwrap();
        let completion = reopened
            .inspect_workspace(&workspace.id)
            .await
            .unwrap()
            .completion
            .unwrap();
        assert!(!completion.cleanup);
        assert_eq!(completion.head, kept.head);
        assert!(
            reopened
                .notifications(true, 100)
                .await
                .unwrap()
                .iter()
                .any(|event| event.kind == NotificationKind::WorkspaceDone)
        );
        reopened.store.shutdown().await;
    }
}
