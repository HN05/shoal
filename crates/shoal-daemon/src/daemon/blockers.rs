//! Clearing blockers. Every 30 seconds Shoal looks each forge item that
//! blocks a queued launch or a space up once, and the newest finished run of
//! each blocking CI run's workflow on the default branch once, then forgets
//! the launch blockers that cleared and removes the space blockers that did,
//! with what cleared them. A space blocker clears once that space is done at
//! its current HEAD or removed, as admission decides for a launch.
use std::collections::{HashMap, HashSet};

use anyhow::Result;

use crate::{
    client::{files::Attacher, progress::Progress, space_ids::SpaceId, space_names::SpaceSelector},
    daemon::{log, progress::Cleared, workspace::Manager},
    state::BlockerKind,
};

mod forge;

/// What Shoal tells an agent it stops because its space is blocked, which
/// waits for the agent's restore and so starts its turn there.
pub(crate) const STOPPED_BLOCKED: &str =
    "Shoal stopped you while your work was blocked and restored you to continue it.";

/// A blocker by its kind and target: a space's ID or an item's URL.
pub(crate) type Target = (BlockerKind, String);

/// What a lookup of a blocker found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Lookup {
    /// It cleared, as this says, such as `run <url> passed on main at <commit>`.
    Cleared(String),
    Open,
    /// The lookup failed, which keeps it.
    Failed(String),
}

/// What the forge says of each blocker it was asked about.
pub(crate) type Lookups = HashMap<Target, Lookup>;

impl Manager {
    /// Look up every forge blocker of the queue and of each space's
    /// progress, forget the queue's that cleared, and clear the spaces'.
    pub(crate) async fn poll_blockers(&self) -> Result<()> {
        let mut targets: HashSet<Target> = self.queued_forge_blockers().await?;
        targets.extend(self.progress_forge_blockers().await?);
        let lookups = self.look_up_blockers(targets).await;
        let forgotten = self.forget_cleared_launch_blockers(&lookups).await;
        let cleared = self.clear_progress_blockers(&lookups).await?;
        for space in &cleared {
            self.tell_cleared(space).await;
        }
        // A restore that waited for the space to stop being blocked may be
        // due now.
        self.agent_queue.wake();
        forgotten
    }

    /// Tell the agent of the space whose blockers `cleared` names what
    /// cleared and what still blocks it; an agent stopped while blocked
    /// reads it once it runs again.
    async fn tell_cleared(&self, cleared: &Cleared) {
        let selector = SpaceSelector::id(&cleared.space);
        let progress = match self.space_progress(&selector).await {
            Ok(progress) => progress,
            Err(error) => {
                log!("blockers of {} cleared, unread: {error:#}", cleared.space);
                return;
            }
        };
        let text = cleared_message(cleared, progress.as_ref());
        self.tell(&selector, text).await;
    }

    /// Tell the agent of the space `id`, restored while the space is still
    /// blocked, what blocks it.
    pub(crate) async fn tell_still_blocked(&self, id: &SpaceId) {
        let selector = SpaceSelector::id(id);
        let progress = match self.space_progress(&selector).await {
            Ok(progress) => progress.filter(Progress::blocked),
            Err(error) => {
                log!("blockers of {id} unread for its restore: {error:#}");
                return;
            }
        };
        if let Some(progress) = progress {
            let text = format!("Your work is still blocked:{}", open_blockers(&progress));
            self.tell(&selector, text).await;
        }
    }

    /// Send the agent of the space `selector` names `text`, from Shoal.
    async fn tell(&self, selector: &SpaceSelector, text: String) {
        let sent = self.send_space_message(selector, text, Vec::new(), &Attacher::User);
        if let Err(error) = sent.await {
            log!("blockers of {selector} untold: {error:#}");
        }
    }
}

/// What `cleared` cleared, with what cleared each, and what of `progress`
/// still blocks the space.
fn cleared_message(cleared: &Cleared, progress: Option<&Progress>) -> String {
    let mut text = String::from("What blocked your work cleared:");
    for event in &cleared.cleared {
        let step = (event.step.as_ref()).map_or_else(String::new, |step| format!(" (step {step})"));
        let how = event.cleared.as_deref().unwrap_or("it cleared");
        text += &format!(
            "\n- {}{step}, {}: {how}",
            event.blocker.cause(),
            event.blocker.reason
        );
    }
    if let Some(progress) = progress.filter(|progress| progress.all_blockers().next().is_some()) {
        text += "\nStill open:";
        text += &open_blockers(progress);
    }
    text
}

/// Each blocker of `progress` on a line of its own, with its step and reason.
fn open_blockers(progress: &Progress) -> String {
    (progress.all_blockers())
        .map(|(step, blocker)| {
            let step = step.map_or_else(String::new, |step| format!(" (step {})", step.name));
            format!("\n- {}{step}, {}", blocker.cause(), blocker.reason)
        })
        .collect()
}

#[cfg(test)]
mod tests;
