//! What each forge does its own way with issues and PRs: reading, listing,
//! opening, editing and commenting on them, and finding a PR by its head.
use anyhow::{Context, Result};
use serde_json::Value;

use crate::{
    ForgeRepo,
    account::Account,
    backends::{Backend, ForgeKind, LoginHint, rest},
    items::{
        action::Edit,
        comment::Found,
        create::{Head, NewIssue, NewPull},
        item::{Current, Item, ItemState},
        link::ItemKind,
        list,
        reaction::Subject,
    },
    numbers::{IssueNumber, PullNumber},
    transport::api::Request,
};

/// How the forge reads, lists, opens, edits and comments on issues
/// and PRs, and finds a PR by its head.
#[shoal_forge_derive::forwarded(ForgeKind, dispatch)]
pub(crate) trait Items: Backend {
    /// The item an item endpoint returned as `value`; an issue read that
    /// returns a PR is refused.
    fn current(
        &self,
        repo: &ForgeRepo,
        kind: ItemKind,
        number: u64,
        value: &Value,
    ) -> Result<Current>;

    /// Whether issues and PRs share one sequence, so a number names one item.
    fn numbers_items_together(&self) -> bool;

    /// Issues or PRs that `search` matches, newest first, through the
    /// forge's search where it has one: the first `search.limit` matches at
    /// least. The caller drops items beyond the limit and those in a state
    /// the search leaves out.
    async fn list_items(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        kind: ItemKind,
        search: &list::Search,
    ) -> Result<Vec<Item>>;

    /// An issue's state; a PR's number is refused.
    async fn issue_state(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        number: IssueNumber,
    ) -> Result<ItemState> {
        rest::issue_state(repo, account, number).await
    }

    /// An open issue.
    async fn issue_details(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        number: IssueNumber,
    ) -> Result<Current> {
        rest::issue_details(repo, account, number).await
    }

    /// The URLs of the open issues the forge records as blocking an issue.
    /// A forge that records no blocking issues lists none.
    async fn open_issue_blockers(
        &self,
        _repo: &ForgeRepo,
        _account: &Account,
        _number: IssueNumber,
    ) -> Result<Vec<String>> {
        Ok(Vec::new())
    }

    /// The PR `number`, for checking out its head.
    async fn pull_details(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        number: PullNumber,
    ) -> Result<Current> {
        repo.read(account, ItemKind::Pr, number.get())
            .await
            .hint_login(self)
    }

    /// The ref a remote of the repository keeps PR `number`'s head
    /// commit under, where the forge keeps one after its branch goes.
    fn pull_head_ref(&self, number: PullNumber) -> Option<String>;

    /// Read what `edit` needs beyond `current` before it is planned.
    async fn prepare_edit(
        &self,
        _repo: &ForgeRepo,
        _account: &Account,
        _edit: &Edit,
        _current: &mut Current,
    ) -> Result<()> {
        Ok(())
    }

    /// The requests that change `current` as `edit` says, in order.
    fn edit(&self, repo: &ForgeRepo, current: &Current, edit: &Edit) -> Result<Vec<Request>>;

    /// Post `body` as a comment on the item.
    fn comment(&self, repo: &ForgeRepo, item: &Item, body: &str) -> Request;

    /// Close or reopen the item.
    fn set_state(&self, repo: &ForgeRepo, item: &Item, state: ItemState) -> Result<Request>;

    /// Where activity records a posted comment: its source, and the
    /// response field that holds its ID.
    fn comment_key(&self) -> (&'static str, &'static str);

    /// Where activity records a reply in a review thread.
    fn reply_key(&self) -> (&'static str, &'static str);

    /// The open PRs that may have `head` as their head, filtered as far
    /// as the forge can.
    async fn open_pulls_from(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        head: &Head<'_>,
    ) -> Result<Vec<Value>>;

    /// Whether a listed PR's head is `head`.
    fn has_head(&self, repo: &ForgeRepo, pull: &Value, head: &Head<'_>) -> bool;

    /// The request that reads `branch`, and the JSON pointer to its commit
    /// in the response.
    fn branch_read(&self, repo: &ForgeRepo, branch: &str) -> (Request, &'static str);

    /// The request that opens `pull`.
    fn new_pull(&self, repo: &ForgeRepo, pull: &NewPull) -> Request;

    /// The request that opens `issue`.
    fn new_issue(&self, repo: &ForgeRepo, issue: &NewIssue) -> Request;

    /// Whether a new issue's labels are added after opening it, because
    /// opening it cannot set them by name.
    fn labels_issue_after_opening(&self) -> bool;

    /// Whether the login of `account` wrote `comment` of item `number`.
    async fn wrote(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        _kind: ItemKind,
        _number: u64,
        comment: &Found,
    ) -> Result<bool> {
        let login = (self.own_login(repo, account).await?)
            .context("editing a comment needs a login on the forge")?;
        Ok(comment.author.eq_ignore_ascii_case(&login))
    }

    /// The request that replaces the body of the comment `subject` of item
    /// `number`.
    fn edit_comment(
        &self,
        repo: &ForgeRepo,
        kind: ItemKind,
        number: u64,
        subject: Subject,
        body: &str,
    ) -> Result<Request>;
}
