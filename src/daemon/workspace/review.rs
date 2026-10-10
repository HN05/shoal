//! Ready-for-review marks: an agent's statement that linked work is ready at a
//! commit. Integrations read them from the event journal; they neither notify
//! the user nor affect completion or cleanup.
use anyhow::{Result, ensure};
use rusqlite::{Connection, params};

use super::{GuardMode, Manager};
use crate::{
    daemon::{notifications::NotificationKind, store},
    forge::{
        link::{ItemKind, Selection},
        pr::{current_head, wait::linked_items},
    },
    hooks::{self, Hook, HookKind},
    model::{ReviewMark, Workspace},
};

pub(crate) fn list(db: &Connection, id: &str) -> Result<Vec<ReviewMark>> {
    Ok(db
        .prepare(
            "SELECT url,kind,head,created_at FROM workspace_review
             WHERE workspace_id=?1 ORDER BY created_at,url",
        )?
        .query_map([id], |row| {
            let url: String = row.get(0)?;
            Ok(ReviewMark {
                kind: row.get(1)?,
                url: (!url.is_empty()).then_some(url),
                head: row.get(2)?,
                created_at: row.get(3)?,
                stale: None,
            })
        })?
        .collect::<rusqlite::Result<_>>()?)
}

/// Unlinking an item withdraws its mark in the same transaction.
pub(crate) fn forget(db: &Connection, id: &str, urls: &[String]) -> Result<()> {
    db.execute(
        "DELETE FROM workspace_review WHERE workspace_id=?1
         AND url IN (SELECT value FROM json_each(?2))",
        params![id, serde_json::to_string(urls)?],
    )?;
    Ok(())
}

impl Manager {
    /// Mark the selected linked items ready at HEAD, or the workspace itself
    /// when nothing is linked and nothing was selected. Marking again moves
    /// the mark to the current HEAD and reruns `post_ready_cmd`.
    pub async fn mark_ready(
        &self,
        selector: &str,
        selection: Selection,
    ) -> Result<Vec<ReviewMark>> {
        let _guard = self.pr_gate.lock().await;
        let workspace = self.workspace(selector).await?;
        let command = HookKind::PostReady
            .command(&self.workspace_settings(&workspace).await?)
            .map(|path| workspace.path.join(path));
        // Like post_done_cmd, the hook excludes lifecycle and permit changes.
        let _resources = match command {
            Some(_) => Some(
                self.resource_guard(&workspace.id, GuardMode::Exclusive)
                    .await?,
            ),
            None => None,
        };
        self.verify_worktree(&workspace).await?;
        let head = current_head(&workspace).await?;
        let explicit = self.explicit_item(&workspace, &selection).await?;
        let marks = self
            .record_marks(&workspace.id, selection, explicit, head)
            .await?;
        if let Some(command) = command {
            let configured_env = self.configured_workspace_environment(&workspace).await?;
            if let Err(error) = hooks::run_detached(
                Hook::PostReady(&marks),
                &workspace,
                &command,
                &self.paths,
                &configured_env,
            )
            .await
            {
                self.notify(
                    Some(&workspace.name),
                    NotificationKind::HookFailed,
                    format!("marked ready for review; {error:#}"),
                )
                .await;
            }
        }
        Ok(marks)
    }

    async fn record_marks(
        &self,
        id: &str,
        selection: Selection,
        explicit: Option<(String, ItemKind)>,
        head: String,
    ) -> Result<Vec<ReviewMark>> {
        let id = id.to_owned();
        let created_at = i64::try_from(crate::time::unix_seconds())?;
        self.store
            .run(move |db| {
                let tx = db.transaction()?;
                store::require_ready(&tx, &id)?;
                let linked = linked_items(&tx, &id, selection.kind)?;
                let items: Vec<(Option<String>, Option<ItemKind>)> = match explicit {
                    Some((url, kind)) => {
                        ensure!(
                            linked.iter().any(|(linked, _)| linked == &url),
                            "{kind} is not linked: {url}"
                        );
                        vec![(Some(url), Some(kind))]
                    }
                    None if !linked.is_empty() => linked
                        .into_iter()
                        .map(|(url, kind)| (Some(url), Some(kind)))
                        .collect(),
                    None => {
                        ensure!(
                            selection.kind.is_none(),
                            "no linked items of the selected kind; register one with shoal link"
                        );
                        vec![(None, None)]
                    }
                };
                // Linked-item marks replace a mark made while nothing was linked.
                if items.iter().any(|(url, _)| url.is_some()) {
                    tx.execute(
                        "DELETE FROM workspace_review WHERE workspace_id=?1 AND url=''",
                        [&id],
                    )?;
                }
                let mut marks = Vec::new();
                for (url, kind) in items {
                    tx.execute(
                        "INSERT INTO workspace_review(workspace_id,url,kind,head,created_at)
                         VALUES (?1,?2,?3,?4,?5)
                         ON CONFLICT(workspace_id,url) DO UPDATE
                         SET head=excluded.head,created_at=excluded.created_at",
                        params![id, url.as_deref().unwrap_or(""), kind, head, created_at],
                    )?;
                    marks.push(ReviewMark {
                        kind,
                        url,
                        head: head.clone(),
                        created_at,
                        stale: Some(false),
                    });
                }
                tx.commit()?;
                Ok(marks)
            })
            .await
    }

    /// Withdraw the selected marks, or every mark without a selection.
    pub async fn clear_ready(
        &self,
        selector: &str,
        selection: Selection,
    ) -> Result<Vec<ReviewMark>> {
        let _guard = self.pr_gate.lock().await;
        let workspace = self.workspace(selector).await?;
        let explicit = self.explicit_item(&workspace, &selection).await?;
        let id = workspace.id;
        self.store
            .run(move |db| {
                let tx = db.transaction()?;
                let mut cleared = list(&tx, &id)?;
                cleared.retain(|mark| match (&explicit, selection.kind) {
                    (Some((url, _)), _) => mark.url.as_ref() == Some(url),
                    (None, Some(kind)) => mark.kind == Some(kind),
                    (None, None) => true,
                });
                if let Some((url, kind)) = &explicit {
                    ensure!(!cleared.is_empty(), "{kind} is not marked ready: {url}");
                }
                for mark in &cleared {
                    tx.execute(
                        "DELETE FROM workspace_review WHERE workspace_id=?1 AND url=?2",
                        params![id, mark.url.as_deref().unwrap_or("")],
                    )?;
                }
                tx.commit()?;
                Ok(cleared)
            })
            .await
    }

    /// Say whether each mark still describes HEAD. HEAD that cannot be read
    /// leaves staleness unknown rather than failing a listing.
    pub async fn annotate_review(&self, workspace: &mut Workspace) {
        if workspace.review.is_empty() {
            return;
        }
        let head = current_head(workspace).await.ok();
        for mark in &mut workspace.review {
            mark.stale = head.as_ref().map(|head| head != &mark.head);
        }
    }
}

#[cfg(test)]
mod tests;
