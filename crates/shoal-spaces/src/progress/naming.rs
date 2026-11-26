//! Looking up the blockers a progress change names: a space by its ID, an
//! issue, PR or CI run by its URL, and an issue by its number in the blocked
//! space's repository.
use crate::Spaces;
use crate::daemon::Daemon;
use anyhow::{Context as _, Result, ensure};
use shoal_protocol::progress::Blocker;

use super::{Change, blockers::Named};
use crate::{
    forge::{
        ForgeRepo,
        items::{
            link::LinkKind,
            reference::{ItemInput, ItemRef},
        },
    },
    model::Space,
    state::BlockerKind,
};
use shoal_protocol::{
    progress::{ProgressChange, RecordedBlocker},
    space_names::SpaceSelector,
};

impl Spaces {
    pub(super) async fn looked_up(
        &self,
        daemon: &dyn Daemon,
        space: &Space,
        change: ProgressChange,
    ) -> Result<Change> {
        Ok(match change {
            ProgressChange::AddBlocker {
                blocker,
                step,
                reason,
            } => Change::AddBlocker {
                blocker: self
                    .recorded_blocker(daemon, space, blocker, reason)
                    .await?,
                step,
            },
            ProgressChange::RemoveBlocker { blocker, step } => Change::RemoveBlocker {
                named: self.named_blocker(daemon, space, blocker).await,
                step,
            },
            change => Change::Plan(change),
        })
    }

    async fn recorded_blocker(
        &self,
        daemon: &dyn Daemon,
        space: &Space,
        blocker: Blocker,
        reason: String,
    ) -> Result<RecordedBlocker> {
        let (kind, target, name) = match blocker {
            Blocker::Space(selector) => {
                let blocking = self.space(&SpaceSelector::parse(&selector)?).await?;
                ensure!(blocking.id != space.id, "a space cannot block itself");
                let id = blocking.id.to_string();
                (BlockerKind::Space, id, blocking.name.to_string())
            }
            Blocker::Issue(input) => {
                let url = self.issue_url(daemon, space, &input).await?.to_string();
                (BlockerKind::Issue, url.clone(), url)
            }
            Blocker::Pr(url) => (BlockerKind::Pr, url.to_string(), url.to_string()),
            Blocker::CiRun(url) => (BlockerKind::CiRun, url.to_string(), url.to_string()),
        };
        Ok(RecordedBlocker {
            kind,
            target,
            name,
            reason,
            lookup_failed: None,
        })
    }

    /// Matches by what `blocker` says, and by what that looks up to while it
    /// still does: a removed space no longer looks up, but its ID still
    /// names it.
    async fn named_blocker(&self, daemon: &dyn Daemon, space: &Space, blocker: Blocker) -> Named {
        let (kind, input) = match &blocker {
            Blocker::Space(input) => (BlockerKind::Space, input.clone()),
            Blocker::Issue(input) => (BlockerKind::Issue, input.to_string()),
            Blocker::Pr(input) => (BlockerKind::Pr, input.to_string()),
            Blocker::CiRun(input) => (BlockerKind::CiRun, input.to_string()),
        };
        let mut names = vec![input];
        let found = self.recorded_blocker(daemon, space, blocker.clone(), String::new());
        if let Ok(found) = found.await {
            names.push(found.target);
        }
        Named { kind, names }
    }

    /// A number is read in the repository of `space`.
    async fn issue_url(
        &self,
        daemon: &dyn Daemon,
        space: &Space,
        input: &ItemInput,
    ) -> Result<ItemRef> {
        let number = match input {
            ItemInput::Url(issue) => return Ok(issue.clone()),
            ItemInput::Number(number) => *number,
        };
        let repository = (space.repository_id.as_ref())
            .context("a space without a repository names an issue by its URL")?;
        let repository = daemon
            .repository(crate::repository::Selector::from(
                &crate::repository::Selector::from(repository),
            ))
            .await?;
        let forge: ForgeRepo = self.forges.checkout_forge(&repository).await?;
        ItemRef::numbered(&forge, LinkKind::Issue, number)
    }
}
