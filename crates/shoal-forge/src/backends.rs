//! What differs between forges. Each forge implements [`Backend`] and,
//! in a module of its own for each, the area traits over it, such as
//! [`Items`] and [`Runs`]; [`ForgeKind`] names the one a repository uses
//! and forwards each call to it, so a new forge implements the traits and
//! adds its variant here.
pub mod bitbucket;
pub(crate) mod forgejo;
pub(crate) mod github;
mod github_rest;
pub(crate) mod gitlab;
pub(crate) mod rest;

use anyhow::Result;
use serde_json::Value;

use crate::{
    ForgeRepo,
    account::Account,
    capability::{Capability, Support},
    repo::locator::ItemRoute,
    transport::{api::Request, resend::Transient, response::Response},
};

shoal_states::states!(
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
pub(crate) use area::{
    Activity, Attachments, Checks, Conventions, Items, Merging, Reactions, Reviews, Runners, Runs,
};

/// What a forge does its own way that every area needs: its identity and
/// support, routes and endpoints, sign-in, requests and pages. Methods that
/// need the repository take it as `repo`.
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

    /// Whether `account` may push branches to the repository.
    async fn pushable(&self, repo: &ForgeRepo, account: &Account) -> Result<bool>;

    /// `owner/repository` of `account`'s fork of the repository, which the
    /// forge creates when the account has none.
    async fn fork(&self, repo: &ForgeRepo, account: &Account) -> Result<String>;

    /// The login `account` signs in to the forge as; none when it reads
    /// anonymously.
    async fn own_login(&self, repo: &ForgeRepo, account: &Account) -> Result<Option<String>>;

    /// The default branch a read of the repository's own endpoint returned
    /// as `value`.
    fn default_branch<'a>(&self, value: &'a Value) -> Option<&'a str> {
        value["default_branch"].as_str()
    }

    /// The repository's numeric ID, where the forge's uploads name it.
    async fn repository_id(&self, _repo: &ForgeRepo, _account: &Account) -> Result<Option<u64>> {
        Ok(None)
    }
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
