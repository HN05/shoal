//! Running queued reviews and the reviewer's answers in findings' threads.
use std::{collections::HashSet, sync::Arc};

use anyhow::Result;
use tokio::task::JoinSet;

use crate::{
    Reviews,
    daemon::{AwaitedPost, Daemon},
    log,
    reviews::ReviewId,
    store,
};

mod attempts;
mod commits;
mod directories;
mod instructions;
mod left;
mod review;

pub(crate) use attempts::{InPlace, Task, resumes};
pub(crate) use commits::review_commits;
pub(crate) use directories::{release_run, run_files};
/// Start every queued review and every answer a finding's thread waits
/// for, now and whenever one is queued or posted; each then waits in the
/// agent queue for admission.
pub async fn run(reviews: Arc<Reviews>, daemon: Arc<dyn Daemon>) {
    let mut running = JoinSet::new();
    let mut started = HashSet::new();
    loop {
        let queued = reviews.queued.notified();
        tokio::pin!(queued);
        queued.as_mut().enable();
        match waiting(&reviews).await {
            Ok(jobs) => {
                for job in jobs.into_iter().filter(|job| started.insert(*job)) {
                    let (reviews, daemon) = (reviews.clone(), daemon.clone());
                    running.spawn(async move {
                        match job {
                            Job::Review(id) => reviews.run_review(&*daemon, id).await,
                            Job::Answer(post) => reviews.answer(&*daemon, post).await,
                        }
                        job
                    });
                }
            }
            Err(error) => log!("queued reviews: {error:#}"),
        }
        tokio::select! {
            () = &mut queued => {}
            Some(ended) = running.join_next(), if !running.is_empty() => {
                if let Ok(job) = ended {
                    started.remove(&job);
                }
            }
        }
    }
}

/// What the scheduler runs: a queued review, or the reviewer's answer to
/// the latest post in a finding's thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Job {
    Review(ReviewId),
    Answer(AwaitedPost),
}

/// The queued reviews, then the posts in findings' threads that wait for
/// an answer the reviewer has not yet run for.
async fn waiting(reviews: &Reviews) -> Result<Vec<Job>> {
    let records = reviews.records.clone();
    let (queued, awaited) = (reviews.store)
        .run(move |db| Ok((store::queued(db)?, records.awaiting_answers(db)?)))
        .await?;
    let unanswered = reviews.unanswered(&awaited);
    let queued = queued.into_iter().map(Job::Review);
    Ok(queued
        .chain(unanswered.into_iter().map(Job::Answer))
        .collect())
}
