//! What differs between forges. Each forge implements [`Backend`];
//! [`ForgeKind`] names the one a repository uses and forwards each call to
//! it, so a new forge implements the trait and adds its variant here.
pub mod bitbucket;
mod forgejo;
pub(crate) mod github;
mod github_rest;
pub(crate) mod gitlab;
pub(crate) mod glab;
pub(crate) mod rest;
pub(crate) mod tea;

use anyhow::Result;
use serde_json::Value;

use anyhow::Context;

use crate::{
    ForgeRepo, MERGED_HINT,
    account::Account,
    items::{
        action::{Edit, MergeMethod},
        item::{Current, Item, ItemState},
        link::ItemKind,
        list,
    },
    repo::locator::ItemRoute,
    transport::api::Request,
};
use bitbucket::Bitbucket;
use forgejo::Forgejo;
use github::GitHub;
use gitlab::GitLab;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ForgeKind {
    GitHub,
    /// Forgejo, and Gitea, which serves the same API.
    Forgejo,
    GitLab,
    /// Bitbucket Cloud.
    Bitbucket,
}

impl ForgeKind {
    /// Well-known hosts by name, where Bitbucket means Bitbucket Cloud; a host glab has a login for, or whose name
    /// has a `gitlab` label, as GitLab; any other as Forgejo or Gitea.
    pub fn detect(host: &str) -> Self {
        match host {
            "github.com" => Self::GitHub,
            "gitlab.com" => Self::GitLab,
            "bitbucket.org" => Self::Bitbucket,
            _ if host.split(['.', ':']).any(|label| label == "gitlab") || glab::has_login(host) => {
                Self::GitLab
            }
            _ => Self::Forgejo,
        }
    }
}

/// What a forge does its own way. Methods that need the repository take it
/// as `repo`.
pub(crate) trait Backend {
    /// Whether a repository path may have more than `owner/repository`.
    fn nests_groups(&self) -> bool;

    fn issue_route(&self) -> ItemRoute;

    fn pull_route(&self) -> ItemRoute;

    /// The API endpoint for `path` under the repository.
    fn repo_endpoint(&self, repo: &ForgeRepo, path: &str) -> String;

    /// The REST collection PRs are under.
    fn pull_resource(&self) -> &'static str;

    /// The field that holds an issue's or PR's number in the repository.
    fn number_field(&self) -> &'static str;

    /// The field that holds a commit's full ID in a list of PR commits.
    fn commit_field(&self) -> &'static str;

    /// What a failed read as the user most likely needs.
    fn login_hint(&self) -> &'static str;

    /// Send `request` as `account` and return the response body, or null
    /// for an empty one.
    async fn send(&self, repo: &ForgeRepo, account: &Account, request: &Request) -> Result<Value>;

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

    /// The repository's open issues or PRs.
    async fn open_items(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        kind: ItemKind,
    ) -> Result<Vec<list::Item>>;

    /// An issue's state; a PR's number is refused.
    async fn issue_state(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        number: u64,
    ) -> Result<ItemState> {
        rest::issue_state(repo, account, number).await
    }

    /// An open issue's title and description.
    async fn issue_details(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        number: u64,
    ) -> Result<(String, String)> {
        rest::issue_details(repo, account, number).await
    }

    /// Title, head and base of a PR whose head branch is in the repository.
    async fn pull_details(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        number: u64,
    ) -> Result<(String, String, String)> {
        rest::pull_details(repo, account, number)
            .await
            .context(self.login_hint())
    }

    /// Fail closed on changed output: only commits the merged PR lists can
    /// authorize cleanup, including squash and rebase merges. None while the
    /// PR, which must have `branch` as its head, is unmerged.
    async fn merged_commits(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        number: u64,
        branch: &str,
    ) -> Result<Option<Vec<String>>> {
        rest::merged_commits(repo, account, number, branch)
            .await
            .context(MERGED_HINT.trim_start_matches("; ").to_owned())
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

    /// Where activity records a posted comment: its source, and the
    /// response field that holds its ID.
    fn comment_key(&self) -> (&'static str, &'static str);

    /// Where activity records a reply in a review thread.
    fn reply_key(&self) -> (&'static str, &'static str);
}

/// Run `$body` with `$backend` bound to the backend `$kind` names.
macro_rules! dispatch {
    ($kind:expr, $backend:ident => $body:expr) => {
        match $kind {
            ForgeKind::GitHub => {
                let $backend = GitHub;
                $body
            }
            ForgeKind::Forgejo => {
                let $backend = Forgejo;
                $body
            }
            ForgeKind::GitLab => {
                let $backend = GitLab;
                $body
            }
            ForgeKind::Bitbucket => {
                let $backend = Bitbucket;
                $body
            }
        }
    };
}

impl Backend for ForgeKind {
    fn nests_groups(&self) -> bool {
        dispatch!(self, backend => backend.nests_groups())
    }

