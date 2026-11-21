//! The PRs a space assigns, as its PR watch record names them, and their
//! titles.
use std::sync::Arc;

use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension};
use shoal_core::{
    git,
    refusal::{Reason, Refusal},
};
use shoal_daemon_base::events::{self, EventKind};
use shoal_forge::RepoRequests;
use shoal_forge_types::items::{
    link::ItemKind,
    reference::{ItemInput, ItemRef},
};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::{
    links::pr::{Action, Registration, RegistrationKind},
    model::{Relation, Workspace},
    space_names::SpaceSelector,
};
use shoal_refs::CommitId;

use crate::{Links, daemon::Daemon, space::LinkSpace};

pub mod titles;

/// The PRs the space `id` watches; an unreadable registration watches none.
pub fn watched_prs(db: &Connection, id: &SpaceId) -> rusqlite::Result<Vec<ItemRef>> {
    let registration = stored(db, id)?.and_then(|record| serde_json::from_str(&record).ok());
    Ok(watched_urls(registration.as_ref()).to_vec())
}

/// The PR registration of the space `id`, refusing an unreadable one.
pub fn registration(db: &Connection, id: &SpaceId) -> anyhow::Result<Option<Registration>> {
    let record = stored(db, id)?;
    Ok(record.map(|text| serde_json::from_str(&text)).transpose()?)
}

/// The PR registration of the space `id` as stored, which each reader
/// parses as it must take an unreadable one.
fn stored(db: &Connection, id: &SpaceId) -> rusqlite::Result<Option<String>> {
    db.query_row(
        "SELECT record FROM pr_cleanup WHERE space_id=?1",
        [id],
        |row| row.get(0),
    )
    .optional()
}

/// The PRs `registration` watches, none without one.
pub fn watched_urls(registration: Option<&Registration>) -> &[ItemRef] {
    match registration {
        Some(Registration {
            kind: RegistrationKind::Watch { urls, .. },
            ..
        }) => urls,
        _ => &[],
    }
}

/// How a new PR watch is checked before it is recorded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchCheck {
    /// Look the PR up with the user's login.
    Lookup,
    /// The caller just read the PR from the forge with the workspace branch as
    /// its head, so a lookup that could fail now must not leave it unlinked.
    Known,
}

/// A space's PR registration replacing its previous one, with the title
/// the caller knows of the PR it watches.
pub struct RegistrationChange {
    registration: Option<Registration>,
    pub added: Vec<ItemRef>,
    removed: Vec<ItemRef>,
    title: Option<(ItemRef, String)>,
}

impl RegistrationChange {
    pub fn new(
        previous: Option<&Registration>,
        registration: Option<Registration>,
        title: Option<(ItemRef, String)>,
    ) -> Self {
        let before = watched_urls(previous);
        let after = watched_urls(registration.as_ref());
        let added = after
            .iter()
            .filter(|url| !before.contains(url))
            .cloned()
            .collect();
        let removed = before
            .iter()
            .filter(|url| !after.contains(url))
            .cloned()
            .collect();
        Self {
            registration,
            added,
            removed,
            title,
        }
    }

    /// Store the registration within the caller's transaction. A dropped watch
    /// takes its replay cursors, review mark and title with it, an added one
    /// replaces the PR's opened or related link, and every watch added or
    /// dropped is journaled as an assigned PR link change. A title the caller
    /// knows replaces the recorded one.
    fn apply(&self, links: &Links, db: &Connection, id: &SpaceId) -> anyhow::Result<()> {
        links.forget_assigned(db, id, ItemKind::Pr, &self.removed)?;
        titles::forget(db, id, &self.removed)?;
        store_registration(db, id, self.registration.as_ref())?;
        for url in &self.added {
            crate::records::drop_recorded(db, id, url.url(), None)?;
            events::record_link(
                db,
                id,
                EventKind::Linked,
                Relation::Assigned,
                ItemKind::Pr,
                url.url(),
            )?;
        }
        if let Some((url, title)) = &self.title {
            titles::record(db, id, url.url(), title)?;
        }
        Ok(())
    }
}

fn store_registration(
    db: &Connection,
    id: &SpaceId,
    registration: Option<&Registration>,
) -> anyhow::Result<()> {
    match registration {
        Some(registration) => db.execute(
            "INSERT INTO pr_cleanup(space_id,record) VALUES (?1,?2)
             ON CONFLICT(space_id) DO UPDATE SET record=excluded.record",
            rusqlite::params![id, serde_json::to_string(registration)?],
        )?,
        None => db.execute("DELETE FROM pr_cleanup WHERE space_id=?1", [id])?,
    };
    Ok(())
}

impl Links {
    /// Change the PR registration of the space `selector` names under its
    /// PR gate, as [`Self::set_pr_gated`] does.
    pub async fn set_pr(
        self: &Arc<Self>,
        daemon: &dyn Daemon,
        selector: &SpaceSelector,
        action: Action,
    ) -> Result<()> {
        let id = self.link_space(daemon, selector).await?.id.to_owned();
        let _guard = self.gates.lock_prs([&id]).await;
        let space = self.link_space(daemon, &SpaceSelector::id(&id)).await?;
        self.set_pr_gated(daemon, &space, action, WatchCheck::Lookup)
            .await
    }

