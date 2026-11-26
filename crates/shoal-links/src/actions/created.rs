//! Recording what a space opens as its own: an open PR of its branch, or an
//! item it just created.
use std::sync::Arc;

use anyhow::{Context, Result};
use shoal_daemon_base::events;
use shoal_forge_types::items::{
    change::ItemChange, item::Item, link::ItemKind, reference::ItemRef,
};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::{
    links::pr::Action,
    model::{Assigner, Relation, Workspace},
    space_names::SpaceSelector,
};

use super::ensure_branch_head;
use crate::{Links, daemon::Daemon, pr::WatchCheck};

impl Links {
    /// Link `item`, an open PR the workspace did not create, after checking
    /// that its head is the workspace branch.
    pub async fn link_open_pull(
        self: &Arc<Self>,
        daemon: &dyn Daemon,
        workspace: &Workspace,
        item: &Item,
    ) -> Result<()> {
        ensure_branch_head(workspace, item)?;
        let watch = Action::Watch {
            url: item.url.clone().into(),
            title: Some(item.title.clone()),
        };
        let space = workspace.clone().into();
        (self.set_pr_gated(daemon, &space, watch, WatchCheck::Known))
            .await
            .with_context(|| format!("{} is open, but linking it failed", item.url))
    }

    /// Assign an item the workspace just created with `link`, and record it
    /// as opened without, or when assigning it fails. A removal that starts
    /// after the caller's check leaves it unrecorded with a warning, so
    /// nothing fails once the item exists and a retry cannot create a
    /// duplicate. An assigned issue is assigned on the forge to the account
    /// of `assigner`. Returns whether it was assigned, and the warnings.
    pub async fn keep_created(
        self: &Arc<Self>,
        daemon: &dyn Daemon,
        space: &SpaceId,
        kind: ItemKind,
        item: &Item,
        link: bool,
        assigner: Assigner,
    ) -> Result<(bool, Vec<String>)> {
        self.journal_created(space, kind, item).await?;
        let failure = match link {
            true => match (self.assign_created(daemon, space, kind, item, assigner)).await {
                Ok(warnings) => return Ok((true, warnings)),
                Err(error) => Some(error),
            },
            false => None,
        };
        if !self.record_created(space, kind, item).await? {
            return Ok((false, vec![removal_warning(&item.url)]));
        }
        match failure {
            Some(error) => {
                Err(error.context(format!("{} was opened, but linking it failed", item.url)))
            }
            None => Ok((false, Vec::new())),
        }
    }

    /// Journal that the space created `item`.
    async fn journal_created(&self, space: &SpaceId, kind: ItemKind, item: &Item) -> Result<()> {
        let (id, url) = (space.to_owned(), item.url.clone());
        self.store
            .write(move |tx| events::record_item(tx, &id, kind, url.url(), ItemChange::Open))
            .await
            .with_context(|| format!("{} was opened, but recording it failed", item.url))
    }

    /// Assign `item`, which the space just created, returning what
    /// assigning an issue on the forge warns of.
    async fn assign_created(
        self: &Arc<Self>,
        daemon: &dyn Daemon,
        space: &SpaceId,
        kind: ItemKind,
        item: &Item,
        assigner: Assigner,
    ) -> Result<Vec<String>> {
        let url = item.url.clone();
        let (selector, title) = (SpaceSelector::id(space), Some(item.title.clone()));
        match kind {
            ItemKind::Issue => {
                (self.set_issue_gated(daemon, &selector, &url.into(), title, assigner)).await
            }
            ItemKind::Pr => {
                let watch = Action::Watch {
                    url: url.into(),
                    title,
                };
                let space = self.link_space(daemon, &selector).await?;
                (self.set_pr_gated(daemon, &space, watch, WatchCheck::Known)).await?;
                Ok(Vec::new())
            }
        }
    }

    /// Record `item`, which the space just created, as opened, returning
    /// whether it was recorded: a removal that started after the caller's
    /// check takes no link changes, and the item exists either way.
    async fn record_created(
        self: &Arc<Self>,
        space: &SpaceId,
        kind: ItemKind,
        item: &Item,
    ) -> Result<bool> {
        let (id, url, title) = (space.to_owned(), item.url.clone(), item.title.clone());
        let links = self.clone();
        self.store
            .write(move |tx| record_opened(&links, tx, &id, kind, url.url(), &title))
            .await
            .with_context(|| format!("{} was opened, but recording it failed", item.url))
    }
}

/// Record an item the workspace created as opened in one transaction,
/// returning whether it was recorded, which a removal that started after the
/// caller's check prevents.
fn record_opened(
    links: &Links,
    tx: &rusqlite::Transaction<'_>,
    id: &SpaceId,
    kind: ItemKind,
    url: &str,
    title: &str,
) -> Result<bool> {
    let recorded = links.records.accepts_links(tx, id)?;
    if recorded {
        links.record(tx, id, Relation::Opened, kind, url, Some(title))?;
    }
    Ok(recorded)
}

/// The warning for an item created after removal started.
fn removal_warning(url: &ItemRef) -> String {
    format!("{url} was opened, but removal started first, so the workspace does not link it")
}
