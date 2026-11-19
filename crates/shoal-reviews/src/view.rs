//! Reviews as requests read them: one by ID, a workspace's newest, a
//! listing, and a wait for one to end, each marked outdated once its
//! workspace's HEAD has moved past it.
use std::collections::HashMap;

use anyhow::{Context, Result};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::{repository::Selector, space_names::SpaceSelector};

use crate::{
    Reviews,
    daemon::Daemon,
    reviews::{Review, ReviewId},
    store,
};

impl Reviews {
    /// Review `id` in full, marked outdated once its workspace's HEAD moved.
    pub async fn review(&self, daemon: &dyn Daemon, id: ReviewId) -> Result<Review> {
        let mut review = self.stored(id).await?;
        annotate(daemon, std::slice::from_mut(&mut review)).await;
        Ok(review)
    }

    /// The workspace's newest review.
    pub async fn latest(&self, daemon: &dyn Daemon, selector: &SpaceSelector) -> Result<Review> {
        let workspace = daemon.workspace(selector).await?;
        let (id, records) = (workspace.id.clone(), self.records.clone());
        let latest = (self.store)
            .run(move |db| store::latest(db, &*records, &id))
            .await?;
        let mut review = latest.with_context(|| {
            format!(
                "{} has no reviews; start one with shoal review run",
                workspace.name
            )
        })?;
        annotate(daemon, std::slice::from_mut(&mut review)).await;
        Ok(review)
    }

    /// The workspaces whose reviews a listing shows: the one named, else the
    /// repository's, else every workspace.
    pub async fn workspaces(
        &self,
        daemon: &dyn Daemon,
        workspace: Option<&SpaceSelector>,
        repository: Option<Selector>,
    ) -> Result<Vec<SpaceId>> {
        if let Some(workspace) = workspace {
            return Ok(vec![daemon.workspace(workspace).await?.id]);
        }
        let repository = match repository {
            Some(repository) => Some(daemon.select_repository(repository).await?.id),
            None => None,
        };
        let records = self.records.clone();
        self.store
            .run(move |db| records.workspaces(db, repository.as_ref()))
            .await
    }

    /// The reviews of the workspaces `ids`, newest first.
    pub async fn list(&self, daemon: &dyn Daemon, ids: Vec<SpaceId>) -> Result<Vec<Review>> {
        let records = self.records.clone();
        let mut reviews = (self.store)
            .run(move |db| store::list(db, &*records, &ids))
            .await?;
        annotate(daemon, &mut reviews).await;
        Ok(reviews)
    }

    /// Review `id` once it is neither queued nor running.
    pub async fn wait(&self, daemon: &dyn Daemon, id: ReviewId) -> Result<Review> {
        let mut changed = self.store.watch_changes();
        let mut ended = self.ended.subscribe();
        loop {
            let review = self.review(daemon, id).await?;
            if !review.state.is_active() {
                return Ok(review);
            }
            tokio::select! {
                changed = changed.changed() => changed.context("the daemon is stopping")?,
                ended = ended.changed() => ended.context("the daemon is stopping")?,
            }
        }
    }

    /// The workspace review `id` is of.
    pub async fn workspace_of(&self, id: ReviewId) -> Result<SpaceId> {
        Ok(self.stored(id).await?.workspace_id)
    }

    /// Review `id` as stored, without what its workspace says of it.
    pub(crate) async fn stored(&self, id: ReviewId) -> Result<Review> {
        let review = self.load(id).await?;
        review.with_context(|| format!("review {id} does not exist"))
    }

    /// Review `id` as stored, while it and its workspace exist.
    pub(crate) async fn load(&self, id: ReviewId) -> Result<Option<Review>> {
        let records = self.records.clone();
        (self.store)
            .run(move |db| store::load(db, &*records, id))
            .await
    }
}

/// Mark each review whose workspace's HEAD has moved past it as outdated;
/// HEAD that cannot be read leaves it unknown.
async fn annotate(daemon: &dyn Daemon, reviews: &mut [Review]) {
    let mut heads = HashMap::new();
    for review in reviews.iter_mut() {
        let Some(reviewed) = &review.head else {
            continue;
        };
        if !heads.contains_key(&review.workspace_id) {
            let head = match daemon.workspace_by_id(&review.workspace_id).await {
                Ok(workspace) => daemon.head(&workspace).await.ok(),
                Err(_) => None,
            };
            heads.insert(review.workspace_id.clone(), head);
        }
        review.outdated = heads[&review.workspace_id]
            .as_ref()
            .map(|head| head != reviewed);
    }
}
