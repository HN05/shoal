//! Link issues, PRs and CI runs to spaces, workspaces and chats, by
//! relation. Assigned items keep the issue and PR watch rules, which drive a
//! workspace's completion and cleanup; opened and related items are only
//! recorded, and watches follow linked runs until they finish.
mod actions;
pub(in crate::daemon) mod branches;
mod checks;
mod ci;
mod forges;
pub mod issue;
mod listing;
mod merge_path;
mod merge_review;
mod merge_warning;
mod permissions;
pub mod pr;
pub mod proposals;
mod pushes;
mod raw;
mod reads;
pub(crate) mod records;
mod relink;
mod revert;
mod run_approval;
mod runs;
pub(crate) mod space;
mod spaces;
pub(in crate::daemon) mod stack;
mod verdict;
mod view;
pub mod wait;

use crate::refusal::{Reason, Refusal};
use anyhow::{Context, Result, bail, ensure};
use rusqlite::{Connection, OptionalExtension};
use shoal_ids::space_ids::SpaceId;
use shoal_links::place::Access;
use shoal_links::space::LinkSpace;
use shoal_protocol::space_names::SpaceSelector;

use crate::{
    daemon::{links::pr::Action, manager::Manager, scope::Caller},
    forge::items::{
        link::{ItemKind, LinkKind},
        reference::{ItemInput, ItemRef},
    },
    model::{LinkedItem, Relation},
    state::WorkspaceState,
};
use shoal_forge::{ci::run_view::RunViewRequests, items::action::ActionRequests};
use shoal_protocol::links::{Linked, Unlinked};

impl Manager {
    /// Link the item `input` names with `relation`; an issue or PR linked
    /// without its title gets the one the forge shows, or else none, which
    /// the answer says why.
    pub async fn link_item(
        &self,
        selector: &SpaceSelector,
        kind: LinkKind,
        input: &ItemInput,
        relation: Relation,
        title: Option<String>,
        caller: Option<&Caller>,
    ) -> Result<Linked> {
        let (title, title_error) = match (kind.item(), title) {
            (Some(kind), None) => self.item_title(selector, kind, input, caller).await,
            (_, title) => (title, None),
        };
        let warnings = self
            .record_link(selector, kind, input, relation, title, caller)
            .await?;
        Ok(Linked {
            title_error,
            warnings,
        })
    }

    /// Record the link [`Self::link_item`] makes, with `title`, returning
    /// what assigning an issue on the forge warns of.
    async fn record_link(
        &self,
        selector: &SpaceSelector,
        kind: LinkKind,
        input: &ItemInput,
        relation: Relation,
        title: Option<String>,
        caller: Option<&Caller>,
    ) -> Result<Vec<String>> {
        let assigner = <issue::Assigner as issue::AssignerOf>::of(caller);
        match (relation, kind) {
            (Relation::Assigned, LinkKind::Issue) => {
                return self
                    .links
                    .set_issue(self, selector, input, title, assigner)
                    .await;
            }
            (Relation::Assigned, LinkKind::Pr) => {
                let url = input.clone();
                self.set_pr(selector, Action::Watch { url, title }).await
            }
            (Relation::Assigned, LinkKind::Ci) => bail!(
                "a CI run cannot be assigned, because assigned items drive completion \
                 and cleanup; link it as opened or related"
            ),
            (relation, LinkKind::Ci) => self.record_run(selector, relation, input, caller).await,
            (relation, kind) => {
                self.record_item(selector, relation, kind, input, title)
                    .await
            }
        }?;
        Ok(Vec::new())
    }

    /// The title of the issue or PR `input` names, which lists show for the
    /// space, or else why the forge could not be read.
    async fn item_title(
        &self,
        selector: &SpaceSelector,
        kind: ItemKind,
        input: &ItemInput,
        caller: Option<&Caller>,
    ) -> (Option<String>, Option<String>) {
        let read = async {
            let context = self.links.link_space(self, selector).await?.item_context();
            let forge = self.links.context_forge(self, &context).await?;
            let item = ItemRef::in_repository(&forge, kind.into(), input)?;
            let account = self.links.read_account(caller);
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
        &self,
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
            let warnings = self.unassign(selector, kind, input).await?;
            return Ok(Unlinked { warnings });
        }
        self.forget_items(selector, relation, kind, input).await?;
        Ok(Unlinked::default())
    }

    /// Record an opened or related run named by number or URL, once the
    /// forge shows it, with its workflow as its title; workspace processes
    /// link only runs of their own repository.
    async fn record_run(
        &self,
        selector: &SpaceSelector,
        relation: Relation,
        input: &ItemInput,
        caller: Option<&Caller>,
    ) -> Result<()> {
        let space = self.links.link_space(self, selector).await?;
        // A watch reads the run as the user where it is not the space's own,
        // so linking it is a change there.
        let target = space.item_context().into();
        let linked = self.named_run(&target, input, Access::Change, caller);
        let (place, url) = linked.await?;
        let run = (place.forge().view_run(&place.account, &url).await)
            .with_context(|| format!("could not read CI run {url}"))?;
        let item = LinkedItem {
            url,
            title: run.workflow,
        };
        self.record_items(&space.id, vec![(relation, item)]).await
    }

