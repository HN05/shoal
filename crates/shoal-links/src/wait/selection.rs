//! The items a request selects among a space's links: the one it names, or
//! the space's linked items of a kind, and the registered repository a chat
//! without one watches an item in.
use anyhow::{Context, Result, ensure};
use shoal_core::repository::remote_url_from_path;
use shoal_forge_types::items::{
    link::{LinkKind, Selection},
    reference::{ItemInput, ItemRef},
};
use shoal_ids::{repository_ids::RepositoryId, space_ids::SpaceId};
use shoal_protocol::model::Space;

use super::{
    activity::links_kind,
    followed::{LinkedItems, followed_items, shown_items},
};
use crate::{Links, daemon::Daemon, forges, space::LinkSpace};

pub const NO_LINKED_ITEMS: &str =
    "no linked items of the selected kind; register one with shoal link";

impl Links {
    /// The explicitly selected item, linked or not, of the repository whose
    /// origin `checkout` has, or of a CI run of any.
    pub async fn explicit_item(
        &self,
        checkout: &std::path::Path,
        selection: &Selection,
    ) -> Result<Option<ItemRef>> {
        let Some(input) = &selection.input else {
            return Ok(None);
        };
        let remote = remote_url_from_path(checkout)
            .await?
            .context("item lookup needs an origin remote")?;
        let forge = self.forges.forge(&remote).await?;
        let kind = selection.kind.context("explicit items need a kind")?;
        // A run's URL may name another repository.
        let named = match kind {
            LinkKind::Ci => forges::of_input(&self.forges, input).await?,
            LinkKind::Issue | LinkKind::Pr => None,
        };
        let item = ItemRef::in_repository(&named.unwrap_or(forge), kind, input)?;
        Ok(Some(item))
    }

    /// The registered repository in which `space` watches the item
    /// `selection` names: none for a space with a repository, which watches
    /// its own, and for a chat without one, which reads only registered
    /// repositories, the one the item's URL names, or else `named`.
    pub async fn watched_repository(
        &self,
        daemon: &dyn Daemon,
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
        let repository = self.item_repository(daemon, item).await?;
        ensure!(
            named.is_none_or(|named| *named == repository),
            "{item} is not in the repository --repo names"
        );
        Ok(Some(repository))
    }

    /// The registered repository of `item`.
    async fn item_repository(&self, daemon: &dyn Daemon, item: &ItemRef) -> Result<RepositoryId> {
        let repository = self.forges.item_forge(item).await?.repository_url();
        let registered = daemon.registered_named(Some(&repository)).await?;
        let registered = registered.with_context(|| {
            format!("{item} is in no registered repository; only those are read from a chat")
        })?;
        Ok(registered.id)
    }

    /// The explicitly selected item, or the space's linked items of the
    /// selected kind, with every linked run.
    pub async fn selected_items(
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
    pub async fn watched_items(
        &self,
        daemon: &dyn Daemon,
        space: &LinkSpace,
        selection: &Selection,
    ) -> Result<Vec<ItemRef>> {
        let items = self
            .selection_items(space, selection, followed_items)
            .await?;
        if items.is_empty() && selection.input.is_none() {
            self.ensure_links_kind(daemon, &space.id, selection.kind)
                .await?;
        }
        Ok(items)
    }

    /// Refuse a watch of `kind` that neither the space `id` nor a space
    /// below it whose activity its agent receives links an item of.
    pub async fn ensure_links_kind(
        &self,
        daemon: &dyn Daemon,
        id: &SpaceId,
        kind: Option<LinkKind>,
    ) -> Result<()> {
        let agents = daemon.tracked_agents().await;
        let (id, records) = (id.to_owned(), self.records.clone());
        let links = (self.store).read(move |db| links_kind(&*records, db, &id, kind, &agents));
        ensure!(links.await?, "{NO_LINKED_ITEMS}");
        Ok(())
    }

    /// The explicitly selected item, or the space's linked items of the
    /// selected kind that `linked` lists.
    pub async fn selection_items(
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
        self.store.read(move |db| linked(db, &id, kind)).await
    }
}
