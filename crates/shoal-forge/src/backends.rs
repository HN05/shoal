//! What differs between forges. Each forge implements [`Backend`];
//! [`ForgeKind`] names the one a repository uses and forwards each call to
//! it, so a new forge implements the trait and adds its variant here.
pub mod bitbucket;
pub(crate) mod forgejo;
pub(crate) mod github;
mod github_rest;
pub(crate) mod gitlab;
pub(crate) mod rest;

use anyhow::Result;
use serde_json::Value;

use anyhow::Context;

use crate::{
    ForgeRepo, MERGED_HINT,
    account::Account,
    capability::{Capability, Support},
    items::{
        action::{Edit, MergeMethod, UpdateMethod},
        comment::Found,
        create::{Head, NewIssue, NewPull},
        item::{Current, Item, ItemState},
        link::ItemKind,
        list,
        reaction::Subject,
        revert::NewRevert,
    },
    numbers::{CommentId, IssueNumber, PullNumber},
    repo::locator::ItemRoute,
    review::{
        diff::Diff,
        reviewers::Reviewer,
        status::PrStatus,
        submit::{PlacedReview, Submission},
        thread::Thread,
        view::{Details, Review},
    },
    transport::{api::Request, resend::Transient, response::Response},
    updates::Snapshot,
};

crate::state::states!(
    /// The API a forge host serves.
    ForgeKind: Variants {
        GitHub => "github",
        /// Forgejo, and Gitea, which serves the same API.
        Forgejo => "forgejo",
        GitLab => "gitlab",
        /// Bitbucket Cloud.
        Bitbucket => "bitbucket",
    }
);

impl ForgeKind {
    /// The well-known hosts, where Bitbucket means Bitbucket Cloud.
    pub fn named(host: &str) -> Option<Self> {
        match host {
            "github.com" => Some(Self::GitHub),
            "gitlab.com" => Some(Self::GitLab),
            "bitbucket.org" => Some(Self::Bitbucket),
            _ => None,
        }
    }

    /// What this process learned `host` serves, else the well-known hosts by
    /// name, else `linked`, the forge whose routes a link of the host uses;
    /// failing all three a host whose name has a `gitlab` label counts as
    /// GitLab and any other as Forgejo or Gitea.
    pub(crate) fn detect(host: &str, linked: Option<Self>) -> Self {
        crate::hosts::learned(host)
            .or_else(|| Self::named(host))
            .or(linked)
            .unwrap_or_else(|| {
                if host.split(['.', ':']).any(|label| label == "gitlab") {
                    Self::GitLab
                } else {
                    Self::Forgejo
                }
            })
    }

    /// Whether a token for the forge signs in together with the email of
    /// the account it belongs to.
    pub fn uses_account_email(self) -> bool {
        Backend::uses_account_email(&self)
    }

    /// The username Git sends over HTTPS with a token as the password.
    pub fn git_username(self) -> &'static str {
        Backend::git_username(&self)
    }
}

/// Run `$body` with `$backend` bound to the backend `$kind` names. Area
/// modules below this one forward their traits' methods through it.
macro_rules! dispatch {
    ($kind:expr, $backend:ident => $body:expr) => {
        match $kind {
            $crate::backends::ForgeKind::GitHub => {
                let $backend = $crate::backends::github::GitHub;
                $body
            }
            $crate::backends::ForgeKind::Forgejo => {
                let $backend = $crate::backends::forgejo::Forgejo;
                $body
            }
            $crate::backends::ForgeKind::GitLab => {
                let $backend = $crate::backends::gitlab::GitLab;
                $body
            }
            $crate::backends::ForgeKind::Bitbucket => {
                let $backend = $crate::backends::bitbucket::Bitbucket;
                $body
            }
        }
    };
}

mod area;
pub(crate) use area::{Attachments, Checks, Conventions, Reactions, Runners, Runs};

/// What a forge does its own way. Methods that need the repository take it
/// as `repo`.
#[shoal_forge_derive::forwarded(ForgeKind, dispatch)]
pub(crate) trait Backend {
    /// Whether `host`, a repository without a path, answers this forge's
    /// version or metadata endpoint as `account`; an error when it could
    /// not be asked.
    async fn serves(&self, host: &ForgeRepo, account: &Account) -> Result<bool>;

    /// The forge's name in messages.
    fn name(&self) -> &'static str;

    /// Whether the forge has `capability`.
    fn support(&self, capability: Capability) -> Support;

    /// Whether the server `repo` is on has `capability`, where the forge's
    /// versions differ in it.
    async fn server_support(
        &self,
        _repo: &ForgeRepo,
        _account: &Account,
        capability: Capability,
    ) -> Result<Support> {
        Ok(self.support(capability))
    }

    /// Whether a repository path may have more than `owner/repository`.
    fn nests_groups(&self) -> bool;

    fn issue_route(&self) -> ItemRoute;

    fn pull_route(&self) -> ItemRoute;

