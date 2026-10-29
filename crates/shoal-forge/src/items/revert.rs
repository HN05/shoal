//! Revert a merged PR: through a forge that opens the revert itself, or
//! with what reverting its merge with Git needs.
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    ForgeRepo,
    account::Account,
    backends::Backend,
    capability::Capability,
    items::{
        action::Edit,
        attach::Attachment,
        create::{Created, PullOptions, pending_edit},
        item::{Current, Item, ItemState, PrSummary},
        link::ItemKind,
    },
};

/// The PR that undoes a merged one.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct NewRevert {
    pub title: String,
    pub body: String,
    pub draft: bool,
    pub labels: Vec<String>,
    pub reviewers: Vec<String>,
    pub attachments: Vec<Attachment>,
}

/// A merged PR and what its merge left on the base branch, for reverting it
/// with Git.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MergedPull {
    pub item: Item,
    /// The commit the merge left at the tip of the base branch: a merge
    /// commit, the squashed commit, or the last of the rebased ones.
    pub merge_commit: String,
    /// The messages of the PR's commits, in the order the forge lists them.
    pub messages: Vec<String>,
}

/// `options` for the revert of `item`, filling what the caller left unset:
/// GitHub's title for a revert, a reference to the reverted PR, and its
/// labels, so a repository's required labels need no repeating.
pub fn revert_options(item: &Item, options: PullOptions) -> PullOptions {
    PullOptions {
        title: Some(
            options
                .title
                .unwrap_or_else(|| format!("Revert \"{}\"", item.title)),
        ),
        body: Some(
            options
                .body
                .unwrap_or_else(|| format!("Reverts #{}", item.number)),
        ),
        labels: if options.labels.is_empty() {
            item.labels.clone()
        } else {
            options.labels
        },
        ..options
    }
}

impl NewRevert {
    /// The revert `options` describe, after [`revert_options`] filled them;
    /// `default_draft` applies unless they choose.
    pub fn new(options: PullOptions, default_draft: bool) -> Self {
        Self {
            title: options.title.unwrap_or_default(),
            body: options.body.unwrap_or_default(),
            draft: options.draft.unwrap_or(default_draft),
            labels: options.labels,
            reviewers: options.reviewers,
            attachments: options.attachments,
        }
    }

    /// The capabilities the forge needs to open the revert, besides
    /// attachments.
    fn needs(&self) -> Vec<Capability> {
        let labels = (!self.labels.is_empty()).then_some(Capability::Labels);
        let draft = self.draft.then_some(Capability::DraftPrs);
        [Capability::RevertPrs]
            .into_iter()
            .chain(labels)
            .chain(draft)
            .collect()
    }
}

impl ForgeRepo {
    /// Have the forge open `revert` for `current`, a PR [`Self::merged`]
    /// read; only failing to open it fails.
    pub async fn revert_pull(
        &self,
        account: &Account,
        current: &Current,
        revert: &NewRevert,
    ) -> Result<Created> {
        self.require_all(revert.needs())?;
        let number = current.item.number;
        let base = current
            .item
            .pr
            .as_ref()
            .context("only PRs revert")?
            .base
            .clone();
        let per_item = self.check_uploads(account, &revert.attachments).await?;
        let (body, pending) = self
            .new_body(account, &revert.body, &revert.attachments, per_item)
            .await?;
        let posted = NewRevert {
            body,
            ..revert.clone()
        };
        let (number, head) = self
            .kind
            .revert(self, account, current, &posted)
            .await
            .with_context(|| format!("could not revert PR #{number}"))?;
        let posted = Item {
            kind: ItemKind::Pr,
            number,
            url: self.item_url(ItemKind::Pr, number)?,
            title: posted.title,
            state: ItemState::Open,
            labels: Vec::new(),
            pr: Some(PrSummary {
                head,
                base,
                draft: posted.draft,
            }),
        };
        let follow_up = Edit {
            add_labels: revert.labels.clone(),
            add_reviewers: revert.reviewers.clone(),
            ..pending_edit(pending)
        };
        Ok(self.finish_posted(account, posted, &follow_up).await)
    }

    /// The merged PR `number` with its merge commit and commit messages.
    pub async fn merged_pull(&self, account: &Account, number: u64) -> Result<MergedPull> {
        let current = self.merged(account, number).await?;
        let merge_commit = current.merge_commit.with_context(|| {
            format!("the forge did not report the commit PR #{number} merged as")
        })?;
        let endpoint = format!("{}/commits", self.item_endpoint(ItemKind::Pr, number));
        let commits = self.pages(account, &endpoint).await?;
        Ok(MergedPull {
            item: current.item,
            merge_commit,
            messages: messages(&commits, self.kind.commit_message())?,
        })
    }

    /// The PR `number`, refused unless it has merged.
    pub async fn merged(&self, account: &Account, number: u64) -> Result<Current> {
        let current = self.read(account, ItemKind::Pr, number).await?;
        ensure!(
            current.item.state == ItemState::Merged,
            "PR #{number} has not merged, so there is nothing to revert"
        );
        Ok(current)
    }
}

/// The message at `pointer` in each of a list of PR commits.
fn messages(commits: &[Value], pointer: &str) -> Result<Vec<String>> {
    commits
        .iter()
        .map(|commit| {
            commit
                .pointer(pointer)
                .and_then(Value::as_str)
                .map(str::to_owned)
                .context("PR commit has no message")
        })
        .collect()
}

#[cfg(test)]
mod tests;
