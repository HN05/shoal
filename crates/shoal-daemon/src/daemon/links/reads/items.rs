//! The issues and PRs a read names: which items a selection names and what
//! kind a bare number is, an issue's details and blockers, and a PR's head,
//! reviewers and review threads.
use anyhow::{Context, Result};

use super::ReadPlace;
use crate::{
    daemon::{
        links::{actions::NOTHING_LINKED, space::LinkSpace},
        scope::Caller,
        workspace::Manager,
    },
    forge::{
        PullRequest,
        account::Account,
        capability::Capability,
        items::{
            issue::IssueDetails,
            link::{ItemKind, LinkKind, Selection},
            reference::{ItemInput, ItemRef},
        },
        numbers::CommentId,
        review::{reviewers::Reviewer, thread::Thread},
    },
    model::{Blocker, Repository, Selected, SelectedItem},
    protocol::ItemContext,
};

impl Manager {
    /// The items `selection` names in `context`: in a space, where it may
    /// name linked ones, or in a registered checkout, where only a number or
    /// URL selects one and a number without a kind takes the one the forge
    /// reports for it. With `elsewhere`, an issue, PR or CI run URL of
    /// another repository selects that item.
    pub async fn select_context_items(
        &self,
        context: &ItemContext,
        selection: &Selection,
        elsewhere: bool,
        caller: Option<&Caller>,
    ) -> Result<Selected> {
        match context {
            ItemContext::Workspace(_) | ItemContext::Chat(_) | ItemContext::ChatIn { .. } => {
                let space = self.context_space(context).await?;
                self.select_items(&space, selection, elsewhere).await
            }
            ItemContext::Checkout(selector) => {
                let checkout = self.repository(selector).await?;
                let account = self.read_account(caller);
                let item = (self.checkout_item(&checkout, selection, elsewhere, &account)).await?;
                Ok(Selected {
                    path: checkout.path,
                    items: vec![item],
                })
            }
        }
    }

    /// The item `selection` names in the repository of the registered
    /// checkout, where nothing is linked, or with `elsewhere` the item of
    /// another repository its URL names. A number without a kind takes the
    /// one the forge reports for it.
    pub(in crate::daemon::links) async fn checkout_item(
        &self,
        checkout: &Repository,
        selection: &Selection,
        elsewhere: bool,
        account: &Account,
    ) -> Result<SelectedItem> {
        let input = selection.input.as_ref().context(NOTHING_LINKED)?;
        // A URL names its item in any repository, so the checkout's origin
        // need not decide.
        if elsewhere && let Some((_, item)) = self.item_at_url(selection).await? {
            return Ok(item);
        }
        let forge = self.checkout_forge(checkout).await?;
        let kind = match (selection.kind, input) {
            (Some(kind), _) => kind,
            (None, ItemInput::Number(number)) => forge.item_kind(account, *number).await?.into(),
            (None, ItemInput::Url(item)) => item.kind(),
        };
        Ok(SelectedItem {
            url: ItemRef::in_repository(&forge, kind, input)?,
            relations: Vec::new(),
        })
    }

    /// The kind of the item numbered `number` in `context`: in a space,
    /// that of the linked item with that number, otherwise the kind the
    /// forge reports for it.
    pub async fn number_kind(
        &self,
        context: &ItemContext,
        number: u64,
        caller: Option<&Caller>,
    ) -> Result<LinkKind> {
        let account = self.read_account(caller);
        match self.read_place(context).await? {
            ReadPlace::Space(space) => self.space_number_kind(&space, number, &account).await,
            ReadPlace::Checkout(checkout) => {
                let forge = self.checkout_forge(&checkout).await?;
                Ok(forge.item_kind(&account, number).await?.into())
            }
        }
    }

