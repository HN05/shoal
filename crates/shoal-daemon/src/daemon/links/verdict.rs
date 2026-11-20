//! Reviews that approve or request changes on a PR for a scoped caller:
//! its `pr_review` permission governs them, they go out as the host's
//! `review_token`, and approving one binds the PR's head and the review's
//! text.
use anyhow::Result;
use serde::Serialize;
use shoal_daemon_base::permitted_actions::Action;
use shoal_links::place::Access;
use shoal_refs::CommitId;

use crate::{
    config::permissions::PermissionKey,
    daemon::{manager::Manager, scope::Caller},
    forge::items::{
        reference::ItemInput,
        review::{LineComment, NewReview, ReviewVerdict},
    },
    protocol::RepoTarget,
};
use shoal_forge::items::review::{OwnPull, SubmitRequests};
use shoal_ids::repository_ids::RepositoryId;

/// What approving a review with a verdict binds.
#[derive(Serialize)]
struct Verdict<'a> {
    verdict: ReviewVerdict,
    pr: String,
    commit: Option<&'a CommitId>,
    body: &'a str,
    comments: &'a [LineComment],
}

impl Manager {
    /// The `pr_review` action of a scoped `caller`'s `review` of the PR
    /// `input` names in the `target` repository, or the space's linked PR,
    /// in `repository`: the PR, the head it covers, to which the review is
    /// pinned, and its text. A host without a `review_token`, a PR its
    /// account opened and another repository than the caller's refuse it
    /// before anything is sent or asked.
    pub(crate) async fn verdict_action(
        &self,
        target: &RepoTarget,
        input: Option<ItemInput>,
        review: &mut NewReview,
        repository: RepositoryId,
        caller: &Caller,
    ) -> Result<Action> {
        let pull = self.context_pull(target, input, Access::Change, Some(caller));
        let (forge, url) = pull.await?;
        let account = self.links.review_account();
        let head = forge.verdict_head(&account, url.pull()?).await;
        let head = head.map_err(|error| naming_review_token(error, &forge.host))?;
        if review.commit.is_none() {
            review.commit = head;
        }
        let verdict = Verdict {
            verdict: review.verdict,
            pr: url.to_string(),
            commit: review.commit.as_ref(),
            body: &review.body,
            comments: &review.comments,
        };
        let key = PermissionKey::PrReview;
        Ok(Action::request(key, &verdict, Some(repository)))
    }
}

/// `error`, saying for a review account that opened the PR which setting
/// names another.
fn naming_review_token(error: anyhow::Error, host: &str) -> anyhow::Error {
    match error.is::<OwnPull>() {
        true => anyhow::anyhow!(
            "{error}; set review_token under [machine.forges.\"{host}\"] in the global config to another account, then run `shoal config reload`"
        ),
        false => error,
    }
}
