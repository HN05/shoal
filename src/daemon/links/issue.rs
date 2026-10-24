//! Persist issue associations.
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::{
    daemon::{
        events::{self, EventKind},
        workspace::Manager,
    },
    forge::{ForgeRepo, items::link::ItemKind},
    model::Workspace,
    state::WorkspaceState,
};

#[derive(Debug, Serialize, Deserialize)]
pub struct Registration {
    pub url: String,
    pub error: Option<String>,
}

impl Manager {
    pub async fn issue_registration(&self, id: &str) -> Result<Option<Registration>> {
        let id = id.to_owned();
        self.store
            .run(move |db| {
                Ok(db
                    .query_row(
                        "SELECT url,error FROM workspace_issue WHERE workspace_id=?1",
                        [id],
                        |row| {
                            Ok(Registration {
                                url: row.get(0)?,
                                error: row.get(1)?,
                            })
                        },
                    )
                    .optional()?)
            })
            .await
    }

    /// Association is part of opening a workspace, before tracked setup starts.
    /// Linking the same issue again records a newly known title.
    pub async fn set_issue(
        &self,
        selector: &str,
        input: &str,
        title: Option<String>,
    ) -> Result<()> {
        let _guard = self.pr_gate.lock().await;
        self.set_issue_gated(selector, input, title).await
    }

    /// [`Self::set_issue`] for a caller already holding `pr_gate`.
    pub(crate) async fn set_issue_gated(
        &self,
        selector: &str,
        input: &str,
        title: Option<String>,
    ) -> Result<()> {
        let workspace = self.workspace(selector).await?;
        self.verify_worktree(&workspace).await?;
        let (_, _, url) = self.issue_forge(&workspace, input).await?;
        self.store
            .run(move |db| {
                let tx = db.transaction()?;
                let state: WorkspaceState = tx.query_row(
                    "SELECT state FROM workspaces WHERE id=?1",
                    [&workspace.id],
                    |row| row.get(0),
                )?;
                ensure!(
                    state.accepts_links(),
                    "workspace cannot accept an issue in its current state"
                );
                let existing: Option<String> = tx
                    .query_row(
                        "SELECT url FROM workspace_issue WHERE workspace_id=?1",
                        [&workspace.id],
                        |row| row.get(0),
                    )
                    .optional()?;
                ensure!(
                    existing.as_ref().is_none_or(|existing| existing == &url),
                    "workspace is already associated with a different issue"
                );
                let inserted = existing.is_none();
                tx.execute(
                    "INSERT INTO workspace_issue(workspace_id,url,title) VALUES (?1,?2,?3)
                 ON CONFLICT(workspace_id) DO UPDATE SET title=COALESCE(excluded.title,title)",
                    rusqlite::params![workspace.id, url, title],
                )?;
                if inserted {
                    events::record_link(
                        &tx,
                        &workspace.id,
                        EventKind::Linked,
                        crate::model::Relation::Assigned,
                        ItemKind::Issue,
                        &url,
                    )?;
                }
                tx.commit()?;
                Ok(())
            })
            .await
    }

    pub async fn clear_issue(&self, selector: &str, input: Option<&str>) -> Result<()> {
        let _guard = self.pr_gate.lock().await;
        let workspace = self.workspace(selector).await?;
        let selected = match input {
            Some(input) => Some(self.issue_forge(&workspace, input).await?.2),
            None => None,
        };
        self.store
            .run(move |db| {
                let tx = db.transaction()?;
                unlink(&tx, &workspace.id, selected.as_deref())?;
                tx.commit()?;
                Ok(())
            })
            .await?;
        self.cleanup_notify.notify_one();
        Ok(())
    }

    pub(super) async fn issue_forge(
        &self,
        workspace: &Workspace,
        input: &str,
    ) -> Result<(ForgeRepo, u64, String)> {
        let remote = crate::repository::remote_url_from_path(&workspace.path)
            .await?
            .context("issue lookup needs an origin remote")?;
        let forge = self.forge(&remote).await?;
        let (number, url) = forge.issue(input)?;
        Ok((forge, number, url))
    }
}

