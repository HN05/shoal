//! Reviews the user or a space writes, as Shoal's reviewer writes its own:
//! a writer starts a pending review of a workspace below it, the threads it
//! opens towards that workspace join it and hold their posts, and
//! cancelling it drops them.
use anyhow::{Context as _, Result, bail, ensure};
use rusqlite::{Connection, Transaction};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::{space_names::SpaceSelector, time::UnixTime};

use crate::{
    Reviews,
    daemon::{Daemon, Records},
    model::Workspace,
    reviews::{Review, ReviewId},
};

pub mod store;

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
        let (id, records, now) = (workspace.id.clone(), self.records.clone(), UnixTime::now());
        let review = self
            .store
            .write(move |tx| {
                ensure_here(tx, &*records, &writer)?;
                store::start(tx, &id, &writer, now)
            })
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

/// Cancel the pending reviews `writer` wrote, with their threads, as the
/// removal of the space that wrote them does in its transaction.
pub fn drop_written_by(tx: &Transaction<'_>, records: &dyn Records, writer: &Writer) -> Result<()> {
    let now = UnixTime::now();
    for id in store::pending_by(tx, writer)? {
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

/// Refuse a space of another machine writing a review here, since the
/// threads it opens are numbered in its own space there.
fn ensure_here(db: &Connection, records: &dyn Records, writer: &Writer) -> Result<()> {
    if let Writer::Space(id) = writer {
        records.space_name(db, id)?.with_context(|| {
            format!("{id} is on another machine; a space reviews the workspaces of its own machine")
        })?;
    }
    Ok(())
}

/// Refuse `writer` acting on `review` unless it wrote it; the user acts on
/// any.
fn ensure_writes(review: &Review, writer: &Writer) -> Result<()> {
    let wrote = match (&review.author, writer) {
        (_, Writer::User) => true,
        (crate::reviews::Author::Space { space }, Writer::Space(id)) => space.id == *id,
        _ => false,
    };
    ensure!(
        wrote,
        "review {} is {}'s, who submits or cancels it",
        review.id,
        review.author.label()
    );
    Ok(())
}

#[cfg(test)]
mod tests;
