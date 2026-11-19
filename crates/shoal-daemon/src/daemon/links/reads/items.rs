//! The issues and PRs a read names: which items a selection names and what
//! kind a bare number is, an issue's details and blockers, and a PR's head,
//! reviewers and review threads.
use anyhow::{Context, Result};

use crate::{
    daemon::{
        links::{
            actions::NOTHING_LINKED,
            place::{Access, Place},
            space::LinkSpace,
            target::ForgeTarget,
        },
        manager::Manager,
        scope::Caller,
    },
    forge::{
        ForgeRepo, PullRequest,
        capability::Capability,
        items::{
            issue::IssueDetails,
            link::{ItemKind, LinkKind, Selection},
            reference::{ItemInput, ItemRef},
        },
        numbers::CommentId,
        review::{reviewers::Reviewer, thread::Thread},
    },
    model::{Blocker, Selected, SelectedItem},
    protocol::{ItemContext, RepoTarget},
};
use shoal_forge::{
    RepoRequests,
    account::Account,
    capability::CapabilityRequests,
    items::{action::ActionRequests, issue::IssueRequests},
    review::{reviewers::ReviewerRequests, thread::ThreadRequests},
};

impl Manager {
    /// The items `selection` names in the `target` repository: in a space's
    /// own, where it may name linked ones, or elsewhere, where only a number
    /// or URL selects one and a number without a kind takes the one the
    /// forge reports for it. With `elsewhere`, an issue, PR or CI run URL of
    /// another repository selects that item.
    pub async fn select_context_items(
        &self,
        target: &RepoTarget,
        selection: &Selection,
        elsewhere: bool,
        caller: Option<&Caller>,
    ) -> Result<Selected> {
        let account = self.read_account(caller);
        let target = (self.selection_target(target, selection, elsewhere, caller)).await?;
        let items = (self.target_selection(&target, selection, elsewhere, &account)).await?;
        let path = (target.origin.as_ref()).map(|place| place.origin_path().to_owned());
        Ok(Selected { path, items })
    }

    /// The repository `target` names for reading what `selection` names,
    /// which with `elsewhere` may be another repository by its URL.
    pub(in crate::daemon::links) async fn selection_target(
        &self,
        target: &RepoTarget,
        selection: &Selection,
        elsewhere: bool,
        caller: Option<&Caller>,
    ) -> Result<ForgeTarget> {
        let named = match (&selection.input, elsewhere) {
            (Some(input), true) => self.input_forge(input).await?,
            _ => None,
        };
        (self.forge_target(target, named, Access::Read, caller)).await
    }

    /// The items `selection` names in `target`: in a space's own
    /// repository, where it may name linked ones, otherwise the one its
    /// number or URL names.
    pub(in crate::daemon::links) async fn target_selection(
        &self,
        target: &ForgeTarget,
        selection: &Selection,
        elsewhere: bool,
        account: &Account,
    ) -> Result<Vec<SelectedItem>> {
        match target.own_place().and_then(Place::space) {
            Some(space) => Ok(self.select_items(space, selection, elsewhere).await?.items),
            None => Ok(vec![
                self.unlinked_item(target.forge()?, selection, account)
                    .await?,
            ]),
        }
    }

    /// The item `selection` names by number or URL in `forge`, where nothing
    /// is linked. A number without a kind takes the one the forge reports
    /// for it.
    async fn unlinked_item(
        &self,
        forge: &ForgeRepo,
        selection: &Selection,
        account: &Account,
    ) -> Result<SelectedItem> {
        let input = selection.input.as_ref().context(NOTHING_LINKED)?;
        let kind = match (selection.kind, input) {
            (Some(kind), _) => kind,
            (None, ItemInput::Number(number)) => forge.item_kind(account, *number).await?.into(),
            (None, ItemInput::Url(item)) => item.kind(),
        };
        Ok(SelectedItem {
            url: ItemRef::in_repository(forge, kind, input)?,
            relations: Vec::new(),
        })
    }

    /// The kind of the item numbered `number` in the `target` repository:
    /// in a space's own, that of the linked item with that number,
    /// otherwise the kind the forge reports for it.
    pub async fn number_kind(
        &self,
        target: &RepoTarget,
        number: u64,
        caller: Option<&Caller>,
    ) -> Result<LinkKind> {
        let account = self.read_account(caller);
        let target = (self.forge_target(target, None, Access::Read, caller)).await?;
        match target.own_place().and_then(Place::space) {
            Some(space) => self.space_number_kind(space, number, &account).await,
            None => Ok(target.forge()?.item_kind(&account, number).await?.into()),
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

    /// Who the PR `item` names in the `target` repository, or the space's
    /// linked PR, can ask for a review.
    pub async fn reviewers(
        &self,
        target: &RepoTarget,
        item: Option<ItemInput>,
        caller: Option<&Caller>,
    ) -> Result<Vec<Reviewer>> {
        let read = self.target_item(target, item, ItemKind::Pr, Access::Read, caller);
        let (target, pull) = read.await?;
        let forge = target.forge()?;
        forge
            .reviewers(&self.read_account(caller), pull.pull()?)
            .await
    }

    /// The review thread of `comment` on the PR `item` names in the
    /// `target` repository, or on the space's linked PR, refused where the
    /// forge cannot resolve threads.
    pub async fn review_thread(
        &self,
        target: &RepoTarget,
        item: Option<ItemInput>,
        comment: CommentId,
        caller: Option<&Caller>,
    ) -> Result<Thread> {
        let read = self.target_item(target, item, ItemKind::Pr, Access::Read, caller);
        let (target, pull) = read.await?;
        let forge = target.forge()?;
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
        path: Some(space.origin_path().to_owned()),
        items: (space.links.items().into_iter())
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