    fn issue_route(&self) -> ItemRoute {
        dispatch!(self, backend => backend.issue_route())
    }

    fn pull_route(&self) -> ItemRoute {
        dispatch!(self, backend => backend.pull_route())
    }

    fn repo_endpoint(&self, repo: &ForgeRepo, path: &str) -> String {
        dispatch!(self, backend => backend.repo_endpoint(repo, path))
    }

    fn pull_resource(&self) -> &'static str {
        dispatch!(self, backend => backend.pull_resource())
    }

    fn number_field(&self) -> &'static str {
        dispatch!(self, backend => backend.number_field())
    }

    fn commit_field(&self) -> &'static str {
        dispatch!(self, backend => backend.commit_field())
    }

    fn login_hint(&self) -> &'static str {
        dispatch!(self, backend => backend.login_hint())
    }

    async fn send(&self, repo: &ForgeRepo, account: &Account, request: &Request) -> Result<Value> {
        dispatch!(self, backend => backend.send(repo, account, request).await)
    }

    fn page_size(&self) -> (&'static str, usize) {
        dispatch!(self, backend => backend.page_size())
    }

    fn page<'a>(&self, response: &'a Value, size: usize) -> Option<(&'a [Value], bool)> {
        dispatch!(self, backend => backend.page(response, size))
    }

    fn current(
        &self,
        repo: &ForgeRepo,
        kind: ItemKind,
        number: u64,
        value: &Value,
    ) -> Result<Current> {
        dispatch!(self, backend => backend.current(repo, kind, number, value))
    }

    fn numbers_items_together(&self) -> bool {
        dispatch!(self, backend => backend.numbers_items_together())
    }

    async fn open_items(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        kind: ItemKind,
    ) -> Result<Vec<list::Item>> {
        dispatch!(self, backend => backend.open_items(repo, account, kind).await)
    }

    async fn issue_state(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        number: u64,
    ) -> Result<ItemState> {
        dispatch!(self, backend => backend.issue_state(repo, account, number).await)
    }

    async fn issue_details(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        number: u64,
    ) -> Result<(String, String)> {
        dispatch!(self, backend => backend.issue_details(repo, account, number).await)
    }

    async fn pull_details(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        number: u64,
    ) -> Result<(String, String, String)> {
        dispatch!(self, backend => backend.pull_details(repo, account, number).await)
    }

    async fn merged_commits(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        number: u64,
        branch: &str,
    ) -> Result<Option<Vec<String>>> {
        dispatch!(self, backend => backend.merged_commits(repo, account, number, branch).await)
    }
    async fn prepare_edit(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        edit: &Edit,
        current: &mut Current,
    ) -> Result<()> {
        dispatch!(self, backend => backend.prepare_edit(repo, account, edit, current).await)
    }

    fn edit(&self, repo: &ForgeRepo, current: &Current, edit: &Edit) -> Result<Vec<Request>> {
        dispatch!(self, backend => backend.edit(repo, current, edit))
    }

    fn comment(&self, repo: &ForgeRepo, item: &Item, body: &str) -> Request {
        dispatch!(self, backend => backend.comment(repo, item, body))
    }

    fn set_state(&self, repo: &ForgeRepo, item: &Item, state: ItemState) -> Result<Request> {
        dispatch!(self, backend => backend.set_state(repo, item, state))
    }

    fn merge(
        &self,
        repo: &ForgeRepo,
        current: &Current,
        method: MergeMethod,
        delete_branch: bool,
    ) -> Result<Vec<Request>> {
        dispatch!(self, backend => backend.merge(repo, current, method, delete_branch))
    }

    fn comment_key(&self) -> (&'static str, &'static str) {
        dispatch!(self, backend => backend.comment_key())
    }

    fn reply_key(&self) -> (&'static str, &'static str) {
        dispatch!(self, backend => backend.reply_key())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn well_known_and_gitlab_named_hosts_are_recognized() {
        for (host, kind) in [
            ("github.com", ForgeKind::GitHub),
            ("gitlab.com", ForgeKind::GitLab),
            ("bitbucket.org", ForgeKind::Bitbucket),
            ("gitlab.corp.example", ForgeKind::GitLab),
            ("git.corp.example", ForgeKind::Forgejo),
            ("mygitlab.example", ForgeKind::Forgejo),
            ("codeberg.org", ForgeKind::Forgejo),
        ] {
            assert_eq!(ForgeKind::detect(host), kind, "{host}");
        }
    }
}
