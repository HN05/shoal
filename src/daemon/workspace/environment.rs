//! Persistent scope for processes whose lifetime belongs to an external caller.
use super::Manager;
use crate::{
    daemon::{scope::Caller, store},
    env,
};
use anyhow::{Context, Result, ensure};
use rusqlite::{OptionalExtension, params};
use std::collections::BTreeMap;
use uuid::Uuid;

impl Manager {
    pub(crate) async fn caller(&self, token: &str) -> Result<Option<Caller>> {
        if let Some(caller) = self.scopes.lock().await.get(token).cloned() {
            return Ok(Some(caller));
        }
        let token = token.to_owned();
        self.store
            .run(move |db| {
                Ok(db
                    .query_row(
                        "SELECT workspace_id FROM workspace_scopes WHERE token=?1",
                        [token],
                        |row| {
                            Ok(Caller {
                                workspace_id: row.get(0)?,
                                execution: None,
                            })
                        },
                    )
                    .optional()?)
            })
            .await
    }

    pub(crate) async fn workspace_environment(
        &self,
        selector: &str,
    ) -> Result<BTreeMap<String, String>> {
        let workspace = self.workspace(selector).await?;
        let _guard = self.lock_repository_git(&workspace.repository_id).await;
        let workspace = self.workspace(&workspace.id).await?;
        self.verify_worktree(&workspace).await?;
        let workspace_id = workspace.id.clone();
        let token = Uuid::new_v4().to_string();
        let paths = self.paths.clone();
        self.store
            .run(move |db| {
                let tx = db.transaction()?;
                store::require_ready(&tx, &workspace_id)?;
                let ports = store::ports(&tx, Some(&workspace_id))?;
                let values = env::workspace_environment(&workspace, &paths, &ports, &token)
                    .into_iter()
                    .map(|(name, value)| {
                        let value = value.into_string().ok().with_context(|| {
                            format!("{name} cannot be exported as JSON: invalid UTF-8")
                        })?;
                        Ok((name, value))
                    })
                    .collect::<Result<BTreeMap<_, _>>>()?;
                tx.execute(
                    "INSERT INTO workspace_scopes(token,workspace_id) VALUES (?1,?2)",
                    params![token, workspace_id],
                )?;
                tx.commit()?;
                Ok(values)
            })
            .await
    }

    pub(crate) async fn revoke_workspace_environment(
        &self,
        selector: &str,
        token: String,
    ) -> Result<()> {
        let workspace_id = self.workspace(selector).await?.id;
        self.store
            .run(move |db| {
                let deleted = db.execute(
                    "DELETE FROM workspace_scopes WHERE workspace_id=?1 AND token=?2",
                    params![workspace_id, token],
                )?;
                ensure!(deleted == 1, "unknown workspace environment token");
                Ok(())
            })
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{daemon::scope, protocol::Method, state::WorkspaceState, test_support};

    #[tokio::test]
    async fn exported_scopes_are_persistent_independent_and_untracked() {
        let (root, manager) = test_support::manager().await;
        let repo = test_support::repository(root.path(), "repo");
        let repo = manager
            .register_repository(repo.to_str().unwrap().into(), None, None)
            .await
            .unwrap();
        let workspace = manager
            .create_workspace(&repo.id, "external".into(), None, None, None)
            .await
            .unwrap();
        let first = manager
            .workspace_environment(&workspace.name)
            .await
            .unwrap();
        let second = manager.workspace_environment(&workspace.id).await.unwrap();
        let token = &first[env::SCOPE_TOKEN];
        assert_ne!(token, &second[env::SCOPE_TOKEN]);
        assert!(
            manager
                .inspect_workspace(&workspace.id)
                .await
                .unwrap()
                .executions
                .is_empty()
        );
        assert!(!first.contains_key(env::EXECUTION_ID));
        let caller = manager.caller(token).await.unwrap().unwrap();
        assert_eq!(caller.workspace_id, workspace.id);
        assert!(caller.execution.is_none());
        for mut method in [
            Method::WorkspaceEnv {
                workspace: workspace.id.clone(),
            },
            Method::RevokeWorkspaceEnv {
                workspace: workspace.id.clone(),
                token: token.clone(),
            },
            Method::CheckLanding,
            Method::ListNotifications {
                unread_only: true,
                limit: 50,
            },
        ] {
            assert!(
                scope::authorize(&manager, Some(token), &mut method)
                    .await
                    .is_err()
            );
        }
        manager.store.shutdown().await;
        let restored = Manager::open(manager.paths.clone()).await.unwrap();
        assert!(restored.caller(token).await.unwrap().is_some());
        restored
            .revoke_workspace_environment(&workspace.id, token.clone())
            .await
            .unwrap();
        assert!(restored.caller(token).await.unwrap().is_none());
        assert!(
            restored
                .caller(&second[env::SCOPE_TOKEN])
                .await
                .unwrap()
                .is_some()
        );
        let id = workspace.id.clone();
        restored
            .store
            .run(move |db| {
                db.execute("DELETE FROM workspaces WHERE id=?1", [id])?;
                Ok(())
            })
            .await
            .unwrap();
        assert!(
            restored
                .caller(&second[env::SCOPE_TOKEN])
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn exporting_requires_ready_verified_worktree_without_minting_on_failure() {
        let (root, manager) = test_support::manager().await;
        let repo = test_support::repository(root.path(), "repo");
        let repo = manager
            .register_repository(repo.to_str().unwrap().into(), None, None)
            .await
            .unwrap();
        let workspace = manager
            .create_workspace(&repo.id, "external".into(), None, None, None)
            .await
            .unwrap();
        manager
            .set_state(&workspace.id, WorkspaceState::Failed, None)
            .await
            .unwrap();
        assert!(manager.workspace_environment(&workspace.id).await.is_err());
        manager
            .set_state(&workspace.id, WorkspaceState::Ready, None)
            .await
            .unwrap();
        std::fs::rename(
            workspace.path.join(".git"),
            workspace.path.join(".git.saved"),
        )
        .unwrap();
        assert!(manager.workspace_environment(&workspace.id).await.is_err());
        assert_eq!(
            manager
                .store
                .run(|db| Ok(db
                    .query_row("SELECT count(*) FROM workspace_scopes", [], |r| r
                        .get::<_, i64>(0))?))
                .await
                .unwrap(),
            0
        );
    }
}
