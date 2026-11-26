//! Reviews of a PR, and submitting one.
pub mod diff;
pub mod reviewers;
pub mod thread;

use std::{
    collections::HashMap,
    sync::{Arc, LazyLock, Mutex, PoisonError},
};

use anyhow::{Context, Result};
use serde_json::Value;
use shoal_refs::CommitId;

use crate::{
    items::{
        Items,
        action::{Acted, after_post},
        item::ItemRequests,
    },
    transport::{
        account::Account,
        api::{Request, TransportRequests},
    },
    updates::Edited,
};
use diff::{Bases, Diff, DiffRequests, Placed};
use shoal_forge_types::{
    ForgeRepo,
    backends::ForgeKind,
    items::{
        item::Current,
        link::ItemKind,
        review::{NewReview, ReviewVerdict, reviewers::Reviewer, thread::Thread},
    },
    numbers::{CommentId, PullNumber},
};

/// How the forge asks for reviews, answers review threads and submits
/// reviews of a PR's diff.
#[shoal_forge_derive::forwarded(ForgeKind, dispatch)]
pub(crate) trait Reviews: Items {
    /// The users and teams the repository's PRs can ask for a review.
    async fn reviewers(&self, repo: &ForgeRepo, account: &Account) -> Result<Vec<Reviewer>>;

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
}

/// A review bound to a commit the PR's head has moved past, refused before
/// anything was sent.
#[derive(Debug)]
pub struct HeadMoved {
    pub reviewed: CommitId,
}

impl std::fmt::Display for HeadMoved {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "the PR's head moved past {}, so the review was not sent",
            self.reviewed
        )
    }
}

impl std::error::Error for HeadMoved {}

/// A review with a verdict refused before anything was sent because its
/// account opened the PR, which forges take no verdict from.
#[derive(Debug)]
pub struct OwnPull {
    pub login: String,
}

impl std::fmt::Display for OwnPull {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} opened the PR, and a forge takes no approval or change request from a PR's author",
            self.login
        )
    }
}

impl std::error::Error for OwnPull {}

/// A review whose line comments are on the lines the PR's diff shows.
pub(crate) struct PlacedReview<'a> {
    pub verdict: ReviewVerdict,
    pub body: &'a str,
    pub comments: Vec<Placed<'a>>,
    /// The PR's head commit when Shoal read it, which the review is of.
    pub head: Option<&'a str>,
    pub bases: Option<&'a Bases>,
}

/// The requests that submit a review.
pub(crate) struct Submission {
    /// Requests that create drafts no one else sees until `requests`
    /// publish them.
    pub drafts: Vec<Request>,
    /// The requests that post the review, in order.
    pub requests: Vec<Request>,
}

impl Submission {
    /// A review posted by `requests`, in order, without drafts.
    pub(crate) fn posting(requests: Vec<Request>) -> Self {
        Self {
            drafts: Vec::new(),
            requests,
        }
    }

    /// Send the drafts, then the requests in order, to `forge`, stopping
    /// at the first failure. A failure before the review is published
    /// deletes the drafts created, so nothing of it shows.
    async fn send(self, forge: &impl Exchange) -> Submitted {
        let mut drafts = Vec::new();
        for request in &self.drafts {
            match forge.exchange(request).await {
                Ok(draft) => drafts.push(draft),
                Err(error) => {
                    return Submitted::unpublished(discarded(forge, &drafts, error).await);
                }
            }
        }
        let mut responses = drafts.clone();
        for request in &self.requests {
            match forge.exchange(request).await {
                Ok(response) => responses.push(response),
                Err(error) if responses.len() == drafts.len() => {
                    return Submitted::unpublished(discarded(forge, &drafts, error).await);
                }
                Err(error) => {
                    return Submitted {
                        responses,
                        failure: Some(error),
                        published: true,
                    };
                }
            }
        }
        Submitted {
            responses,
            failure: None,
            published: true,
        }
    }
}

/// What sending a submission left: the responses to the requests sent, in
/// order, and the failure that stopped it, if any.
pub(crate) struct Submitted {
    responses: Vec<Value>,
    failure: Option<anyhow::Error>,
    /// Whether a request that posts the review reached the forge.
    published: bool,
}

/// Where a submission's requests go.
trait Exchange {
    /// The forge's response to `request`.
    async fn exchange(&self, request: &Request) -> Result<Value>;

    /// The request that deletes the draft a draft request's response
    /// describes.
    fn discard(&self, draft: &Value) -> Result<Request>;
}

/// The PR `number` on `repo`, reached as `account`.
struct Forge<'a> {
    repo: &'a ForgeRepo,
    account: &'a Account,
    number: PullNumber,
}

impl Exchange for Forge<'_> {
    async fn exchange(&self, request: &Request) -> Result<Value> {
        self.repo.send(self.account, request).await
    }

    fn discard(&self, draft: &Value) -> Result<Request> {
        let repo = self.repo;
        Ok(repo
            .kind()
            .discard_draft(repo, self.number, draft)?
            .repeatable())
    }
}

