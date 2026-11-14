//! New issues and PRs, as a caller asks to open them.

use serde::{Deserialize, Serialize};
use shoal_refs::BranchName;

use crate::items::attach::Attachment;
use crate::{ForgeRepo, capability::Capability, items::link::ItemKind};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewIssue {
    pub title: String,
    pub body: String,
    pub labels: Vec<String>,
    #[serde(default)]
    pub assignees: Vec<String>,
    /// The milestone's title.
    #[serde(default)]
    pub milestone: Option<String>,
    #[serde(default)]
    pub attachments: Vec<Attachment>,
}

/// What `shoal pr open` was told about a new PR; unset fields take defaults
/// from the workspace.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PullOptions {
    pub title: Option<String>,
    pub body: Option<String>,
    pub base: Option<BranchName>,
    /// Overrides `[pr] draft` when set.
    pub draft: Option<bool>,
    pub labels: Vec<String>,
    pub reviewers: Vec<String>,
    pub assignees: Vec<String>,
    /// The milestone's title.
    pub milestone: Option<String>,
    pub attachments: Vec<Attachment>,
}

/// A PR to open from `head` into `base`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewPull {
    pub title: String,
    pub body: String,
    pub head: BranchName,
    /// `owner/repository` of the fork `head` is in; unset for this repository.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_repository: Option<String>,
    pub base: BranchName,
    pub draft: bool,
    pub labels: Vec<String>,
    pub reviewers: Vec<String>,
    #[serde(default)]
    pub assignees: Vec<String>,
    /// The milestone's title.
    #[serde(default)]
    pub milestone: Option<String>,
    pub attachments: Vec<Attachment>,
}

impl NewIssue {
    /// The capabilities the forge needs to open the issue, besides
    /// attachments, which uploads check.
    pub fn needs(&self) -> Vec<Capability> {
        let labels = (!self.labels.is_empty()).then_some(Capability::Labels);
        let assignment = assignment_needs(ItemKind::Issue, &self.assignees, &self.milestone);
        labels.into_iter().chain(assignment).collect()
    }
}

impl NewPull {
    /// The capabilities the forge needs to open the PR, besides attachments.
    pub fn needs(&self) -> Vec<Capability> {
        let labels = (!self.labels.is_empty()).then_some(Capability::Labels);
        let draft = self.draft.then_some(Capability::DraftPrs);
        let fork = self
            .head_repository
            .is_some()
            .then_some(Capability::ForkPrs);
        let assignment = assignment_needs(ItemKind::Pr, &self.assignees, &self.milestone);
        labels
            .into_iter()
            .chain(draft)
            .chain(fork)
            .chain(assignment)
            .collect()
    }
}

/// The capabilities a new item of `kind` needs for its assignees and
/// milestone.
fn assignment_needs(
    kind: ItemKind,
    assignees: &[String],
    milestone: &Option<String>,
) -> impl Iterator<Item = Capability> {
    let assignees = (!assignees.is_empty()).then_some(Capability::assignees(kind));
    let milestone = milestone.as_ref().map(|_| Capability::milestones(kind));
    assignees.into_iter().chain(milestone)
}

/// A PR tests fill in, from and into empty branch names.
#[cfg(any(test, feature = "test-support"))]
impl Default for NewPull {
    fn default() -> Self {
        Self {
            title: String::new(),
            body: String::new(),
            head: BranchName::from(""),
            head_repository: None,
            base: BranchName::from(""),
            draft: false,
            labels: Vec::new(),
            reviewers: Vec::new(),
            assignees: Vec::new(),
            milestone: None,
            attachments: Vec::new(),
        }
    }
}

impl ForgeRepo {
    /// Another repository on the same forge, such as a fork.
    pub fn sibling(&self, path: &str) -> ForgeRepo {
        ForgeRepo {
            host: self.host.clone(),
            path: path.to_owned(),
            kind: self.kind,
            web_scheme: self.web_scheme,
        }
    }
}

#[cfg(test)]
mod tests;
