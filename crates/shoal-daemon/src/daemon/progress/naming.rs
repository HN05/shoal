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
        ForgeRepo, IssueInput,
        items::link::{LinkKind, item},
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
                let url = self.issue_url(space, &input).await?;
                (BlockerKind::Issue, url.clone(), url)
            }
            Blocker::Pr(url) => {
                let url = canonical(LinkKind::Pr, &url)?;
                (BlockerKind::Pr, url.clone(), url)
            }
            Blocker::CiRun(url) => {
                let url = canonical(LinkKind::Ci, &url)?;
                (BlockerKind::CiRun, url.clone(), url)
            }
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
            Blocker::Space(input) => (BlockerKind::Space, input),
            Blocker::Issue(input) => (BlockerKind::Issue, input),
            Blocker::Pr(input) => (BlockerKind::Pr, input),
            Blocker::CiRun(input) => (BlockerKind::CiRun, input),
        };
        let mut names = vec![input.clone()];
        let found = self.recorded_blocker(space, blocker.clone(), String::new());
        if let Ok(found) = found.await {
            names.push(found.target);
        }
        Named { kind, names }
    }

    /// The URL of the issue `input` names: its URL, or its number in the
    /// repository of `space`.
    async fn issue_url(&self, space: &PresentSpace, input: &str) -> Result<String> {
        if IssueInput::parse(input) != IssueInput::Number {
            return canonical(LinkKind::Issue, input);
        }
        let repository = (space.repository_id())
            .context("a space without a repository names an issue by its URL")?;
        let repository = self
            .repository(&crate::repository::Selector::from(repository))
            .await?;
        let forge: ForgeRepo = self.checkout_forge(&repository).await?;
        Ok(forge.issue(input)?.1)
    }
}

/// The canonical URL of the `kind` item `url` names.
fn canonical(kind: LinkKind, url: &str) -> Result<String> {
    let (repository, _) = item(kind, url)?;
    Ok(repository.item(kind, url)?.1)
}
