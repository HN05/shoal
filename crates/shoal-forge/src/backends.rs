//! What differs between forges' requests. Each forge's marker type, which
//! implements its routes in `shoal_forge_types::backends`, implements
//! [`Backend`] and, in a module of its own for each, the area traits over
//! it, such as [`Items`] and [`Runs`]; `ForgeKind` names the one a
//! repository uses and forwards each call to it, so a new forge adds its
//! variant and routes there and implements the traits here.
pub mod bitbucket;
pub(crate) mod forgejo;
pub(crate) mod github;
mod github_rest;
pub(crate) mod gitlab;
pub(crate) mod rest;

use anyhow::Result;
use serde_json::Value;

use crate::{
    transport::account::Account,
    transport::{api::Request, resend::Transient, response::Response},
};
use shoal_forge_types::{
    ForgeRepo,
    backends::{ForgeKind, Routes},
};

/// How a forge's tokens sign in.
#[shoal_forge_derive::extension(pub SignIn)]
impl ForgeKind {
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

mod area;
pub(crate) use area::{
    Activity, Attachments, BranchFields, Branches, Checks, Conventions, Items, Merging, Reactions,
    Reviews, Runners, Runs, subject_and_body,
};

/// What a forge does its own way that every area needs beyond its
/// [`Routes`]: its endpoints, sign-in, requests and pages.
/// Methods that need the repository take it as `repo`.
#[shoal_forge_derive::forwarded(ForgeKind, dispatch)]
pub(crate) trait Backend: Routes {
    /// The API endpoint for `path` under the repository.
    fn repo_endpoint(&self, repo: &ForgeRepo, path: &str) -> String;

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

    /// The login `account` signs in to the forge as; none when it reads
    /// anonymously.
    async fn own_login(&self, repo: &ForgeRepo, account: &Account) -> Result<Option<String>>;
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