    /// The kind of the item numbered `number` linked to `space` under any
    /// relation, otherwise the kind the forge reports for it.
    pub(in crate::daemon::links) async fn space_number_kind(
        &self,
        space: &LinkSpace,
        number: u64,
        account: &Account,
    ) -> Result<LinkKind> {
        match linked_kind(space, number).await? {
            Some(kind) => Ok(kind),
            None => Ok((self.space_forge(space).await?)
                .item_kind(account, number)
                .await?
                .into()),
        }
    }

    /// The open issue `item` names in the repository of `context`, by
    /// number or URL, or with `closed` one in any state.
    pub async fn issue_details(
        &self,
        context: &ItemContext,
        item: &ItemInput,
        closed: bool,
        caller: Option<&Caller>,
    ) -> Result<IssueDetails> {
        let forge = self.context_forge(context).await?;
        let issue = ItemRef::in_repository(&forge, LinkKind::Issue, item)?;
        forge
            .read_issue(&self.read_account(caller), &issue, closed)
            .await
    }

    /// The open issues the forge records as blocking the issue `item` names
    /// in the repository of `context`, by number or URL.
    pub async fn issue_blockers(
        &self,
        context: &ItemContext,
        item: &ItemInput,
        caller: Option<&Caller>,
    ) -> Result<Vec<Blocker>> {
        let forge = self.context_forge(context).await?;
        let issue = ItemRef::in_repository(&forge, LinkKind::Issue, item)?;
        let account = self.read_account(caller);
        let blockers = forge.open_issue_blockers(&account, issue.issue()?).await?;
        Ok((blockers.into_iter())
            .map(|issue| Blocker::Issue(issue.into()))
            .collect())
    }

    /// The PR `item` names in the repository of `context`, with where its
    /// head is.
    pub async fn pull_request(
        &self,
        context: &ItemContext,
        item: &ItemInput,
        caller: Option<&Caller>,
    ) -> Result<PullRequest> {
        let forge = self.context_forge(context).await?;
        let pull = ItemRef::in_repository(&forge, LinkKind::Pr, item)?;
        forge.pull_request(&self.read_account(caller), &pull).await
    }

    /// Who the PR `item` names in the repository of `context`, or the
    /// space's linked PR, can ask for a review.
    pub async fn reviewers(
        &self,
        context: &ItemContext,
        item: Option<ItemInput>,
        caller: Option<&Caller>,
    ) -> Result<Vec<Reviewer>> {
        let place = self.read_place(context).await?;
        let pull = self.place_item(item, &place, ItemKind::Pr).await?;
        let forge = self.read_place_forge(&place).await?;
        forge
            .reviewers(&self.read_account(caller), pull.pull()?)
            .await
    }

    /// The review thread of `comment` on the PR `item` names in the
    /// repository of `context`, or on the space's linked PR, refused where
    /// the forge cannot resolve threads.
    pub async fn review_thread(
        &self,
        context: &ItemContext,
        item: Option<ItemInput>,
        comment: CommentId,
        caller: Option<&Caller>,
    ) -> Result<Thread> {
        let place = self.read_place(context).await?;
        let pull = self.place_item(item, &place, ItemKind::Pr).await?;
        let forge = self.read_place_forge(&place).await?;
        forge.require(Capability::ResolveThreads)?;
        forge
            .thread(&self.read_account(caller), pull.pull()?, comment)
            .await
    }
}

/// The kind of the item numbered `number` linked to `space` under any
/// relation.
async fn linked_kind(space: &LinkSpace, number: u64) -> Result<Option<LinkKind>> {
    let linked = Selected {
        path: space.origin_path().to_owned(),
        items: (space.links().items().into_iter())
            .map(|item| SelectedItem {
                url: item.url,
                relations: Vec::new(),
            })
            .collect(),
    };
    // A number names an item of the space's repository; issues opened in
    // another one share its numbers.
    let remote = crate::repository::remote_url_from_path(space.origin_path())
        .await?
        .context("item lookup needs an origin remote")?;
    linked.kind_of(&remote, number)
}