    /// What precedes a branch's name in the forge's branch URLs.
    fn branch_route(&self) -> &'static str;

    /// Whether `url` uses one of the forge's issue, PR or branch routes.
    fn links_item(&self, url: &str) -> bool {
        [
            self.issue_route().marker(),
            self.pull_route().marker(),
            self.branch_route(),
        ]
        .iter()
        .any(|route| url.contains(route))
    }

    /// The API endpoint for `path` under the repository.
    fn repo_endpoint(&self, repo: &ForgeRepo, path: &str) -> String;

    /// The REST collection PRs are under.
    fn pull_resource(&self) -> &'static str;

    /// The field that holds an issue's or PR's number in the repository.
    fn number_field(&self) -> &'static str;

    /// The field that holds a commit's full ID in a list of PR commits.
    fn commit_field(&self) -> &'static str;

    /// The ref a remote of the repository keeps PR `number`'s head
    /// commit under, where the forge keeps one after its branch goes.
    fn pull_head_ref(&self, number: PullNumber) -> Option<String>;

    /// What a failed read as the user most likely needs.
    fn login_hint(&self) -> &'static str;

    /// Whether a token signs in together with the email of the account it
    /// belongs to.
    fn uses_account_email(&self) -> bool {
        false
    }

    /// The Authorization header value that signs a request in with
    /// `token`, which belongs to `email` where the forge needs it.
    fn authorization(&self, token: &str, _email: Option<&str>) -> String {
        format!("Bearer {token}")
    }

    /// The username Git sends over HTTPS with a token as the password.
    fn git_username(&self) -> &'static str;

    /// Send `request` as `account` and return the forge's response, which
    /// may report a failure.
    async fn send(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        request: &Request,
    ) -> Result<Response>;

    /// A failure the forge reports in the body of a successful response.
    fn reported_failure(&self, _body: &Value) -> Option<String> {
        None
    }

    /// The endpoint for a read of `endpoint` a caller wrote, such as
    /// through `shoal forge api`, so the forge applies the page size its
    /// query asks for.
    fn raw_read(&self, endpoint: String) -> String {
        endpoint
    }