    /// Record an opened or related item named by number or URL.
    async fn record_item(
        &self,
        selector: &SpaceSelector,
        relation: Relation,
        kind: LinkKind,
        input: &ItemInput,
        title: Option<String>,
    ) -> Result<()> {
        let space = self.links.link_space(self, selector).await?;
        self.verify_space(&space).await?;
        let url = self.space_item(&space, kind, input).await?;
        let item = LinkedItem { url, title };
        self.record_items(&space.id, vec![(relation, item)]).await
    }

    /// Forget a relation's recorded items of `kind`, or the one `input` names,
    /// which must be recorded.
    async fn forget_items(
        &self,
        selector: &SpaceSelector,
        relation: Relation,
        kind: Option<LinkKind>,
        input: Option<&ItemInput>,
    ) -> Result<()> {
        let space = self.links.link_space(self, selector).await?;
        self.verify_space(&space).await?;
        let url = match (kind, input) {
            (Some(kind), Some(input)) => Some(self.space_item(&space, kind, input).await?),
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
    pub(crate) async fn record_items(
        &self,
        id: &SpaceId,
        items: Vec<(Relation, LinkedItem)>,
    ) -> Result<()> {
        let (id, links) = (id.to_owned(), self.links.clone());
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
        &self,
        id: &SpaceId,
        relation: Relation,
        kind: Option<LinkKind>,
        url: Option<ItemRef>,
    ) -> Result<usize> {
        let (id, links) = (id.to_owned(), self.links.clone());
        self.store
            .write(move |tx| links.remove(tx, &id, relation, kind, url.as_ref().map(ItemRef::url)))
            .await
    }

    /// Unlink the assigned items of `kind`, or the one `input` names,
    /// returning why an issue's assignee stays on the forge.
    async fn unassign(
        &self,
        selector: &SpaceSelector,
        kind: Option<ItemKind>,
        input: Option<&ItemInput>,
    ) -> Result<Vec<String>> {
        if input.is_none() {
            self.require_assigned(selector, kind).await?;
        }
        let mut warnings = Vec::new();
        if kind != Some(ItemKind::Pr) {
            warnings = self.links.clear_issue(self, selector, input).await?;
        }
        if kind != Some(ItemKind::Issue) {
            let action = input.map_or(Action::Clear, |url| Action::Unwatch { url: url.clone() });
            self.set_pr(selector, action).await?;
        }
        Ok(warnings)
    }

    /// Fail unless an item of `kind` is assigned: an issue, or a PR watch or
    /// acknowledgement, which an unreadable registration counts as.
    async fn require_assigned(
        &self,
        selector: &SpaceSelector,
        kind: Option<ItemKind>,
    ) -> Result<()> {
        let space = self.links.link_space(self, selector).await?;
        let id = &space.id;
        let issue =
            kind != Some(ItemKind::Pr) && !self.links.issue_registrations(id).await?.is_empty();
        let pr = kind != Some(ItemKind::Issue)
            && !matches!(self.links.pr_registration(id).await, Ok(None));
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
        space: &LinkSpace,
        kind: LinkKind,
        input: &ItemInput,
    ) -> Result<ItemRef> {
        let forge = match kind {
            // A run's URL may name another repository.
            LinkKind::Ci => match shoal_links::forges::of_input(self, input).await? {
                Some(forge) => forge,
                None => shoal_links::forges::of_space(self, space).await?,
            },
            LinkKind::Issue | LinkKind::Pr => {
                let missing = "item lookup needs an origin remote";
                self.guarded_forge(space.origin_path(), missing).await?
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

/// Refuse changes while the workspace is removed or otherwise in transition,
/// so a removal record taken during removal holds its final links; a chat
/// takes them whenever it runs.
pub(crate) fn ensure_accepts_links(db: &Connection, id: &SpaceId) -> Result<()> {
    if space::is_chat(db, id)? {
        return Ok(());
    }
    let state = state(db, id)?;
    ensure!(
        state.accepts_links(),
        "workspace cannot change links while {state}"
    );
    Ok(())
}

/// Whether the workspace still exists and its state takes link changes now,
/// or the chat still runs.
pub(crate) fn accepts_links(db: &Connection, id: &SpaceId) -> Result<bool> {
    if space::is_chat(db, id)? {
        return Ok(true);
    }
    let state: Option<WorkspaceState> = db
        .query_row("SELECT state FROM workspaces WHERE id=?1", [id], |row| {
            row.get(0)
        })
        .optional()?;
    Ok(state.is_some_and(WorkspaceState::accepts_links))
}

fn state(db: &Connection, id: &SpaceId) -> Result<WorkspaceState> {
    Ok(
        db.query_row("SELECT state FROM workspaces WHERE id=?1", [id], |row| {
            row.get(0)
        })?,
    )
}

#[cfg(test)]
mod tests;
