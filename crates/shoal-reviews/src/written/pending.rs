//! The pending reviews a caller writes, wherever they are kept. The machine
//! that runs a workspace keeps the reviews of it, so a request reads every
//! machine that runs a space its caller sees. A request another machine
//! forwarded here reads this machine's alone, so it is never forwarded
//! again.
use anyhow::{Context as _, Result, bail, ensure};
use futures_util::future::join_all;
use serde::de::DeserializeOwned;
use shoal_daemon_base::{callers::Caller, log};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::{
    hub::Member,
    protocol::{Body, Operation, ReviewMethod},
    reviews::Review,
    space_names::SpaceSelector,
};

use super::Writer;
use crate::{Reviews, daemon::Daemon};

/// The pending reviews of a request's caller, the user without one.
pub struct Pending<'a> {
    reviews: &'a Reviews,
    daemon: &'a dyn Daemon,
    caller: Option<&'a Caller>,
    /// Whether another machine forwarded the request here.
    forwarded: bool,
}

/// A pending review and the machine that keeps it, none for this one.
struct Kept {
    review: Review,
    machine: Option<Member>,
}

impl<'a> Pending<'a> {
    pub fn new(
        reviews: &'a Reviews,
        daemon: &'a dyn Daemon,
        caller: Option<&'a Caller>,
        forwarded: bool,
    ) -> Self {
        Self {
            reviews,
            daemon,
            caller,
            forwarded,
        }
    }

    /// The caller's pending reviews, oldest first, leaving out those of a
    /// machine that cannot be asked, as while it is offline.
    pub async fn reviews(&self) -> Result<Vec<Review>> {
        let kept = self.kept().await?;
        Ok(kept.into_iter().map(|kept| kept.review).collect())
    }

    /// Submit the caller's only pending review with `summary`, approving
    /// its work when `approve`.
    pub async fn submit_only(&self, approve: bool, summary: Option<String>) -> Result<Review> {
        let kept = self.kept().await?;
        ensure!(
            !kept.is_empty(),
            "you have no pending review; start one with shoal review start <space>"
        );
        let only = only(kept)
            .context("you have several pending reviews; name the space whose review to submit")?;
        let workspace = SpaceSelector::id(&only.review.workspace_id);
        match only.machine {
            None => {
                let submitted =
                    (self.reviews).submit(self.daemon, &workspace, self.writer(), approve, summary);
                submitted.await
            }
            Some(machine) => {
                let method = ReviewMethod::SubmitReview {
                    workspace: Some(workspace),
                    approve,
                    summary,
                };
                self.ask(&machine, method).await
            }
        }
    }

    /// Cancel, for a command run in the workspace `here` names, the
    /// caller's pending review of it, else its queued or running one, else
    /// the caller's only pending review.
    pub async fn cancel_here(&self, here: &SpaceSelector) -> Result<Review> {
        let reviews = self.reviews;
        if let Some(id) = reviews.to_cancel(self.daemon, here, &self.writer()).await? {
            return reviews.cancel(self.daemon, None, Some(id), self.by()).await;
        }
        let only = only(self.kept().await?)
            .context("nothing here to cancel, nor is any one review of yours pending")?;
        match only.machine {
            None => {
                let cancelled = reviews.cancel(self.daemon, None, Some(only.review.id), self.by());
                cancelled.await
            }
            Some(machine) => {
                let method = ReviewMethod::CancelReview {
                    workspace: Some(SpaceSelector::id(&only.review.workspace_id)),
                    id: None,
                    here: false,
                };
                self.ask(&machine, method).await
            }
        }
    }

    /// The caller's pending reviews with the machine that keeps each,
    /// oldest first.
    async fn kept(&self) -> Result<Vec<Kept>> {
        let here = (self.reviews).pending_reviews(self.daemon, self.writer());
        let mut kept: Vec<Kept> = (here.await?.into_iter())
            .map(|review| Kept {
                review,
                machine: None,
            })
            .collect();
        if !self.forwarded {
            let machines = self.machines().await;
            let elsewhere = join_all(machines.into_iter().map(|machine| self.kept_on(machine)));
            kept.extend(elsewhere.await.into_iter().flatten());
        }
        kept.sort_by_key(|kept| kept.review.created_at);
        Ok(kept)
    }

    /// The other machines that run a space the caller sees, which keep the
    /// reviews of those spaces; none while the hub cannot be asked.
    async fn machines(&self) -> Vec<Member> {
        let machines = self.daemon.machines_seen(self.caller);
        machines.await.unwrap_or_else(|error| {
            log!("could not ask the hub which machines keep pending reviews: {error:#}");
            Vec::new()
        })
    }

    /// The caller's pending reviews `machine` keeps; none while it cannot
    /// be asked.
    async fn kept_on(&self, machine: Member) -> Vec<Kept> {
        match self
            .ask::<Vec<Review>>(&machine, ReviewMethod::PendingReviews)
            .await
        {
            Ok(reviews) => (reviews.into_iter())
                .map(|review| Kept {
                    review,
                    machine: Some(machine.clone()),
                })
                .collect(),
            Err(error) => {
                log!(
                    "{} kept no pending review read here: {error:#}",
                    machine.name
                );
                Vec::new()
            }
        }
    }

    /// What `machine` answers the caller's review request `method`.
    async fn ask<T: DeserializeOwned>(&self, machine: &Member, method: ReviewMethod) -> Result<T> {
        let operation = Operation::Review(method);
        let asked = self.daemon.ask_machine(machine, operation, self.caller);
        let Body::ReviewPieces(pieces) = asked.await? else {
            bail!(
                "machine {} answered a review request unexpectedly",
                machine.name
            );
        };
        let json: String = pieces.into_iter().map(|piece| piece.0).collect();
        (serde_json::from_str(&json))
            .with_context(|| format!("machine {} sent an invalid review", machine.name))
    }

    /// Who writes the caller's reviews.
    fn writer(&self) -> Writer {
        Writer::of(self.caller.map(|caller| &caller.space_id))
    }

    /// The space whose process asks, none for the user.
    fn by(&self) -> Option<SpaceId> {
        self.caller.map(|caller| caller.space_id.clone())
    }
}

/// The only one of `kept`; none for none or several.
fn only(mut kept: Vec<Kept>) -> Option<Kept> {
    match kept.len() {
        1 => kept.pop(),
        _ => None,
    }
}
