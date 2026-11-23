//! Reviews the user or a space writes, as Shoal's reviewer writes its own:
//! a writer starts a pending review of a workspace below it, the threads it
//! opens towards that workspace join it and hold their posts, submitting it
//! at the workspace's HEAD delivers them together, and cancelling it drops
//! them.
use anyhow::{Context as _, Result, bail, ensure};
use rusqlite::Transaction;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::{space_names::SpaceSelector, time::UnixTime};

use crate::{
    Reviews,
    daemon::{Daemon, Records},
    model::Workspace,
    reviews::{Review, ReviewId, Verdict},
    validate,
};

pub mod store;

/// Longest summary of a review its writer submits, in bytes: half of the
/// message that delivers it, so its comments and how to answer them fit.
pub const MAX_SUMMARY_LEN: usize = validate::MAX_AGENT_MESSAGE_LEN / 2;

/// Who writes a review: the user, or a space's agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Writer {
    User,
    Space(SpaceId),
}

impl Writer {
    /// The writer a request of the space `space` speaks for, the user
    /// without one.
    pub fn of(space: Option<&SpaceId>) -> Self {
        match space {
            Some(space) => Self::Space(space.clone()),
            None => Self::User,
        }
    }
}

impl Reviews {
    /// Start `writer`'s pending review of the workspace `selector` names,
    /// or return the one it has.
    pub async fn start_written(
        &self,
        daemon: &dyn Daemon,
        selector: &SpaceSelector,
        writer: Writer,
    ) -> Result<Review> {
        let workspace = daemon.workspace(selector).await?;
        ensure_below(&workspace, &writer)?;
        let (id, now) = (workspace.id.clone(), UnixTime::now());
        let review = (self.store)
            .write(move |tx| store::start(tx, &id, &writer, now))
            .await?;
        self.review(daemon, review).await
    }

    /// `writer`'s pending reviews, oldest first.
    pub async fn pending_reviews(
        &self,
        daemon: &dyn Daemon,
        writer: Writer,
    ) -> Result<Vec<Review>> {
        let records = self.records.clone();
        let reviews = self
            .store
            .read(move |tx| {
                let ids = store::pending_by(tx, &writer)?;
                (ids.into_iter())
                    .filter_map(|id| crate::store::load(tx, &*records, id).transpose())
                    .collect::<Result<Vec<_>>>()
            })
            .await?;
        let mut reviews = reviews;
        crate::view::annotate(daemon, &mut reviews).await;
        Ok(reviews)
    }

    /// Submit `writer`'s pending review of the workspace `selector` names
    /// at the workspace's HEAD with `summary`, approving its work when
    /// `approve`, and deliver its comments together.
    pub async fn submit(
        &self,
        daemon: &dyn Daemon,
        selector: &SpaceSelector,
        writer: Writer,
        approve: bool,
        summary: Option<String>,
    ) -> Result<Review> {
        if let Some(summary) = &summary {
            validate_summary(summary)?;
        }
        let pending = self.pending_of(daemon, selector, &writer).await?;
        let id = pending.with_context(|| {
            format!("no pending review of {selector} is yours; start one with shoal review start")
        })?;
        let review = self.stored(id).await?;
        let workspace = daemon.workspace_by_id(&review.workspace_id).await?;
        let head = daemon.head(&workspace).await?;
        let base = daemon.diff_base(&workspace.id).await?;
        let (records, now) = (self.records.clone(), UnixTime::now());
        let submitted = self
            .store
            .write(move |tx| {
                let comments = records.review_threads(tx, id)?.len();
                let submitted = store::Submitted {
                    head: &head,
                    base: &base,
                    summary: summary.as_deref(),
                    verdict: verdict(approve, comments),
                };
                let submitted = store::submit(tx, id, &submitted, now)?;
                if submitted {
                    records.release_review_threads(tx, id)?;
                }
                Ok(submitted)
            })
            .await?;
        ensure!(submitted, "review {id} is no longer pending");
        daemon.review_threads_changed(&workspace).await;
        self.deliver(daemon, id).await;
        self.review(daemon, id).await
    }

