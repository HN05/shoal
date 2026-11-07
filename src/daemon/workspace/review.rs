//! Ready-for-review marks: an agent's statement that linked work is ready at a
//! commit. Integrations read them from the event journal; they neither notify
//! the user nor affect completion or cleanup.
use std::{collections::BTreeMap, ffi::OsString, path::Path, time::Duration};

use anyhow::{Context, Result, bail, ensure};
use rusqlite::{Connection, params};

use super::{GuardMode, Manager};
use crate::client::space_names::SpaceSelector;
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
    },
    forge::items::link::{ItemKind, LinkKind, Selection},
    hooks::{self, Hook, HookKind, HookSpace},
    model::{Chat, ReadyMarks, Repository, ReviewMark, SkippedPr, Workspace},
};
use pulls::Pulls;

mod pulls;

/// How long a listing waits for its review checks before it leaves their
/// staleness unknown, so stalled file access cannot hold up the
/// reply.
const CHECK_LIMIT: Duration = Duration::from_secs(3);

/// When the review checks of a listing that starts now give up.
pub(super) fn check_deadline() -> tokio::time::Instant {
    tokio::time::Instant::now() + CHECK_LIMIT
}

pub(crate) fn list(db: &Connection, id: &str) -> Result<Vec<ReviewMark>> {
    Ok(db
        .prepare(
            "SELECT url,kind,head,created_at FROM space_review
             WHERE space_id=?1 ORDER BY created_at,url",
        )?
        .query_map([id], |row| {
            let url: String = row.get(0)?;
            Ok(ReviewMark {
                kind: row.get(1)?,
                url: (!url.is_empty()).then_some(url),
                head: row.get(2)?,
                created_at: row.get(3)?,
                stale: None,
                warnings: Vec::new(),
            })
        })?
        .collect::<rusqlite::Result<_>>()?)
}

/// Unlinking an item withdraws its mark in the same transaction.
pub(crate) fn forget(db: &Connection, id: &str, urls: &[String]) -> Result<()> {
    db.execute(
        "DELETE FROM space_review WHERE space_id=?1
         AND url IN (SELECT value FROM json_each(?2))",
        params![id, serde_json::to_string(urls)?],
    )?;
    Ok(())
}

impl Manager {
    /// Mark the selected linked items ready at HEAD, or the workspace itself
    /// when nothing is linked and nothing was selected. A linked PR is marked
    /// only while it is open with HEAD as its head on the forge; the others are
    /// skipped, and an explicitly selected one fails. Marking again moves
    /// the mark to the current HEAD and reruns `hooks.post_ready`. With
    /// `[pr] undraft_on_ready`, marked draft PRs leave draft before the hook runs;
    /// a failure there keeps the marks and runs the hook. With `gated`, as for
    /// a workspace process, `[review] require_for_ready` applies to the HEAD
    /// the marks record.
    pub async fn mark_ready(
        &self,
        selector: &SpaceSelector,
        selection: Selection,
        gated: bool,
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
        if gated {
            self.require_review(&workspace, Gate::Ready, &head).await?;
        }
        let kind = marked_kind(&selection)?;
        let explicit = marked_item(self.explicit_item(&workspace.path, &selection).await?);
        let space = LinkSpace::from(workspace.clone());
        let pulls = (self.markable_pulls(&space, kind, explicit.as_ref(), Some(&head))).await?;
        let mut ready = (self.record_marks(&workspace.id, kind, explicit, head, pulls)).await?;
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
                Err(error) => self.post_ready_failed(&workspace.name, &error).await,
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

    /// [`Self::mark_ready`] for a chat, which works on no branch of its own:
    /// each linked open PR is marked at its head on the forge, and an issue or
    /// the chat itself at its checkout's HEAD. A chat is unscoped, so no review
    /// gate applies, and no coordinator hears of it.
    async fn mark_chat_ready(&self, space: &LinkSpace, selection: Selection) -> Result<ReadyMarks> {
        let LinkSpace::Chat { chat, checkout } = space else {
            bail!("only a chat is marked this way");
        };
        let _guard = self.lock_change_gates(space.id(), &[]).await?;
        let settings = self.space_settings(space).await?;
        let kind = marked_kind(&selection)?;
        let explicit = marked_item(self.explicit_item(&checkout.path, &selection).await?);
        let pulls = (self.markable_pulls(space, kind, explicit.as_ref(), None)).await?;
        let (head, _) = crate::git::head_and_branch(&checkout.path)
            .await
            .context("cannot read the checkout's HEAD")?;
        let mut ready = (self.record_marks(space.id(), kind, explicit, head, pulls)).await?;
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
                Err(error) => self.post_ready_failed(&chat.name, &error).await,
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
            let name = match space {
                HookSpace::Workspace(workspace) => &workspace.name,
                HookSpace::Chat { chat, .. } => &chat.name,
            };
            self.post_ready_failed(name, &error).await;
        }
    }

    /// Tell the user that `hooks.post_ready` of the space `name` failed.
    async fn post_ready_failed(&self, name: &str, error: &anyhow::Error) {
        self.notify(
            Some(name),
            NotificationKind::HookFailed,
            format!("marked ready for review; {error:#}"),
        )
        .await;
    }

