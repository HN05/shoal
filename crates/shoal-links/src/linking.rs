//! Linking issues, PRs and CI runs to spaces, workspaces and chats, by
//! relation. Assigned items keep the issue and PR watch rules, which drive
//! a workspace's completion and cleanup; opened and related items are only
//! recorded, and watches follow linked runs until they finish.
use std::sync::Arc;

use anyhow::{Result, bail, ensure};
use shoal_core::refusal::{Reason, Refusal};
use shoal_daemon_base::callers::Caller;
use shoal_forge::items::action::ActionRequests;
use shoal_forge_types::items::{
    link::{ItemKind, LinkKind},
    reference::{ItemInput, ItemRef},
};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::{
    links::{Linked, Unlinked, pr::Action},
    model::{Assigner, LinkedItem, Relation},
    space_names::SpaceSelector,
};

use crate::{Links, daemon::Daemon, forges, issue::assignees::AssignerOf, space::LinkSpace};

impl Links {
    /// Link the item `input` names with `relation`; an issue or PR linked
    /// without its title gets the one the forge shows, or else none, which
    /// the answer says why.
    #[allow(
        clippy::too_many_arguments,
        reason = "the fields of the link request, beside the daemon and its caller"
    )]
    pub async fn link_item(
        self: &Arc<Self>,
        daemon: &dyn Daemon,
        selector: &SpaceSelector,
        kind: LinkKind,
        input: &ItemInput,
        relation: Relation,
        title: Option<String>,
        caller: Option<&Caller>,
    ) -> Result<Linked> {
        let (title, title_error) = match (kind.item(), title) {
            (Some(kind), None) => self.item_title(daemon, selector, kind, input, caller).await,
            (_, title) => (title, None),
        };
        let warnings = self
            .record_link(daemon, selector, kind, input, relation, title, caller)
            .await?;
        Ok(Linked {
            title_error,
            warnings,
        })
    }

    /// Record the link [`Self::link_item`] makes, with `title`, returning
    /// what assigning an issue on the forge warns of.
    #[allow(
        clippy::too_many_arguments,
        reason = "the fields of the link request, beside the daemon and its caller"
    )]
    async fn record_link(
        self: &Arc<Self>,
        daemon: &dyn Daemon,
        selector: &SpaceSelector,
        kind: LinkKind,
        input: &ItemInput,
        relation: Relation,
        title: Option<String>,
        caller: Option<&Caller>,
    ) -> Result<Vec<String>> {
        let assigner = Assigner::of(caller);
        match (relation, kind) {
            (Relation::Assigned, LinkKind::Issue) => {
                return self
                    .set_issue(daemon, selector, input, title, assigner)
                    .await;
            }
            (Relation::Assigned, LinkKind::Pr) => {
                let url = input.clone();
                self.set_pr(daemon, selector, Action::Watch { url, title })
                    .await
            }
            (Relation::Assigned, LinkKind::Ci) => bail!(
                "a CI run cannot be assigned, because assigned items drive completion \
                 and cleanup; link it as opened or related"
            ),
            (relation, LinkKind::Ci) => {
                self.record_run(daemon, selector, relation, input, caller)
                    .await
            }
            (relation, kind) => {
                self.record_item(daemon, selector, relation, kind, input, title)
                    .await
            }
        }?;
        Ok(Vec::new())
    }

    /// The title of the issue or PR `input` names, which lists show for the
    /// space, or else why the forge could not be read.
    async fn item_title(
        &self,
        daemon: &dyn Daemon,
        selector: &SpaceSelector,
        kind: ItemKind,
        input: &ItemInput,
        caller: Option<&Caller>,
    ) -> (Option<String>, Option<String>) {
        let read = async {
            let context = self.link_space(daemon, selector).await?.item_context();
            let forge = self.context_forge(daemon, &context).await?;
            let item = ItemRef::in_repository(&forge, kind.into(), input)?;
            let account = self.read_account(caller);
            let current = forge.read(&account, kind, item.number().get()).await?;
            anyhow::Ok(current.item.title)
        };
        match read.await {
            Ok(title) => (Some(title), None),
            Err(error) => (None, Some(format!("{error:#}"))),
        }
    }

    /// Unlink the relation's items of `kind`, or only the one `input` names;
    /// one must be linked.
    pub async fn unlink_items(
        self: &Arc<Self>,
        daemon: &dyn Daemon,
        selector: &SpaceSelector,
        relation: Relation,
        kind: Option<LinkKind>,
        input: Option<&ItemInput>,
    ) -> Result<Unlinked> {
        ensure!(
            input.is_none() || kind.is_some(),
            "an item to unlink needs its kind"
        );
        if relation == Relation::Assigned {
            let kind = match kind.map(LinkKind::item) {
                None => None,
                Some(Some(kind)) => Some(kind),
                Some(None) => bail!("{} linked as {relation}", nothing(kind)),
            };
            let warnings = self.unassign(daemon, selector, kind, input).await?;
            return Ok(Unlinked { warnings });
        }
        self.forget_items(daemon, selector, relation, kind, input)
            .await?;
        Ok(Unlinked::default())
    }

    /// Record an opened or related run named by number or URL, once the
    /// forge shows it, with its workflow as its title; workspace processes
    /// link only runs of their own repository.
    async fn record_run(
        self: &Arc<Self>,
        daemon: &dyn Daemon,
        selector: &SpaceSelector,
        relation: Relation,
        input: &ItemInput,
        caller: Option<&Caller>,
    ) -> Result<()> {
        let space = self.link_space(daemon, selector).await?;
        let item = self.linked_run(daemon, &space, input, caller).await?;
        self.record_items(&space.id, vec![(relation, item)]).await
    }

    /// Record an opened or related item named by number or URL.
    async fn record_item(
        self: &Arc<Self>,
        daemon: &dyn Daemon,
        selector: &SpaceSelector,
        relation: Relation,
        kind: LinkKind,
        input: &ItemInput,
        title: Option<String>,
    ) -> Result<()> {
        let space = self.link_space(daemon, selector).await?;
        daemon.verify_space(&space).await?;
        let url = self.space_item(daemon, &space, kind, input).await?;
        let item = LinkedItem { url, title };
        self.record_items(&space.id, vec![(relation, item)]).await
    }

    /// Forget a relation's recorded items of `kind`, or the one `input` names,
    /// which must be recorded.
    async fn forget_items(
        self: &Arc<Self>,
        daemon: &dyn Daemon,
        selector: &SpaceSelector,
        relation: Relation,
        kind: Option<LinkKind>,
        input: Option<&ItemInput>,
    ) -> Result<()> {
        let space = self.link_space(daemon, selector).await?;
        daemon.verify_space(&space).await?;
        let url = match (kind, input) {
            (Some(kind), Some(input)) => Some(self.space_item(daemon, &space, kind, input).await?),
            _ => None,
        };
        let removed = self
            .remove_items(&space.id, relation, kind, url.clone())
            .await?;
        match url {
            Some(url) => ensure!(
                removed > 0,
                Refusal::new(
                    Reason::NotFound,
                    format!("{url} is not linked as {relation}")
                )
            ),
            None => ensure!(
                removed > 0,
                Refusal::new(
                    Reason::NotFound,
                    format!("{} linked as {relation}", nothing(kind))
                )
            ),
        }
        Ok(())
    }

    /// Record opened or related items in one transaction.
    pub async fn record_items(
        self: &Arc<Self>,
        id: &SpaceId,
        items: Vec<(Relation, LinkedItem)>,
    ) -> Result<()> {
        let (id, links) = (id.to_owned(), self.clone());
        self.store
            .write(move |tx| {
                for (relation, item) in &items {
                    let title = item.title.as_deref();
                    links.record(tx, &id, *relation, item.kind(), item.url.url(), title)?;
                }
                Ok(())
            })
            .await
    }

    /// Remove a relation's recorded items in one transaction, returning how
    /// many were removed.
    async fn remove_items(
        self: &Arc<Self>,
        id: &SpaceId,
        relation: Relation,
        kind: Option<LinkKind>,
        url: Option<ItemRef>,
    ) -> Result<usize> {
        let (id, links) = (id.to_owned(), self.clone());
        self.store
            .write(move |tx| links.remove(tx, &id, relation, kind, url.as_ref().map(ItemRef::url)))
            .await
    }

    /// Unlink the assigned items of `kind`, or the one `input` names,
    /// returning why an issue's assignee stays on the forge.
    async fn unassign(
        self: &Arc<Self>,
        daemon: &dyn Daemon,
        selector: &SpaceSelector,
        kind: Option<ItemKind>,
        input: Option<&ItemInput>,
    ) -> Result<Vec<String>> {
        if input.is_none() {
            self.require_assigned(daemon, selector, kind).await?;
        }
        let mut warnings = Vec::new();
        if kind != Some(ItemKind::Pr) {
            warnings = self.clear_issue(daemon, selector, input).await?;
        }
        if kind != Some(ItemKind::Issue) {
            let action = input.map_or(Action::Clear, |url| Action::Unwatch { url: url.clone() });
            self.set_pr(daemon, selector, action).await?;
        }
        Ok(warnings)
    }

    /// Fail unless an item of `kind` is assigned: an issue, or a PR watch or
    /// acknowledgement, which an unreadable registration counts as.
    async fn require_assigned(
        &self,
        daemon: &dyn Daemon,
        selector: &SpaceSelector,
        kind: Option<ItemKind>,
    ) -> Result<()> {
        let space = self.link_space(daemon, selector).await?;
        let id = &space.id;
        let issue = kind != Some(ItemKind::Pr) && !self.issue_registrations(id).await?.is_empty();
        let pr =
            kind != Some(ItemKind::Issue) && !matches!(self.pr_registration(id).await, Ok(None));
        ensure!(
            issue || pr,
            Refusal::new(
                Reason::NotFound,
                format!(
                    "{} linked as {}",
                    nothing(kind.map(Into::into)),
                    Relation::Assigned
                )
            )
        );
        Ok(())
    }

    /// The issue or PR of the space's repository, or the CI run of any,
    /// that `input` names.
    async fn space_item(
        &self,
        daemon: &dyn Daemon,
        space: &LinkSpace,
        kind: LinkKind,
        input: &ItemInput,
    ) -> Result<ItemRef> {
        let forge = match kind {
            // A run's URL may name another repository.
            LinkKind::Ci => match forges::of_input(daemon, input).await? {
                Some(forge) => forge,
                None => forges::of_space(daemon, space).await?,
            },
            LinkKind::Issue | LinkKind::Pr => {
                let missing = "item lookup needs an origin remote";
                self.guarded_forge(daemon, space.origin_path(), missing)
                    .await?
            }
        };
        ItemRef::in_repository(&forge, kind, input)
    }
}

/// The subject of an error that no item of `kind` is linked.
fn nothing(kind: Option<LinkKind>) -> String {
    match kind {
        Some(kind) => format!("no {kind} is"),
        None => "nothing is".into(),
    }
}