    /// Change the PR registration of `space` by `action`, for a caller
    /// already holding the space's PR gate.
    pub async fn set_pr_gated(
        self: &Arc<Self>,
        daemon: &dyn Daemon,
        space: &LinkSpace,
        action: Action,
        check: WatchCheck,
    ) -> Result<()> {
        if !matches!(action, Action::Clear | Action::Unwatch { .. }) {
            daemon.verify_space(space).await?;
        }
        let previous = match self.pr_registration(&space.id).await {
            Ok(previous) => previous,
            Err(_) if matches!(action, Action::Clear) => None,
            Err(error) => return Err(error),
        };
        let (kind, titled) = match action {
            Action::Clear => (None, None),
            Action::Unwatch { url } => {
                let kind =
                    (self.without_pr_watch(daemon, space.origin_path(), &space.id, url)).await?;
                (kind, None)
            }
            Action::Watch { url, title } => {
                let (kind, url) = self.with_pr_watch(daemon, space, url, check).await?;
                (Some(kind), title.map(|title| (url, title)))
            }
            Action::Acknowledge => (Some(self.merge_acknowledgement(daemon, space).await?), None),
        };
        let change = RegistrationChange::new(
            previous.as_ref(),
            kind.map(|kind| Registration { kind, error: None }),
            titled,
        );
        let (id, added) = (space.id.to_owned(), change.added.clone());
        let links = self.clone();
        (self.store)
            .write(move |tx| links.record_change(tx, &id, change))
            .await?;
        self.start_linked_watches_gated(daemon, &space.id, added)
            .await;
        daemon.wake_cleanup();
        Ok(())
    }

    /// The acknowledgement that the branch of `space`'s worktree merged at
    /// its HEAD; a space without a worktree has no branch of its own.
    async fn merge_acknowledgement(
        &self,
        daemon: &dyn Daemon,
        space: &LinkSpace,
    ) -> Result<RegistrationKind> {
        let workspace = (space.to_workspace()).with_context(|| {
            format!("{} has no branch whose merge it acknowledges", space.space)
        })?;
        // Links serve more than cleanup; only an acknowledgement exists for it alone.
        ensure!(
            daemon
                .workspace_settings(&workspace)
                .await?
                .cleanup
                .pr
                .enabled,
            "PR cleanup is disabled by [cleanup.pr] enabled = false"
        );
        let head = current_head(&workspace).await?;
        Ok(RegistrationKind::Acknowledgement { head })
    }

    /// The space's PR watches with the PR `input` names among them, and
    /// that PR.
    async fn with_pr_watch(
        &self,
        daemon: &dyn Daemon,
        space: &LinkSpace,
        input: ItemInput,
        check: WatchCheck,
    ) -> Result<(RegistrationKind, ItemRef)> {
        let (forge, url) = self.pr_forge(daemon, space.origin_path(), &input).await?;
        // Detect missing tools or login and invalid PRs now.
        if check == WatchCheck::Lookup {
            forge
                .merged_commits(&self.user_account(), url.pull()?)
                .await?;
        }
        let kind = self.adding_pr_watch(&space.id, url.clone()).await?;
        Ok((kind, url))
    }

    /// The PR watches of the space `id` with `url` among them.
    async fn adding_pr_watch(&self, id: &SpaceId, url: ItemRef) -> Result<RegistrationKind> {
        let (mut urls, mut merged_head) = match self.pr_registration(id).await? {
            Some(Registration {
                kind: RegistrationKind::Watch { urls, merged_head },
                ..
            }) => (urls, merged_head),
            _ => (Vec::new(), None),
        };
        if !urls.contains(&url) {
            urls.push(url);
            merged_head = None;
        }
        Ok(RegistrationKind::Watch { urls, merged_head })
    }

    /// The PR watches of the space `id` without the PR `input` names in the
    /// repository whose origin `checkout` has, which must be watched.
    async fn without_pr_watch(
        &self,
        daemon: &dyn Daemon,
        checkout: &std::path::Path,
        id: &SpaceId,
        input: ItemInput,
    ) -> Result<Option<RegistrationKind>> {
        let (_, url) = self.pr_forge(daemon, checkout, &input).await?;
        let Some(Registration {
            kind:
                RegistrationKind::Watch {
                    mut urls,
                    merged_head,
                },
            ..
        }) = self.pr_registration(id).await?
        else {
            return Err(Refusal::new(Reason::NotFound, format!("PR is not watched: {url}")).into());
        };
        ensure!(
            urls.contains(&url),
            Refusal::new(Reason::NotFound, format!("PR is not watched: {url}"))
        );
        urls.retain(|watched| watched != &url);
        Ok((!urls.is_empty()).then_some(RegistrationKind::Watch { urls, merged_head }))
    }

    /// The PR registration of the space `id`, refusing an unreadable one.
    pub async fn pr_registration(&self, id: &SpaceId) -> anyhow::Result<Option<Registration>> {
        let id = id.to_owned();
        self.store.read(move |tx| registration(tx, &id)).await
    }

    /// Apply `change` to the PR registration of the space `id` in a transaction
    /// that finds it ready: a removed space has no registration to change, and
    /// writing one would outlive it.
    pub fn record_change(
        &self,
        tx: &rusqlite::Transaction<'_>,
        id: &SpaceId,
        change: RegistrationChange,
    ) -> anyhow::Result<()> {
        self.records.require_ready_space(tx, id)?;
        change.apply(self, tx, id)
    }
}

/// The workspace's HEAD: its branch's commit, checking it is still on the
/// branch, or for a workspace at a commit its detached HEAD, checking it is
/// still detached.
pub async fn current_head(workspace: &Workspace) -> Result<CommitId> {
    let (head, branch) = git::head_and_branch(&workspace.path)
        .await
        .context("cannot check the workspace's branch")?;
    match workspace.branch() {
        Some(recorded) => ensure!(
            branch.is_some_and(|branch| branch == *recorded),
            "workspace is not on its recorded branch"
        ),
        None => ensure!(
            branch.is_none(),
            "workspace is at a commit but its worktree is on a branch"
        ),
    }
    Ok(head)
}

#[cfg(test)]
mod tests;