    /// The query parameter that sizes a list page, and the size to ask for.
    fn page_size(&self) -> (&'static str, usize);

    /// A list page's items, and whether another page follows.
    fn page<'a>(&self, response: &'a Value, size: usize) -> Option<(&'a [Value], bool)>;

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

    /// Fail closed on changed output: only commits the merged PR lists can
    /// authorize cleanup, including squash and rebase merges. None while the
    /// PR is unmerged.
    async fn merged_commits(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        number: PullNumber,
    ) -> Result<Option<Vec<String>>> {
        rest::merged_commits(repo, account, number)
            .await
            .context(MERGED_HINT.trim_start_matches("; ").to_owned())
    }

    /// How many commits the base branch of PR `number` has that its head
    /// lacks; none once it is closed, or where the forge cannot tell.
    async fn behind(
        &self,
        _repo: &ForgeRepo,
        _account: &Account,
        _number: PullNumber,
    ) -> Result<Option<u64>> {
        Ok(None)
    }

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

    /// The requests that merge the open PR `current`.
    fn merge(
        &self,
        repo: &ForgeRepo,
        current: &Current,
        method: MergeMethod,
        delete_branch: bool,
    ) -> Result<Vec<Request>>;

    /// The requests that have the forge merge the open PR `current` once its
    /// required checks and approvals pass.
    fn auto_merge(
        &self,
        repo: &ForgeRepo,
        current: &Current,
        method: MergeMethod,
        delete_branch: bool,
    ) -> Result<Vec<Request>>;

    /// The requests that withdraw the automatic merge of the open PR `current`.
    fn cancel_auto_merge(&self, repo: &ForgeRepo, current: &Current) -> Result<Vec<Request>>;

    /// The requests that bring the branch of the open PR `current` up to
    /// date with its base by `method`.
    fn update_branch(
        &self,
        repo: &ForgeRepo,
        current: &Current,
        method: UpdateMethod,
    ) -> Result<Vec<Request>>;

    /// Have the forge open `revert` for the merged PR `current` as
    /// `account`, returning the new PR's number and head branch.
    async fn revert(
        &self,
        _repo: &ForgeRepo,
        _account: &Account,
        _current: &Current,
        _revert: &NewRevert,
    ) -> Result<(PullNumber, String)> {
        Err(crate::capability::unsupported(self, Capability::RevertPrs))
    }

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

    /// The request that deletes `branch` from the repository.
    fn branch_delete(&self, repo: &ForgeRepo, branch: &str) -> Request;

    /// Whether `account` may push branches to the repository.
    async fn pushable(&self, repo: &ForgeRepo, account: &Account) -> Result<bool>;

    /// `owner/repository` of `account`'s fork of the repository, which the
    /// forge creates when the account has none.
    async fn fork(&self, repo: &ForgeRepo, account: &Account) -> Result<String>;

    /// The users and teams the repository's PRs can ask for a review.
    async fn reviewers(&self, repo: &ForgeRepo, account: &Account) -> Result<Vec<Reviewer>>;

    /// The request that opens `pull`.
    fn new_pull(&self, repo: &ForgeRepo, pull: &NewPull) -> Request;

    /// The request that opens `issue`.
    fn new_issue(&self, repo: &ForgeRepo, issue: &NewIssue) -> Request;

    /// Whether a new issue's labels are added after opening it, because
    /// opening it cannot set them by name.
    fn labels_issue_after_opening(&self) -> bool;

    /// The review thread of `comment` on PR `number`, when it is in one.
    async fn thread(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        number: PullNumber,
        comment: CommentId,
    ) -> Result<Option<Thread>>;

    /// The request that replies in `thread` on PR `number`.
    fn reply(
        &self,
        repo: &ForgeRepo,
        number: PullNumber,
        thread: &Thread,
        body: &str,
    ) -> Result<Request>;

    /// The request that resolves or unresolves `thread` on PR `number`.
    fn resolution(
        &self,
        repo: &ForgeRepo,
        number: PullNumber,
        thread: &Thread,
        resolved: bool,
    ) -> Result<Request>;

    /// The diff of the PR `current`, at its head commit where the forge
    /// reads a diff there.
    async fn pr_diff(&self, repo: &ForgeRepo, account: &Account, current: &Current)
    -> Result<Diff>;

    /// Refuse a review of PR `number` as `account` that submitting would
    /// change more than the review on the forge.
    async fn check_review(
        &self,
        _repo: &ForgeRepo,
        _account: &Account,
        _number: PullNumber,
    ) -> Result<()> {
        Ok(())
    }

    /// The requests that submit `review` of PR `number`.
    fn review(
        &self,
        repo: &ForgeRepo,
        number: PullNumber,
        review: &PlacedReview,
    ) -> Result<Submission>;

    /// The request that deletes the review draft a draft request's
    /// response describes, where the forge keeps drafts.
    fn discard_draft(
        &self,
        _repo: &ForgeRepo,
        _number: PullNumber,
        _draft: &Value,
    ) -> Result<Request> {
        anyhow::bail!("{} keeps no review drafts", self.name())
    }

    /// The activity keys of what `review` of PR `number` posted, from the
    /// `responses` to its submission's requests: by default each response
    /// describes a posted comment.
    async fn review_keys(
        &self,
        _repo: &ForgeRepo,
        _account: &Account,
        _number: PullNumber,
        _review: &PlacedReview<'_>,
        responses: &[Value],
    ) -> Result<Vec<String>> {
        let key = self.comment_key();
        Ok(responses
            .iter()
            .filter_map(|response| crate::updates::posted_key(key, response))
            .collect())
    }

    /// An issue's state and discussion.
    async fn issue_activity(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        number: IssueNumber,
    ) -> Result<Snapshot>;

    /// A PR's state, discussion, reviews, mergeability and checks.
    async fn pr_activity(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        number: PullNumber,
    ) -> Result<Snapshot>;

    /// A PR's state, mergeability, checks and review state.
    async fn pr_status(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        number: PullNumber,
        url: &str,
    ) -> Result<PrStatus>;

    /// The item's content for a view, with its comments when `comments` is
    /// set and the forge reads them with the item.
    async fn view_details(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        kind: ItemKind,
        number: u64,
        comments: bool,
    ) -> Result<Details>;

    /// Fills the comments `details` lacks and returns a PR's reviews,
    /// recording in `errors` the parts it could not read.
    async fn view_discussion(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        kind: ItemKind,
        number: u64,
        details: &mut Details,
        errors: &mut Vec<String>,
    ) -> Result<Option<Vec<Review>>>;

    /// The login `account` signs in to the forge as; none when it reads
    /// anonymously.
    async fn own_login(&self, repo: &ForgeRepo, account: &Account) -> Result<Option<String>>;

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

    /// The default branch a read of the repository's own endpoint returned
    /// as `value`.
    fn default_branch<'a>(&self, value: &'a Value) -> Option<&'a str> {
        value["default_branch"].as_str()
    }

    /// The repository's numeric ID, where the forge's uploads name it.
    async fn repository_id(&self, _repo: &ForgeRepo, _account: &Account) -> Result<Option<u64>> {
        Ok(None)
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

/// Adds a backend's login hint to a failed read.
pub(crate) trait LoginHint<T> {
    /// This result, with `backend`'s login hint on a failure a login may
    /// fix: not one that may pass, such as a refused connection.
    fn hint_login(self, backend: &(impl Backend + ?Sized)) -> Result<T>;
}

impl<T> LoginHint<T> for Result<T> {
    fn hint_login(self, backend: &(impl Backend + ?Sized)) -> Result<T> {
        self.map_err(|error| match error.downcast_ref::<Transient>() {
            Some(_) => error,
            None => error.context(backend.login_hint()),
        })
    }
}

#[cfg(test)]
mod tests;