/// `error`, after deleting through `forge` the `drafts` it left behind.
async fn discarded(forge: &impl Exchange, drafts: &[Value], error: anyhow::Error) -> anyhow::Error {
    for draft in drafts {
        let discarded = match forge.discard(draft) {
            Ok(request) => forge.exchange(&request).await.map(drop),
            Err(discard) => Err(discard),
        };
        if let Err(discard) = discarded {
            return error.context(format!(
                "deleting the review's drafts also failed, so some remain: {discard:#}"
            ));
        }
    }
    error
}

/// Submitting reviews.
#[shoal_forge_derive::extension(pub SubmitRequests, pub(crate) SubmitSteps)]
impl ForgeRepo {
    /// Submit `review` of PR `number` as `account`, refusing it before
    /// anything is sent when a line comment is off the PR's diff.
    pub(crate) async fn submit_review(
        &self,
        account: &Account,
        number: PullNumber,
        review: &NewReview,
    ) -> Result<Acted> {
        let _submitting =
            Submitting::start(self.item_url(ItemKind::Pr, number.get())?.to_string()).await;
        let current = self.read(account, ItemKind::Pr, number.get()).await?;
        if let Some(reviewed) = &review.commit
            && current.head_sha.as_ref() != Some(reviewed)
        {
            return Err(HeadMoved {
                reviewed: reviewed.clone(),
            }
            .into());
        }
        self.kind().check_review(self, account, number).await?;
        let diff = if review.comments.is_empty() {
            Diff::default()
        } else {
            self.pr_diff(account, &current).await?
        };
        let placed = PlacedReview {
            verdict: review.verdict,
            body: &review.body,
            comments: diff.place(&review.comments)?,
            head: current.head_sha.as_ref().map(shoal_refs::CommitId::as_str),
            bases: diff.bases.as_ref(),
        };
        let submission = self.kind().review(self, number, &placed)?;
        let forge = Forge {
            repo: self,
            account,
            number,
        };
        let submitted = submission.send(&forge).await;
        let authored = self.posted_keys(account, number, &placed, &submitted).await;
        if let Some(error) = submitted.failure {
            return Err(after_post(error, submitted.published, &authored));
        }
        let item = self
            .read(account, ItemKind::Pr, number.get())
            .await
            .map_err(|error| after_post(error, true, &authored))?
            .item;
        Ok(Acted {
            item,
            authored,
            edited: Edited::default(),
        })
    }

    /// The head commit of PR `number` that a review with a verdict as
    /// `account` would be of, refusing an account without a login of its
    /// own for the host, and with [`OwnPull`] one that opened the PR, before
    /// anything is sent.
    pub async fn verdict_head(
        &self,
        account: &Account,
        number: PullNumber,
    ) -> Result<Option<CommitId>> {
        account.require_login(self)?;
        let login = (self.kind().own_assignee(self, account).await?)
            .context("the forge did not report the account's login")?;
        let current = self.read(account, ItemKind::Pr, number.get()).await?;
        if (current.author.as_ref()).is_some_and(|author| author.eq_ignore_ascii_case(&login)) {
            return Err(OwnPull { login }.into());
        }
        Ok(current.head_sha)
    }

    /// The activity keys of what `submitted` posted of `review`. Without
    /// them the review is reported back like anyone else's, rather than
    /// failing a review that was posted.
    async fn posted_keys(
        &self,
        account: &Account,
        number: PullNumber,
        review: &PlacedReview<'_>,
        submitted: &Submitted,
    ) -> Vec<String> {
        if !submitted.published {
            return Vec::new();
        }
        let responses = &submitted.responses;
        let kind = self.kind();
        let keys = kind.review_keys(self, account, number, review, responses);
        keys.await.unwrap_or_default()
    }
}

impl Submitted {
    fn unpublished(failure: anyhow::Error) -> Self {
        Self {
            responses: Vec::new(),
            failure: Some(failure),
            published: false,
        }
    }
}

/// The lock each PR's review submissions in this process take in turn,
/// by the PR's URL. GitLab publishes every draft an account has pending,
/// so one review's drafts must not be pending while another is checked or
/// published.
static SUBMITTING: LazyLock<Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>> =
    LazyLock::new(Default::default);

/// A review submission's turn on its PR, which ends when it drops.
struct Submitting {
    url: String,
    _turn: tokio::sync::OwnedMutexGuard<()>,
}

impl Submitting {
    /// Wait for the turn of a review of the PR at `url`.
    async fn start(url: String) -> Self {
        let lock = {
            let mut submitting = SUBMITTING.lock().unwrap_or_else(PoisonError::into_inner);
            submitting.entry(url.clone()).or_default().clone()
        };
        Self {
            url,
            _turn: lock.lock_owned().await,
        }
    }
}

impl Drop for Submitting {
    /// Forget the PR's lock once no other submission holds or awaits it:
    /// then only the map and this turn refer to it.
    fn drop(&mut self) {
        let mut submitting = SUBMITTING.lock().unwrap_or_else(PoisonError::into_inner);
        if submitting
            .get(&self.url)
            .is_some_and(|lock| Arc::strong_count(lock) == 2)
        {
            submitting.remove(&self.url);
        }
    }
}

#[cfg(test)]
mod tests;
