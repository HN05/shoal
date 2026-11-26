//! Ready-for-review marks. See `docs/design/progress.md`.
use crate::Spaces;
use crate::config::hooks::HookCommand;
use crate::daemon::Daemon;
use crate::forge::items::reference::ItemRef;
use crate::model::Space;
use shoal_ids::space_ids::SpaceId;
use std::path::Path;

use anyhow::{Context, Result, bail, ensure};
use rusqlite::{Connection, params};

use super::gates::GuardMode;
use crate::daemon::ReviewGate;
use crate::{
    config::Effective,
    forge::items::link::{ItemKind, LinkKind, Selection},
    hooks::{self, Hook, HookKind},
    model::{ReadyMarks, ReviewMark, SkippedPr},
    {notifications::AlertKind, scope::Caller, store::require_ready_space},
};
use pulls::Pulls;
use shoal_protocol::space_names::SpaceSelector;
use shoal_refs::CommitId;

mod pulls;

pub fn list(db: &Connection, id: &SpaceId) -> Result<Vec<ReviewMark>> {
    Ok(db
        .prepare_cached(
            "SELECT url,kind,created_at FROM space_review
             WHERE space_id=?1 ORDER BY created_at,url",
        )?
        .query_map([id], |row| {
            let url: String = row.get(0)?;
            let kind: Option<ItemKind> = row.get(1)?;
            let url = (!url.is_empty())
                .then(|| match kind {
                    Some(kind) => ItemRef::parse_kind(kind.into(), &url),
                    None => url.parse::<ItemRef>(),
                })
                .transpose()
                .map_err(|error| {
                    rusqlite::Error::FromSqlConversionFailure(
                        0,
                        rusqlite::types::Type::Text,
                        error.into(),
                    )
                })?;
            Ok(ReviewMark {
                kind,
                url,
                created_at: row.get(2)?,
                warnings: Vec::new(),
            })
        })?
        .collect::<rusqlite::Result<_>>()?)
}

/// Unlinking an item withdraws its mark in the same transaction.
pub fn forget(db: &Connection, id: &SpaceId, urls: &[String]) -> Result<()> {
    db.execute(
        "DELETE FROM space_review WHERE space_id=?1
         AND url IN (SELECT value FROM json_each(?2))",
        params![id, serde_json::to_string(urls)?],
    )?;
    Ok(())
}

impl Spaces {
    /// Marks the space itself when nothing is linked and nothing was
    /// selected, and tells its coordinator. A workspace's linked PR is
    /// marked only while it is open with HEAD as its head on the forge, a
    /// chat's while it is open. Others are skipped, and an explicitly
    /// selected one fails. With `[pr] undraft_on_ready`, marked draft PRs
    /// leave draft before `hooks.post_ready` runs; a failure there keeps the
    /// marks and runs the hook. `[review] require_for_ready` applies to a
    /// workspace's HEAD where it holds `caller`.
    pub async fn mark_ready(
        &self,
        daemon: &dyn Daemon,
        selector: &SpaceSelector,
        selection: Selection,
        caller: Option<&Caller>,
    ) -> Result<ReadyMarks> {
        let id = self.space(selector).await?.id.to_owned();
        let _guard = daemon.lock_change_gates(&id, &[]).await?;
        let space = self.space(&SpaceSelector::id(&id)).await?;
        let head = self.ready_head(daemon, &space, caller).await?;
        let settings = daemon.acting_settings(&space).await?;
        let origin = daemon.origin_path(&space).await?;
        let command = HookKind::PostReady
            .command(&settings)
            .map(|path| origin.join(path));
        // Like hooks.post_done, the hook excludes lifecycle and permit
        // changes.
        let _resources = match command {
            Some(_) => Some(self.gates.resource_guard(&id, GuardMode::Exclusive)?),
            None => None,
        };
        let kind = marked_kind(&selection)?;
        let explicit = marked_item(daemon.explicit_item(&origin, &selection).await?);
        let head = head.as_ref().map(CommitId::as_str);
        let pulls = (self.markable_pulls(daemon, &space, kind, explicit.as_ref(), head)).await?;
        let mut ready = (self.record_marks(&id, kind, explicit, pulls)).await?;
        let undrafted = match settings.pr.undraft_on_ready {
            true => {
                daemon
                    .set_draft(&space, &pr_urls(&ready.marks), false)
                    .await
            }
            false => Ok(()),
        };
        if let Some(command) = command {
            (self.run_post_ready(daemon, &space, &settings, &command, &ready.marks)).await;
        }
        let about = crate::alerts::About::space(&space.name, &id);
        let kind = crate::alerts::AlertKind::ReadyMarked;
        let alert = crate::alerts::NewAlert::shoal(kind, about, ready_notice(&ready.marks));
        (daemon.coordination().tell_coordinator(
            daemon,
            alert,
            false,
            super::notifications::Source::Agent,
        ))
        .await;
        undrafted.context("marked ready for review, but a PR is still a draft")?;
        (daemon.check_conventions(&space, &settings, &mut ready.marks)).await;
        Ok(ready)
    }

