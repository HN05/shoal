//! Ready-for-review marks: an agent's statement that it needs the user before
//! it can go on. Integrations read them from the event journal; they neither
//! notify the user nor affect completion or cleanup.
use crate::config::hooks::HookCommand;
use crate::forge::items::reference::ItemRef;
use shoal_ids::space_ids::SpaceId;
use std::{collections::BTreeMap, ffi::OsString, path::Path};

use anyhow::{Context, Result, bail, ensure};
use rusqlite::{Connection, params};

use super::{GuardMode, Manager};
use crate::client::space_names::{SpaceName, SpaceSelector};
use crate::{
    config::Effective,
    daemon::{
        links::{
            pr::current_head,
            space::{LinkSpace, require_ready_space},
            wait::linked_items,
        },
        notifications::NotificationKind,
        reviews::gate::Gate,
        scope::Caller,
    },
    forge::items::link::{ItemKind, LinkKind, Selection},
    hooks::{self, Hook, HookKind, HookSpace},
    model::{Chat, ReadyMarks, Repository, ReviewMark, SkippedPr},
};
use pulls::Pulls;

mod pulls;

pub(crate) fn list(db: &Connection, id: &SpaceId) -> Result<Vec<ReviewMark>> {
    Ok(db
        .prepare(
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
pub(crate) fn forget(db: &Connection, id: &SpaceId, urls: &[String]) -> Result<()> {
    db.execute(
        "DELETE FROM space_review WHERE space_id=?1
         AND url IN (SELECT value FROM json_each(?2))",
        params![id, serde_json::to_string(urls)?],
    )?;
    Ok(())
}

impl Manager {
    /// Mark the selected linked items ready, or the workspace itself when
    /// nothing is linked and nothing was selected. A linked PR is marked
    /// only while it is open with HEAD as its head on the forge; the others are
    /// skipped, and an explicitly selected one fails. Marking again refreshes
    /// the mark and reruns `hooks.post_ready`. With
    /// `[pr] undraft_on_ready`, marked draft PRs leave draft before the hook runs,
    /// which removes their progress section with the next publication; a
    /// failure there keeps the marks and runs the hook. For a scoped
    /// `caller`, `[review] require_for_ready` applies to HEAD.
    pub async fn mark_ready(
        &self,
        selector: &SpaceSelector,
        selection: Selection,
        caller: Option<&Caller>,
    ) -> Result<ReadyMarks> {
        if let space @ LinkSpace::Chat { .. } = self.link_space(selector).await? {
            return self.mark_chat_ready(&space, selection).await;
        }
        let id = self.workspace(selector).await?.id;
        let _guard = self.lock_change_gates(&id, &[]).await?;
        let workspace = self.workspace_by_id(&id).await?;
        workspace.require_branch("shoal ready")?;
        let settings = self.workspace_settings(&workspace).await?;
        let command = HookKind::PostReady
            .command(&settings)
            .map(|path| workspace.path.join(path));
        // Like hooks.post_done, the hook excludes lifecycle and permit changes.
        let _resources = match command {
            Some(_) => Some(
                self.gates
                    .resource_guard(&workspace.id, GuardMode::Exclusive)?,
            ),
            None => None,
        };
        self.verify_worktree(&workspace).await?;
        let head = current_head(&workspace).await?;
        if caller.is_some() {
            self.require_review(&workspace, Gate::Ready, &head).await?;
        }
        let kind = marked_kind(&selection)?;
        let explicit = marked_item(self.explicit_item(&workspace.path, &selection).await?);
        let space = LinkSpace::from(workspace.clone());
        let pulls =
            (self.markable_pulls(&space, kind, explicit.as_ref(), Some(head.as_str()))).await?;
        let mut ready = (self.record_marks(&workspace.id, kind, explicit, pulls)).await?;
        let undrafted = match settings.pr.undraft_on_ready {
            true => self.set_draft(&space, &pr_urls(&ready.marks), false).await,
            false => Ok(()),
        };
        if let Some(command) = command {
            match self.configured_workspace_environment(&workspace).await {
                Ok(configured) => {
                    let marks = &ready.marks;
                    (self.run_post_ready((&workspace).into(), &command, &configured, marks)).await;
                }
                Err(error) => {
                    self.post_ready_failed((&workspace.name, &workspace.id), &error)
                        .await
                }
            }
        }
        self.tell_coordinator(
            &workspace.name,
            Some(&workspace.id),
            None,
            ready_notice(&ready.marks),
            Vec::new(),
            super::Source::Agent,
        )
        .await;
        undrafted.context("marked ready for review, but a PR is still a draft")?;
        (self.check_conventions(&space, &settings, &mut ready.marks)).await;
        Ok(ready)
    }

    /// [`Self::mark_ready`] for a chat, which works on no branch of its own,
    /// so each linked open PR is marked whatever its head. A chat is unscoped,
    /// so no review gate applies, and no coordinator hears of it.
    async fn mark_chat_ready(&self, space: &LinkSpace, selection: Selection) -> Result<ReadyMarks> {
        let LinkSpace::Chat { chat, checkout } = space else {
            bail!("only a chat is marked this way");
        };
        let _guard = self.lock_change_gates(space.id(), &[]).await?;
        let settings = self.space_settings(space).await?;
        let kind = marked_kind(&selection)?;
        let explicit = marked_item(self.explicit_item(&checkout.path, &selection).await?);
        let pulls = (self.markable_pulls(space, kind, explicit.as_ref(), None)).await?;
        let mut ready = (self.record_marks(space.id(), kind, explicit, pulls)).await?;
        let undrafted = match settings.pr.undraft_on_ready {
            true => self.set_draft(space, &pr_urls(&ready.marks), false).await,
            false => Ok(()),
        };
        if let Some(command) = HookKind::PostReady.command(&settings) {
            let hook = HookSpace::Chat {
                chat,
                checkout: &checkout.path,
            };
            // As for a workspace, a hook that cannot run keeps the marks.
            match chat_hook_environment(chat, checkout, &settings) {
                Ok(configured) => {
                    let command = checkout.path.join(command);
                    self.run_post_ready(hook, &command, &configured, &ready.marks)
                        .await;
                }
                Err(error) => self.post_ready_failed((&chat.name, &chat.id), &error).await,
            }
        }
        undrafted.context("marked ready for review, but a PR is still a draft")?;
        (self.check_conventions(space, &settings, &mut ready.marks)).await;
        Ok(ready)
    }

    /// Run `hooks.post_ready` in `space` with the `configured`
    /// environment; a failure notifies and keeps the marks.
    async fn run_post_ready(
        &self,
        space: HookSpace<'_>,
        command: &Path,
        configured: &BTreeMap<String, OsString>,
        marks: &[ReviewMark],
    ) {
        let hook = Hook::PostReady(marks);
        let result = hooks::run_detached(hook, space, command, &self.paths, configured).await;
        if let Err(error) = result {
            let space = match space {
                HookSpace::Workspace(workspace) => (&workspace.name, &workspace.id),
                HookSpace::Chat { chat, .. } => (&chat.name, &chat.id),
            };
            self.post_ready_failed(space, &error).await;
        }
    }

    /// Tell the user that `hooks.post_ready` of the space failed.
    async fn post_ready_failed(&self, (name, id): (&SpaceName, &SpaceId), error: &anyhow::Error) {
        self.notify_space(
            name,
            id,
            NotificationKind::HookFailed,
            format!("marked ready for review; {error:#}"),
        )
        .await;
    }

    /// Record marks of `kind`, or of the `explicit` item, leaving out the
    /// `pulls` skipped.
    async fn record_marks(
        &self,
        id: &SpaceId,
        kind: Option<ItemKind>,
        explicit: Option<ItemRef>,
        pulls: Pulls,
    ) -> Result<ReadyMarks> {
        let id = id.to_owned();
        let created_at = crate::time::UnixTime::now();
        self.store
            .write(move |tx| {
                require_ready_space(tx, &id)?;
                let linked = linked_items(tx, &id, kind)?;
                let items = marked_items(linked, explicit, kind, &pulls)?;
                // Linked-item marks replace a mark made while nothing was linked.
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

    /// Withdraw the selected marks, or every mark without a selection.
    /// `draft`, else `[pr] draft_on_unready`, first returns their PRs to draft; a
    /// failure there keeps the marks, so the withdrawal can be retried.
    pub async fn clear_ready(
        &self,
        selector: &SpaceSelector,
        selection: Selection,
        draft: Option<bool>,
    ) -> Result<Vec<ReviewMark>> {
        let id = self.link_space(selector).await?.id().to_owned();
        let _guard = self.lock_change_gates(&id, &[]).await?;
        let space = self.link_space(&SpaceSelector::id(&id)).await?;
        let kind = marked_kind(&selection)?;
        let explicit = marked_item(self.explicit_item(space.origin_path(), &selection).await?);
        let selected = self.selected_marks(space.id(), kind, explicit).await?;
        let draft = match draft {
            Some(draft) => draft,
            None => self.space_settings(&space).await?.pr.draft_on_unready,
        };
        if draft {
            self.set_draft(&space, &pr_urls(&selected), true)
                .await
                .context(
                    "kept the marks: a PR could not return to draft; retry, or pass --no-draft",
                )?;
        }
        self.delete_marks(space.id(), &selected).await?;
        Ok(selected)
    }

    /// The marks a withdrawal selects, of which there must be one: an
    /// explicit item must be marked.
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

/// What a chat's hooks add to their environment: the repository's `[env]`
/// with the placeholders a chat has, after its temporary directory.
fn chat_hook_environment(
    chat: &Chat,
    checkout: &Repository,
    settings: &Effective,
) -> Result<BTreeMap<String, OsString>> {
    let caches = crate::caches::directories(checkout, &settings.caches)?;
    Ok(crate::env::render_chat_environment(
        &settings.env,
        &checkout.path,
        crate::repository::name(checkout),
        &caches,
        chat.tmp_dir.as_deref(),
    ))
}

/// The items a mark of `kind`, or of the `explicit` item, covers among the
/// `linked` ones, leaving out the `pulls` it skips, or the space itself when
/// nothing is linked.
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

/// The `skipped` PRs with their reasons, on one line.
fn skipped_list(skipped: &[SkippedPr]) -> String {
    (skipped.iter())
        .map(|skipped| format!("{}: {}", skipped.url, skipped.reason))
        .collect::<Vec<_>>()
        .join("; ")
}

/// The PRs among `marks`.
fn pr_urls(marks: &[ReviewMark]) -> Vec<ItemRef> {
    marks
        .iter()
        .filter(|mark| mark.kind == Some(ItemKind::Pr))
        .filter_map(|mark| mark.url.clone())
        .collect()
}

/// What the coordinating agent hears when the workspace marks work ready.
fn ready_notice(marks: &[ReviewMark]) -> String {
    let items = marks
        .iter()
        .map(|mark| mark.url.as_ref().map_or("the workspace", ItemRef::url))
        .collect::<Vec<_>>()
        .join(", ");
    format!("marked ready for review: {items}")
}

/// The kind of item `selection` marks or unmarks; CI runs take no marks.
fn marked_kind(selection: &Selection) -> Result<Option<ItemKind>> {
    match selection.kind {
        Some(LinkKind::Ci) => bail!("CI runs are not marked ready for review"),
        kind => Ok(kind.and_then(LinkKind::item)),
    }
}

/// The explicitly selected issue or PR, which `marked_kind` has kept from
/// being a run.
fn marked_item(explicit: Option<ItemRef>) -> Option<ItemRef> {
    explicit.filter(|item| item.kind().item().is_some())
}

#[cfg(test)]
mod tests;
