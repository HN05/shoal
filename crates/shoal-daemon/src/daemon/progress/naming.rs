//! The blockers a progress change names, looked up: a space by its ID,
//! an issue, PR or CI run by its URL, and an issue by its number in the
//! blocked space's repository.
use anyhow::{Context as _, Result, ensure};

use super::{Change, blockers::Named};
use crate::{
    client::{
        progress::{ProgressChange, RecordedBlocker},
        space_names::SpaceSelector,
    },
    daemon::workspace::Manager,
    forge::{
        ForgeRepo,
        items::{
            link::LinkKind,
            reference::{ItemInput, ItemRef},
        },
    },
    model::{Blocker, PresentSpace},
    state::BlockerKind,
};

impl Manager {
    /// `change` to the progress of `space`, with the blocker it names looked
    /// up.
    pub(super) async fn looked_up(
        &self,
        space: &PresentSpace,
        change: ProgressChange,
    ) -> Result<Change> {
        Ok(match change {
            ProgressChange::AddBlocker {
                blocker,
                step,
                reason,
            } => Change::AddBlocker {
                blocker: self.recorded_blocker(space, blocker, reason).await?,
                step,
            },
            ProgressChange::RemoveBlocker { blocker, step } => Change::RemoveBlocker {
                named: self.named_blocker(space, blocker).await,
                step,
            },
            change => Change::Plan(change),
        })
    }

    /// `blocker` of `space`, blocking it for `reason`.
    async fn recorded_blocker(
        &self,
        space: &PresentSpace,
        blocker: Blocker,
        reason: String,
    ) -> Result<RecordedBlocker> {
        let (kind, target, name) = match blocker {
            Blocker::Space(selector) => {
                let blocking = self.space(&SpaceSelector::parse(&selector)?).await?;
                ensure!(blocking.id() != space.id(), "a space cannot block itself");
                let id = blocking.id().to_string();
                (BlockerKind::Space, id, blocking.name().to_string())
            }
            Blocker::Issue(input) => {
                let url = self.issue_url(space, &input).await?.to_string();
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

    /// The blockers of `space` that `blocker` names: by what it says, and by
    /// what that looks up to when it still does, as a removed space no
    /// longer does, which its ID still names.
    async fn named_blocker(&self, space: &PresentSpace, blocker: Blocker) -> Named {
        let (kind, input) = match &blocker {
            Blocker::Space(input) => (BlockerKind::Space, input.clone()),
            Blocker::Issue(input) => (BlockerKind::Issue, input.to_string()),
            Blocker::Pr(input) => (BlockerKind::Pr, input.to_string()),
            Blocker::CiRun(input) => (BlockerKind::CiRun, input.to_string()),
        };
        let mut names = vec![input];
        let found = self.recorded_blocker(space, blocker.clone(), String::new());
        if let Ok(found) = found.await {
            names.push(found.target);
        }
        Named { kind, names }
    }

    /// The issue `input` names: its URL, or its number in the repository
    /// of `space`.
    async fn issue_url(&self, space: &PresentSpace, input: &ItemInput) -> Result<ItemRef> {
        let number = match input {
            ItemInput::Url(issue) => return Ok(issue.clone()),
            ItemInput::Number(number) => *number,
        };
        let repository = (space.repository_id())
            .context("a space without a repository names an issue by its URL")?;
        let repository = self
            .repository(&crate::repository::Selector::from(repository))
            .await?;
        let forge: ForgeRepo = self.checkout_forge(&repository).await?;
        ItemRef::numbered(&forge, LinkKind::Issue, number)
    }
}
