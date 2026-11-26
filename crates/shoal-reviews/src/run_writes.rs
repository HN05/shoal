//! What a running review's reviewer writes through the scope its run holds.
//! Its findings and reopens wait for the submit, which delivers them
//! together.
use anyhow::{Context as _, Result, ensure};
use shoal_protocol::{
    space_names::SpaceSelector,
    threads::{NewPost, Thread, ThreadNumber, ThreadOpening},
    time::UnixTime,
};

use crate::{
    Reviews, code,
    daemon::{Daemon, FindingThread},
    model::Workspace,
    reviewer::Files,
    reviews::{Review, ReviewId, ReviewState},
    store, validate,
};

impl Reviews {
    /// Open the finding `opening` gives as a `review` thread of running
    /// review `id`, held by the reviewed workspace, which `to` may name,
    /// until the review is submitted: its name the thread's, its body,
    /// else its name, the opening post, and its code, paths in the run's
    /// checkout, the lines of the reviewed commit that post points at.
    pub async fn open_run_finding(
        &self,
        daemon: &dyn Daemon,
        id: ReviewId,
        opening: ThreadOpening,
        to: Option<&SpaceSelector>,
    ) -> Result<Thread> {
        let (review, workspace) = self.reviewing(daemon, id, to).await?;
        validate::message(&opening.name).context("invalid thread name")?;
        let severity = (opening.severity).context(
            "a finding says how much it matters with --severity blocker, should-fix or nit",
        )?;
        ensure!(
            !opening.code.is_empty(),
            "a finding points at the lines it means with --code PATH:FIRST-LAST"
        );
        let head = (review.head.clone()).context("a running review records its commit")?;
        let checkout = self.run_checkout(id)?;
        let code = code::in_checkout(&checkout, &workspace.path, &head, &opening.code).await?;
        let (records, workspace_id) = (self.records.clone(), workspace.id.clone());
        let thread = self
            .store
            .write(move |tx| {
                ensure_running(store::state(tx, id)?, id)?;
                let finding = FindingThread {
                    review: id,
                    workspace: &workspace_id,
                    severity,
                    title: &opening.name,
                    body: opening.body.as_deref().unwrap_or(&opening.name),
                    code: &code,
                };
                let thread = records.open_finding(tx, &finding)?;
                records.threads(tx, &[thread])
            })
            .await?;
        daemon.review_threads_changed(&workspace).await;
        thread
            .into_iter()
            .next()
            .context("the finding's thread was not recorded")
    }

    /// Hold running review `id`'s reopen of the finding `number`, which the
    /// reviewed workspace holds resolved `fixed`, with `post` saying why its
    /// problem remains, until the review is submitted; the thread as it is
    /// until then.
    pub async fn reopen_run_finding(
        &self,
        daemon: &dyn Daemon,
        id: ReviewId,
        number: ThreadNumber,
        post: NewPost,
    ) -> Result<Thread> {
        let (_, workspace) = self.reviewing(daemon, id, None).await?;
        let number = number.resolve(Some(workspace.number))?;
        ensure!(
            !post.text.trim().is_empty(),
            "say why the problem of finding {number} remains"
        );
        let (records, workspace_id) = (self.records.clone(), workspace.id.clone());
        let thread = self
            .store
            .write(move |tx| {
                ensure_running(store::state(tx, id)?, id)?;
                let thread = records.fixed_finding(tx, &workspace_id, number)?;
                let thread = thread.with_context(|| {
                    format!(
                        "thread {number} is no finding resolved fixed; a review posts only to \
                         reopen one whose problem remains"
                    )
                })?;
                store::hold_reopen(tx, id, thread, &post.text)?;
                records.threads(tx, &[thread])
            })
            .await?;
        thread
            .into_iter()
            .next()
            .context("the finding's thread is gone")
    }

    /// Submit running review `id`, which the reviewed workspace `to` may
    /// name, with `summary`, approving the work when `approve`: it covers
    /// the commits its run recorded, posts the reopens it held, and
    /// delivers its threads together.
    pub async fn submit_run(
        &self,
        daemon: &dyn Daemon,
        id: ReviewId,
        to: Option<&SpaceSelector>,
        approve: bool,
        summary: Option<String>,
    ) -> Result<Review> {
        if let Some(summary) = &summary {
            crate::written::validate_summary(summary)?;
        }
        let (_, workspace) = self.reviewing(daemon, id, to).await?;
        // A managed update waits for the review's posting and delivery.
        let _operation = self.gates.operation().await;
        let (records, workspace_id, now) =
            (self.records.clone(), workspace.id.clone(), UnixTime::now());
        let submitted = self
            .store
            .write(move |tx| {
                let summary = summary.as_deref();
                store::submit_run(tx, &*records, id, &workspace_id, summary, approve, now)
            })
            .await?;
        ensure!(submitted, "review {id} is no longer running");
        daemon.review_threads_changed(&workspace).await;
        let review = self.stored(id).await?;
        self.post(daemon, &review, &workspace).await;
        self.deliver(daemon, id).await;
        self.review(daemon, id).await
    }

    /// Running review `id` and the workspace it reviews, which `to`, when
    /// given, must name.
    async fn reviewing(
        &self,
        daemon: &dyn Daemon,
        id: ReviewId,
        to: Option<&SpaceSelector>,
    ) -> Result<(Review, Workspace)> {
        let review = self.stored(id).await?;
        ensure_running(Some(review.state), id)?;
        let workspace = daemon.workspace_by_id(&review.workspace_id).await?;
        if let Some(to) = to {
            let named = daemon.workspace(to).await?;
            ensure!(
                named.id == workspace.id,
                "review {id} writes only of the workspace it reviews"
            );
        }
        Ok((review, workspace))
    }

    /// The checkout running review `id`'s run holds.
    pub(crate) fn run_checkout(&self, id: ReviewId) -> Result<std::path::PathBuf> {
        let checkouts = self.checkouts.lock().unwrap();
        let held =
            (checkouts.get(&id)).with_context(|| format!("review {id} holds no checkout"))?;
        let files = Files {
            directory: held.run.clone(),
        };
        Ok(files.checkout())
    }
}

/// Refuse review `id` unless its `state` is running.
fn ensure_running(state: Option<ReviewState>, id: ReviewId) -> Result<()> {
    ensure!(
        state == Some(ReviewState::Running),
        "review {id} is no longer running"
    );
    Ok(())
}

#[cfg(test)]
mod tests;
