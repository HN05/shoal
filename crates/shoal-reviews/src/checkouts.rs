//! The checkouts reviews' runs may still have, which hold back the
//! repository's move and removal until they are removed.
use std::path::{Path, PathBuf};

use shoal_ids::repository_ids::RepositoryId;

use crate::{Reviews, reviews::ReviewId};

impl Reviews {
    /// Record that review `id` may have a checkout of `repository` in its
    /// run at `run` until the returned value is dropped, once it is removed.
    pub(crate) fn hold_checkout(
        &self,
        id: ReviewId,
        repository: &RepositoryId,
        run: &Path,
    ) -> CheckoutHold<'_> {
        self.checkouts
            .lock()
            .unwrap()
            .insert(id, HeldCheckout::new(repository, run));
        CheckoutHold { reviews: self, id }
    }

    /// Hold the run at `run` for review `id` as [`Self::hold_checkout`]
    /// does, unless another review's run there holds it; then whether that
    /// one is kept, which only a restarted daemon removes.
    pub(crate) fn try_hold_run(
        &self,
        id: ReviewId,
        repository: &RepositoryId,
        run: &Path,
    ) -> Result<CheckoutHold<'_>, bool> {
        let mut checkouts = self.checkouts.lock().unwrap();
        if let Some(held) = checkouts.values().find(|held| held.run == run) {
            return Err(held.kept);
        }
        checkouts.insert(id, HeldCheckout::new(repository, run));
        Ok(CheckoutHold { reviews: self, id })
    }

    /// Whether a review's checkout of `repository` may exist that its run
    /// still removes, or, when `kept`, that only a restarted daemon removes.
    pub(crate) fn holds_checkout_of(&self, repository: &RepositoryId, kept: bool) -> bool {
        self.checkouts
            .lock()
            .unwrap()
            .values()
            .any(|held| held.repository == *repository && held.kept == kept)
    }
}

/// A review's checkout that may exist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HeldCheckout {
    pub(crate) repository: RepositoryId,
    /// The run's directory, which holds the checkout.
    pub(crate) run: PathBuf,
    /// Whether its run ended without removing it.
    pub(crate) kept: bool,
}

impl HeldCheckout {
    pub(crate) fn new(repository: &RepositoryId, run: &Path) -> Self {
        Self {
            repository: repository.clone(),
            run: run.to_owned(),
            kept: false,
        }
    }
}

/// A review's possible checkout, until it is removed.
pub(crate) struct CheckoutHold<'a> {
    reviews: &'a Reviews,
    id: ReviewId,
}

impl CheckoutHold<'_> {
    /// Keep holding a checkout that could not be removed, until the daemon
    /// restarts and removes what its runs left.
    pub(crate) fn keep(self) {
        if let Some(held) = self.reviews.checkouts.lock().unwrap().get_mut(&self.id) {
            held.kept = true;
        }
        std::mem::forget(self);
    }
}

impl Drop for CheckoutHold<'_> {
    fn drop(&mut self) {
        self.reviews.checkouts.lock().unwrap().remove(&self.id);
    }
}