    /// Checks `[review] require_for_ready` for `caller`. A chat has no HEAD:
    /// it works on no branch of its own, so its linked PRs are marked
    /// whatever their head, and no review covers it.
    async fn ready_head(
        &self,
        daemon: &dyn Daemon,
        space: &Space,
        caller: Option<&Caller>,
    ) -> Result<Option<CommitId>> {
        let Some(workspace) = space.to_workspace() else {
            return Ok(None);
        };
        let workspace = &workspace;
        self.verify_worktree(daemon, workspace).await?;
        let head = daemon.current_head(workspace).await?;
        daemon
            .require_review(workspace, ReviewGate::Ready, &head, caller)
            .await?;
        Ok(Some(head))
    }

    /// A failure, or an environment that cannot be rendered, notifies and
    /// keeps the marks.
    async fn run_post_ready(
        &self,
        daemon: &dyn Daemon,
        space: &Space,
        settings: &Effective,
        command: &Path,
        marks: &[ReviewMark],
    ) {
        let configured = match daemon.acting_repository(space).await {
            Ok(repository) => {
                (self.hook_environment(daemon, space, Some(&repository), settings)).await
            }
            Err(error) => Err(error),
        };
        let result = match configured {
            Ok(configured) => {
                let hook = Hook::PostReady(marks);
                (hooks::run_detached(hook, space, command, &self.paths, &configured)).await
            }
            Err(error) => Err(error),
        };
        if let Err(error) = result {
            let message = format!("marked ready for review; {error:#}");
            let kind = AlertKind::HookFailed;
            (daemon
                .coordination()
                .notify_space(daemon, &space.name, &space.id, kind, message))
            .await;
        }
    }

    async fn record_marks(
        &self,
        id: &SpaceId,
        kind: Option<ItemKind>,
        explicit: Option<ItemRef>,
        pulls: Pulls,
    ) -> Result<ReadyMarks> {
        let id = id.to_owned();
        let created_at = crate::time::UnixTime::now();
        let records = self.records.clone();
        self.store
            .write(move |tx| {
                require_ready_space(tx, &id)?;
                let linked = records.assigned_items(tx, &id, kind)?;
                let items = marked_items(linked, explicit, kind, &pulls)?;
                // Linked-item marks replace a mark made while nothing was
                // linked.
                if items.iter().any(Option::is_some) {
                    tx.execute(
                        "DELETE FROM space_review WHERE space_id=?1 AND url=''",
                        [&id],
                    )?;
                }
                let mut marks = Vec::new();
                for url in items {
                    let kind = url.as_ref().and_then(|url| url.kind().item());
                    tx.execute(
                        "INSERT INTO space_review(space_id,url,kind,created_at)
                         VALUES (?1,?2,?3,?4)
                         ON CONFLICT(space_id,url) DO UPDATE
                         SET created_at=excluded.created_at",
                        params![id, url.as_ref().map_or("", ItemRef::url), kind, created_at],
                    )?;
                    marks.push(ReviewMark {
                        kind,
                        url,
                        created_at,
                        warnings: Vec::new(),
                    });
                }
                Ok(ReadyMarks {
                    marks,
                    skipped: pulls.skipped,
                })
            })
            .await
    }

    /// Withdraws every mark without a selection. `draft`, else `[pr]
    /// draft_on_unready`, first returns their PRs to draft. A failure there
    /// keeps the marks, so the withdrawal can be retried.
    pub async fn clear_ready(
        &self,
        daemon: &dyn Daemon,
        selector: &SpaceSelector,
        selection: Selection,
        draft: Option<bool>,
    ) -> Result<Vec<ReviewMark>> {
        let id = self.space(selector).await?.id.to_owned();
        let _guard = daemon.lock_change_gates(&id, &[]).await?;
        let space = self.space(&SpaceSelector::id(&id)).await?;
        let kind = marked_kind(&selection)?;
        let origin = daemon.origin_path(&space).await?;
        let explicit = marked_item(daemon.explicit_item(&origin, &selection).await?);
        let selected = self.selected_marks(&space.id, kind, explicit).await?;
        let draft = match draft {
            Some(draft) => draft,
            None => daemon.acting_settings(&space).await?.pr.draft_on_unready,
        };
        if draft {
            daemon
                .set_draft(&space, &pr_urls(&selected), true)
                .await
                .context(
                    "kept the marks: a PR could not return to draft; retry, or pass --no-draft",
                )?;
        }
        self.delete_marks(&space.id, &selected).await?;
        Ok(selected)
    }