    /// Record marks of `kind`, or of the `explicit` item, at `head`, or for
    /// a PR at its own head among the `pulls`, which leave out those they skip.
    async fn record_marks(
        &self,
        id: &str,
        kind: Option<ItemKind>,
        explicit: Option<(String, ItemKind)>,
        head: String,
        pulls: Pulls,
    ) -> Result<ReadyMarks> {
        let id = id.to_owned();
        let created_at = crate::time::UnixTime::now();
        self.store
            .run(move |db| {
                let tx = db.transaction()?;
                require_ready_space(&tx, &id)?;
                let linked = linked_items(&tx, &id, kind)?;
                let items = marked_items(linked, explicit, kind, &pulls)?;
                // Linked-item marks replace a mark made while nothing was linked.
                if items.iter().any(|(url, _)| url.is_some()) {
                    tx.execute(
                        "DELETE FROM space_review WHERE space_id=?1 AND url=''",
                        [&id],
                    )?;
                }
                let mut marks = Vec::new();
                for (url, kind) in items {
                    let head = (url.as_ref())
                        .and_then(|url| pulls.heads.get(url))
                        .unwrap_or(&head)
                        .clone();
                    tx.execute(
                        "INSERT INTO space_review(space_id,url,kind,head,created_at)
                         VALUES (?1,?2,?3,?4,?5)
                         ON CONFLICT(space_id,url) DO UPDATE
                         SET head=excluded.head,created_at=excluded.created_at",
                        params![id, url.as_deref().unwrap_or(""), kind, head, created_at],
                    )?;
                    marks.push(ReviewMark {
                        kind,
                        url,
                        head,
                        created_at,
                        stale: Some(false),
                        warnings: Vec::new(),
                    });
                }
                tx.commit()?;
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
        id: &str,
        kind: Option<ItemKind>,
        explicit: Option<(String, ItemKind)>,
    ) -> Result<Vec<ReviewMark>> {
        let id = id.to_owned();
        self.store
            .run(move |db| {
                let mut selected = list(db, &id)?;
                selected.retain(|mark| match (&explicit, kind) {
                    (Some((url, _)), _) => mark.url.as_ref() == Some(url),
                    (None, Some(kind)) => mark.kind == Some(kind),
                    (None, None) => true,
                });
                match (&explicit, kind) {
                    (Some((url, kind)), _) => {
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

    async fn delete_marks(&self, id: &str, marks: &[ReviewMark]) -> Result<()> {
        let id = id.to_owned();
        let urls: Vec<String> = marks
            .iter()
            .map(|mark| mark.url.clone().unwrap_or_default())
            .collect();
        self.store
            .run(move |db| {
                let tx = db.transaction()?;
                for url in &urls {
                    tx.execute(
                        "DELETE FROM space_review WHERE space_id=?1 AND url=?2",
                        params![id, url],
                    )?;
                }
                tx.commit()?;
                Ok(())
            })
            .await
    }

    /// Say whether each mark still describes HEAD. HEAD that cannot be read
    /// by `deadline`, or is in a directory whose file access stalls, leaves staleness
    /// unknown rather than failing or holding up a listing.
    pub async fn annotate_review(&self, workspace: &mut Workspace, deadline: tokio::time::Instant) {
        if workspace.review.is_empty() {
            return;
        }
        let head = self.listed_head(workspace, deadline).await;
        for mark in &mut workspace.review {
            mark.stale = head.as_ref().map(|head| head != &mark.head);
        }
    }

    async fn listed_head(
        &self,
        workspace: &Workspace,
        deadline: tokio::time::Instant,
    ) -> Option<String> {
        if self.file_access.stalled(&workspace.path).is_some() {
            return None;
        }
        tokio::time::timeout_at(deadline, current_head(workspace))
            .await
            .ok()?
            .ok()
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
    linked: Vec<(String, ItemKind)>,
    explicit: Option<(String, ItemKind)>,
    kind: Option<ItemKind>,
    pulls: &Pulls,
) -> Result<Vec<(Option<String>, Option<ItemKind>)>> {
    if let Some((url, kind)) = explicit {
        ensure!(
            linked.iter().any(|(linked, _)| linked == &url),
            "{kind} is not linked: {url}"
        );
        if let Some(reason) = pulls.skip_reason(&url) {
            bail!("{kind} cannot be marked ready: {url}: {reason}");
        }
        return Ok(vec![(Some(url), Some(kind))]);
    }
    if linked.is_empty() {
        ensure!(
            kind.is_none(),
            "no linked items of the selected kind; register one with shoal link"
        );
        return Ok(vec![(None, None)]);
    }
    let items: Vec<_> = (linked.into_iter())
        .filter(|(url, _)| pulls.skip_reason(url).is_none())
        .map(|(url, kind)| (Some(url), Some(kind)))
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
fn pr_urls(marks: &[ReviewMark]) -> Vec<String> {
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
        .map(|mark| mark.url.as_deref().unwrap_or("the workspace"))
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
fn marked_item(explicit: Option<(String, LinkKind)>) -> Option<(String, ItemKind)> {
    explicit.and_then(|(url, kind)| Some((url, kind.item()?)))
}

#[cfg(test)]
mod tests;
