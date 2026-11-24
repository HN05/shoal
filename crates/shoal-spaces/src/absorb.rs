//! Absorbing a space into one above it: `shoal absorb` folds a space's work
//! into a space of the caller's subtree, which takes over its links, files
//! and open threads, moves the spaces below it up one level, then removes it
//! through the shared removal path.
mod children;
mod code;
mod parties;
mod records;
mod report;

use crate::Spaces;
use crate::daemon::Daemon;
use std::sync::Arc;

use anyhow::Result;

use super::scope::Caller;
use shoal_protocol::{
    absorb::{Absorb, Absorbed},
    space_names::SpaceSelector,
};

impl Spaces {
    /// Absorb the space `request` names into the space that takes it, at
    /// `caller`'s request, refusing before anything changes what it cannot
    /// absorb. Returns once its work and records moved and its removal
    /// passed its first checks; the removal goes on in the background.
    pub async fn absorb_space(
        &self,
        daemon: &Arc<dyn Daemon>,
        request: &Absorb,
        caller: Option<&Caller>,
    ) -> Result<Absorbed> {
        let parties = self.absorb_parties(&**daemon, request, caller).await?;
        self.check_code(&**daemon, &parties, request).await?;
        if parties.forced {
            self.stop_absorbed(&**daemon, &parties).await?;
        }
        let unsaved = self.removal_consent(&**daemon, &parties).await?;
        let (folded, children) = (self.while_target_reserved(&parties, async {
            let folded = self.fold_code(&**daemon, &parties, request).await?;
            let children = self
                .lift_children(&**daemon, &parties, &folded, caller)
                .await?;
            Ok((folded, children))
        }))
        .await?;
        let taken = self.take_over(&**daemon, &parties, caller).await?;
        let absorbed = parties.absorbed(folded, children, taken);
        self.report_absorb(&**daemon, &absorbed, caller).await;
        let target = SpaceSelector::id(&parties.target.id);
        (self.start_removal(daemon, &target, unsaved, caller, parties.release_holds)).await?;
        Ok(absorbed)
    }
}

#[cfg(test)]
mod tests;