    /// `writer`'s pending review of the workspace `selector` names.
    pub(crate) async fn pending_of(
        &self,
        daemon: &dyn Daemon,
        selector: &SpaceSelector,
        writer: &Writer,
    ) -> Result<Option<ReviewId>> {
        let workspace = daemon.workspace(selector).await?;
        let writer = writer.clone();
        (self.store)
            .run(move |db| store::pending_of(db, &workspace.id, &writer))
            .await
    }

    /// Who writes review `id`, unless Shoal's reviewer does.
    pub(crate) async fn writer_of(&self, id: ReviewId) -> Result<Option<Writer>> {
        self.store.run(move |db| store::writer(db, id)).await
    }

    /// Cancel the pending review `id` that `writer` wrote, or any for the
    /// user, with its threads.
    pub(crate) async fn cancel_written(
        &self,
        daemon: &dyn Daemon,
        id: ReviewId,
        writer: &Writer,
    ) -> Result<Review> {
        let review = self.stored(id).await?;
        ensure_writes(&review, writer)?;
        let (records, now) = (self.records.clone(), UnixTime::now());
        let cancelled = self
            .store
            .write(move |tx| {
                let cancelled = store::cancel(tx, id, now)?;
                if cancelled {
                    records.drop_review_threads(tx, id)?;
                }
                Ok(cancelled)
            })
            .await?;
        ensure!(cancelled, "review {id} is no longer pending");
        let workspace = daemon.workspace_by_id(&review.workspace_id).await?;
        daemon.review_threads_changed(&workspace).await;
        self.review(daemon, id).await
    }
}

/// Refuse a summary that is empty, holds control characters other than
/// newlines and tabs, or leaves the message that delivers it no room for
/// its comments.
fn validate_summary(summary: &str) -> Result<()> {
    validate::agent_message(summary).context("invalid summary")?;
    ensure!(
        summary.len() <= MAX_SUMMARY_LEN,
        "a review's summary is at most {MAX_SUMMARY_LEN} bytes"
    );
    Ok(())
}

/// What a review submitted with `comments` says: it approves when its
/// writer says so, else asks for changes when it comments, else only
/// comments.
fn verdict(approve: bool, comments: usize) -> Verdict {
    match (approve, comments) {
        (true, _) => Verdict::Approve,
        (false, 0) => Verdict::Comment,
        (false, _) => Verdict::RequestChanges,
    }
}

/// Cancel the pending reviews the space `space` writes, and those of it,
/// with their threads, as its removal does in its transaction: their
/// comments reached nobody, and a restore could never submit them.
pub fn drop_pending_of_removed(
    tx: &Transaction<'_>,
    records: &dyn Records,
    space: &SpaceId,
) -> Result<()> {
    let mut pending = store::pending_by(tx, &Writer::Space(space.clone()))?;
    pending.extend(store::pending_of_workspace(tx, space)?);
    let now = UnixTime::now();
    for id in pending {
        store::cancel(tx, id, now)?;
        records.drop_review_threads(tx, id)?;
    }
    Ok(())
}

/// Refuse `writer` reviewing `workspace` when it is that workspace: a
/// space reviews the work of the spaces below it, which its scope reaches.
fn ensure_below(workspace: &Workspace, writer: &Writer) -> Result<()> {
    if *writer == Writer::Space(workspace.id.clone()) {
        bail!(
            "a space reviews the work of the spaces below it; ask for a review of your own with \
             shoal review run"
        );
    }
    Ok(())
}

/// Whether `writer` wrote `review`, or is the user, who acts on any.
fn writes(review: &Review, writer: &Writer) -> bool {
    match (&review.author, writer) {
        (_, Writer::User) => true,
        (crate::reviews::Author::Space { space }, Writer::Space(id)) => space.id == *id,
        _ => false,
    }
}

/// Refuse `writer` acting on `review` unless it wrote it; the user acts on
/// any.
fn ensure_writes(review: &Review, writer: &Writer) -> Result<()> {
    ensure!(
        writes(review, writer),
        "review {} is {}'s, who submits or cancels it",
        review.id,
        review.author.label()
    );
    Ok(())
}

#[cfg(test)]
mod tests;