    /// Refuses an empty selection and an explicit item that is not marked.
    async fn selected_marks(
        &self,
        id: &SpaceId,
        kind: Option<ItemKind>,
        explicit: Option<ItemRef>,
    ) -> Result<Vec<ReviewMark>> {
        let id = id.to_owned();
        self.store
            .run(move |db| {
                let mut selected = list(db, &id)?;
                selected.retain(|mark| match (&explicit, kind) {
                    (Some(url), _) => mark.url.as_ref() == Some(url),
                    (None, Some(kind)) => mark.kind == Some(kind),
                    (None, None) => true,
                });
                match (&explicit, kind) {
                    (Some(url), _) => {
                        let kind = url.kind();
                        ensure!(!selected.is_empty(), "{kind} is not marked ready: {url}")
                    }
                    (None, Some(kind)) => {
                        ensure!(!selected.is_empty(), "no {kind} is marked ready")
                    }
                    (None, None) => ensure!(!selected.is_empty(), "nothing is marked ready"),
                }
                Ok(selected)
            })
            .await
    }

    async fn delete_marks(&self, id: &SpaceId, marks: &[ReviewMark]) -> Result<()> {
        let id = id.to_owned();
        let urls: Vec<String> = marks
            .iter()
            .map(|mark| {
                mark.url
                    .as_ref()
                    .map_or_else(String::new, ToString::to_string)
            })
            .collect();
        self.store
            .write(move |tx| {
                for url in &urls {
                    tx.execute(
                        "DELETE FROM space_review WHERE space_id=?1 AND url=?2",
                        params![id, url],
                    )?;
                }
                Ok(())
            })
            .await
    }
}

/// The space itself when nothing is linked.
fn marked_items(
    linked: Vec<ItemRef>,
    explicit: Option<ItemRef>,
    kind: Option<ItemKind>,
    pulls: &Pulls,
) -> Result<Vec<Option<ItemRef>>> {
    if let Some(url) = explicit {
        let kind = url.kind();
        ensure!(linked.contains(&url), "{kind} is not linked: {url}");
        if let Some(reason) = pulls.skip_reason(&url) {
            bail!("{kind} cannot be marked ready: {url}: {reason}");
        }
        return Ok(vec![Some(url)]);
    }
    if linked.is_empty() {
        ensure!(
            kind.is_none(),
            "no linked items of the selected kind; register one with shoal link"
        );
        return Ok(vec![None]);
    }
    let items: Vec<_> = (linked.into_iter())
        .filter(|url| pulls.skip_reason(url).is_none())
        .map(Some)
        .collect();
    ensure!(
        !items.is_empty(),
        "no linked item can be marked ready: {}",
        skipped_list(&pulls.skipped)
    );
    Ok(items)
}

fn skipped_list(skipped: &[SkippedPr]) -> String {
    (skipped.iter())
        .map(|skipped| format!("{}: {}", skipped.url, skipped.reason))
        .collect::<Vec<_>>()
        .join("; ")
}

fn pr_urls(marks: &[ReviewMark]) -> Vec<ItemRef> {
    marks
        .iter()
        .filter(|mark| mark.kind == Some(ItemKind::Pr))
        .filter_map(|mark| mark.url.clone())
        .collect()
}

fn ready_notice(marks: &[ReviewMark]) -> String {
    let items = marks
        .iter()
        .map(|mark| mark.url.as_ref().map_or("the space", ItemRef::url))
        .collect::<Vec<_>>()
        .join(", ");
    format!("marked ready for review: {items}")
}

/// CI runs take no marks.
fn marked_kind(selection: &Selection) -> Result<Option<ItemKind>> {
    match selection.kind {
        Some(LinkKind::Ci) => bail!("CI runs are not marked ready for review"),
        kind => Ok(kind.and_then(LinkKind::item)),
    }
}

/// `marked_kind` has already refused a run.
fn marked_item(explicit: Option<ItemRef>) -> Option<ItemRef> {
    explicit.filter(|item| item.kind().item().is_some())
}

#[cfg(test)]
mod tests;
