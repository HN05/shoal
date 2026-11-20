//! The items a space's watches, background polls and views follow, and
//! the item an explicit selection names.
use anyhow::{Context, Result, ensure};
use shoal_links::space::LinkSpace;
use shoal_links::wait::followed::{LinkedItems, followed_items, shown_items};

use crate::{
    daemon::manager::Manager,
    forge::items::{
        link::{LinkKind, Selection},
        reference::{ItemInput, ItemRef},
    },
    model::Space,
    repository,
};
use shoal_ids::{repository_ids::RepositoryId, space_ids::SpaceId};

pub(super) const NO_LINKED_ITEMS: &str =
    "no linked items of the selected kind; register one with shoal link";
impl Manager {
    /// The explicitly selected item, linked or not, of the repository whose
    /// origin `checkout` has, or of a CI run of any.
    pub(crate) async fn explicit_item(
        &self,
        checkout: &std::path::Path,
        selection: &Selection,
    ) -> Result<Option<ItemRef>> {
        let Some(input) = &selection.input else {
            return Ok(None);
        };
        let remote = repository::remote_url_from_path(checkout)
            .await?
            .context("item lookup needs an origin remote")?;
        let forge = self.forge(&remote).await?;
        let kind = selection.kind.context("explicit items need a kind")?;
        // A run's URL may name another repository.
        let named = match kind {
            LinkKind::Ci => shoal_links::forges::of_input(self, input).await?,
            LinkKind::Issue | LinkKind::Pr => None,
        };
        let item = ItemRef::in_repository(&named.unwrap_or(forge), kind, input)?;
        Ok(Some(item))
    }

    /// The registered repository in which `space` watches the item
    /// `selection` names: none for a space with a repository, which watches
    /// its own, and for a chat without one, which reads only registered
    /// repositories, the one the item's URL names, or else `named`.
    pub(super) async fn watched_repository(
        &self,
        space: &Space,
        selection: &Selection,
        named: Option<&RepositoryId>,
    ) -> Result<Option<RepositoryId>> {
        if space.repository_id.as_ref().is_some() {
            ensure!(
                named.is_none(),
                "only a chat without a repository names the repository of an item it watches"
            );
            return Ok(None);
        }
        let Some(input) = &selection.input else {
            ensure!(
                named.is_none(),
                "--repo names the repository of an item number, not of a kind"
            );
            return Ok(None);
        };
        let ItemInput::Url(item) = input else {
            let named = named.with_context(|| {
                format!(
                    "chat {} has no repository of its own; name the item's repository with \
                     --repo or select the item by its URL",
                    space.name
                )
            })?;
            return Ok(Some(named.clone()));
        };
        let repository = self.item_repository(item).await?;
        ensure!(
            named.is_none_or(|named| *named == repository),
            "{item} is not in the repository --repo names"
        );
        Ok(Some(repository))
    }

    /// The registered repository of `item`.
    async fn item_repository(&self, item: &ItemRef) -> Result<RepositoryId> {
        let repository = self.item_forge(item).await?.repository_url();
        let registered = self.registered_named(Some(&repository)).await?;
        let registered = registered.with_context(|| {
            format!("{item} is in no registered repository; only those are read from a chat")
        })?;
        Ok(registered.id)
    }

    /// The explicitly selected item, or the space's linked items of the
    /// selected kind, with every linked run.
    pub(crate) async fn selected_items(
        &self,
        space: &LinkSpace,
        selection: &Selection,
    ) -> Result<Vec<ItemRef>> {
        let items = self.selection_items(space, selection, shown_items).await?;
        ensure!(
            !items.is_empty() || selection.kind.is_none(),
            "{NO_LINKED_ITEMS}"
        );
        Ok(items)
    }

    /// The space's own items a watch selects. A kind it links none of is
    /// still watched when a child workspace whose activity its agent receives
    /// links one.
    pub(super) async fn watched_items(
        &self,
        space: &LinkSpace,
        selection: &Selection,
    ) -> Result<Vec<ItemRef>> {
        let items = self
            .selection_items(space, selection, followed_items)
            .await?;
        if items.is_empty() && selection.input.is_none() {
            self.ensure_links_kind(&space.id, selection.kind).await?;
        }
        Ok(items)
    }

    /// Refuse a watch of `kind` that neither the space `id` nor a space
    /// below it whose activity its agent receives links an item of.
    pub(super) async fn ensure_links_kind(
        &self,
        id: &SpaceId,
        kind: Option<LinkKind>,
    ) -> Result<()> {
        let id = id.to_owned();
        let agents = self.tracked_agents().await;
        ensure!(
            self.store
                .run(move |db| links_kind(db, &id, kind, &agents))
                .await?,
            "{NO_LINKED_ITEMS}"
        );
        Ok(())
    }

    /// The explicitly selected item, or the space's linked items of the
    /// selected kind that `linked` lists.
    pub(in crate::daemon::links) async fn selection_items(
        &self,
        space: &LinkSpace,
        selection: &Selection,
        linked: LinkedItems,
    ) -> Result<Vec<ItemRef>> {
        if let Some(item) = self.explicit_item(space.origin_path(), selection).await? {
            return Ok(vec![item]);
        }
        let id = space.id.to_owned();
        let kind = selection.kind;
        self.store.run(move |db| linked(db, &id, kind)).await
    }
}

/// Whether the workspace, or a child workspace whose activity its agent
/// receives, links an item of `kind`; every workspace has items of no kind.
pub(super) fn links_kind(
    db: &rusqlite::Connection,
    id: &SpaceId,
    kind: Option<LinkKind>,
    agents: &crate::daemon::tree::TrackedAgents,
) -> Result<bool> {
    if kind.is_none() || !followed_items(db, id, kind)?.is_empty() {
        return Ok(true);
    }
    for child in crate::daemon::tree::delegated(db, id, agents)? {
        if !followed_items(db, &child.id, kind)?.is_empty() {
            return Ok(true);
        }
    }
    Ok(false)
}