/// Remove the workspace's issue, which must be `selected` when one is named,
/// along with what its link kept.
fn unlink(db: &Connection, id: &str, selected: Option<&str>) -> Result<()> {
    super::ensure_accepts_links(db, id)?;
    let existing: Option<String> = db
        .query_row(
            "SELECT url FROM workspace_issue WHERE workspace_id=?1",
            [id],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(url) = selected {
        ensure!(
            existing.as_deref() == Some(url),
            "issue is not linked: {url}"
        );
    }
    if let Some(url) = existing {
        super::forget_assigned(db, id, ItemKind::Issue, &[url])?;
    }
    db.execute("DELETE FROM workspace_issue WHERE workspace_id=?1", [id])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        daemon::events::{EventItem, EventKind},
        test_support::{git, manager, repository},
    };

    #[tokio::test]
    async fn association_is_persistent_idempotent_and_bound_to_the_origin() {
        let (root, manager) = manager().await;
        let path = repository(root.path(), "repo");
        git(&path, &["config", "protocol.allow", "never"]);
        let repo = manager
            .register_repository(path.to_str().unwrap().into(), None, None)
            .await
            .unwrap();
        let workspace = manager
            .create_workspace(&repo.id, "issue".into(), Some("HEAD".into()), None, None)
            .await
            .unwrap();
        git(
            &path,
            &["remote", "add", "origin", "https://github.com/team/repo"],
        );
        let url = "https://github.com/team/repo/issues/316";
        manager.set_issue(&workspace.id, url, None).await.unwrap();
        manager
            .set_issue(&workspace.id, "316", Some("Show titles".into()))
            .await
            .unwrap();
        manager.set_issue(&workspace.id, "316", None).await.unwrap();
        let links = manager.workspace(&workspace.id).await.unwrap().links;
        assert_eq!(links.issue.as_deref(), Some(url));
        assert_eq!(links.issue_title.as_deref(), Some("Show titles"));
        let linked: Vec<_> = manager
            .workspace_events(None, 100)
            .await
            .unwrap()
            .into_iter()
            .filter_map(|item| match item {
                EventItem::Event(event) if event.details.kind == EventKind::Linked => {
                    event.details.link
                }
                _ => None,
            })
            .collect();
        assert_eq!(linked.len(), 1);
        assert_eq!(linked[0].kind, crate::forge::items::link::ItemKind::Issue);
        assert_eq!(linked[0].url, url);
        assert!(manager.set_issue(&workspace.id, "317", None).await.is_err());
        assert!(
            manager
                .set_issue(
                    &workspace.id,
                    "https://github.com/other/repo/issues/316",
                    None
                )
                .await
                .is_err()
        );
        assert!(
            manager
                .cleanup_snapshot(&workspace.id)
                .await
                .unwrap()
                .is_none()
        );
        let reopened = Manager::open(manager.paths.clone()).await.unwrap();
        assert_eq!(
            reopened
                .inspect_workspace(&workspace.id)
                .await
                .unwrap()
                .issue
                .unwrap()
                .url,
            url
        );
        assert!(
            reopened
                .clear_issue(&workspace.id, Some("317"))
                .await
                .is_err()
        );
        assert!(
            reopened
                .issue_registration(&workspace.id)
                .await
                .unwrap()
                .is_some()
        );
        reopened
            .clear_issue(&workspace.id, Some("316"))
            .await
            .unwrap();
        assert!(reopened.workspace_events(None, 100).await.unwrap().iter().any(
            |item| matches!(item, EventItem::Event(event) if event.details.kind == EventKind::Unlinked
                && event.details.link.as_ref().is_some_and(|link| link.url == url))
        ));
        assert!(
            reopened
                .issue_registration(&workspace.id)
                .await
                .unwrap()
                .is_none()
        );
        reopened
            .set_issue(&workspace.id, "317", None)
            .await
            .unwrap();
        assert!(
            reopened
                .workspace(&workspace.id)
                .await
                .unwrap()
                .links
                .issue_title
                .is_none()
        );
        assert!(
            reopened
                .issue_registration(&workspace.id)
                .await
                .unwrap()
                .unwrap()
                .url
                .ends_with("/317")
        );
        reopened.store.shutdown().await;
    }
}
