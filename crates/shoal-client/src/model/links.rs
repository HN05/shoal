//! The issues, PRs and CI runs a space links, and selections of them.
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::forge::items::{link::LinkKind, reference::ItemRef};

/// Canonical issue and pull-request links associated with a space, a
/// workspace or chat.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct SpaceLinks {
    /// The assigned issue.
    pub issue: Option<ItemRef>,
    /// The linked issue's title when the linking caller knew it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issue_title: Option<String>,
    /// The assigned PRs.
    pub prs: Vec<ItemRef>,
    /// Issues and PRs the space created through Shoal, oldest first.
    #[serde(default)]
    pub opened: Vec<LinkedItem>,
    /// Issues and PRs linked as related to the work, oldest first.
    #[serde(default)]
    pub related: Vec<LinkedItem>,
}

impl SpaceLinks {
    /// The links a copy of the space keeps: its issues, without the PRs and
    /// CI runs of the original's branch.
    pub fn copied(self) -> Self {
        let issues = |items: Vec<LinkedItem>| {
            items
                .into_iter()
                .filter(|item| item.kind() == LinkKind::Issue)
                .collect()
        };
        Self {
            prs: Vec::new(),
            opened: issues(self.opened),
            related: issues(self.related),
            ..self
        }
    }

    /// Every link with its relation: the assigned issue and PRs, then the
    /// opened and related items.
    pub fn relations(&self) -> Vec<(Relation, LinkedItem)> {
        let issue = self.issue.iter().map(|url| LinkedItem {
            url: url.clone(),
            title: self.issue_title.clone(),
        });
        let prs = self.prs.iter().map(|url| LinkedItem {
            url: url.clone(),
            title: None,
        });
        let assigned = issue.chain(prs).map(|item| (Relation::Assigned, item));
        let opened = (self.opened.iter().cloned()).map(|item| (Relation::Opened, item));
        let related = (self.related.iter().cloned()).map(|item| (Relation::Related, item));
        assigned.chain(opened).chain(related).collect()
    }

    /// The relations under which `linked` is linked.
    pub fn relations_of(&self, linked: &ItemRef) -> Vec<Relation> {
        (self.relations().into_iter())
            .filter(|(_, item)| item.url == *linked)
            .map(|(relation, _)| relation)
            .collect()
    }

    /// Each linked item, the assigned ones first.
    pub fn items(&self) -> Vec<LinkedItem> {
        (self.relations().into_iter())
            .map(|(_, item)| item)
            .collect()
    }
}

shoal_states::states!(
    /// How an issue or PR relates to a workspace.
    Relation: ValueEnum {
        /// The work the workspace exists for; it drives completion and cleanup.
        Assigned => "assigned",
        /// The workspace created the item through Shoal.
        Opened => "opened",
        /// The item informs the work without driving it.
        Related => "related",
    }
);

impl Default for Relation {
    /// Links recorded before relations existed were assignments.
    fn default() -> Self {
        Self::Assigned
    }
}

/// An issue, PR or CI run a space links, with its title when the linking
/// caller knew it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(into = "LinkedFields", try_from = "LinkedFields")]
pub struct LinkedItem {
    pub url: ItemRef,
    pub title: Option<String>,
}

impl LinkedItem {
    pub fn kind(&self) -> LinkKind {
        self.url.kind()
    }

    /// Whether the item belongs to the repository `remote` names, as an
    /// issue opened elsewhere does not.
    pub fn in_repository(&self, remote: &str) -> bool {
        self.url.in_remote(remote)
    }
}

/// How a [`LinkedItem`] serializes, its kind beside its URL.
#[derive(Serialize, Deserialize)]
struct LinkedFields {
    kind: LinkKind,
    url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    title: Option<String>,
}

impl From<LinkedItem> for LinkedFields {
    fn from(item: LinkedItem) -> Self {
        Self {
            kind: item.kind(),
            url: item.url.to_string(),
            title: item.title,
        }
    }
}

impl TryFrom<LinkedFields> for LinkedItem {
    type Error = anyhow::Error;

    fn try_from(fields: LinkedFields) -> anyhow::Result<Self> {
        Ok(Self {
            url: ItemRef::parse_kind(fields.kind, &fields.url)?,
            title: fields.title,
        })
    }
}

/// The items a selection names and the worktree or checkout whose origin
/// names their repository.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Selected {
    pub path: PathBuf,
    pub items: Vec<SelectedItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SelectedItem {
    pub url: ItemRef,
    /// The relations under which the space links the item; none for an
    /// item it does not link.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub relations: Vec<Relation>,
}

impl SelectedItem {
    pub fn kind(&self) -> LinkKind {
        self.url.kind()
    }
}

impl Selected {
    /// The kind of the selected item numbered `number` in the repository
    /// `remote` names, when one is selected; an issue and a PR that share the
    /// number are ambiguous.
    pub fn kind_of(&self, remote: &str, number: u64) -> anyhow::Result<Option<LinkKind>> {
        // A number alone never names a run.
        let mut kinds = self
            .items
            .iter()
            .filter(|item| item.kind().item().is_some())
            .filter(|item| item.url.number().get() == number && item.url.in_remote(remote))
            .map(SelectedItem::kind);
        let kind = kinds.next();
        anyhow::ensure!(
            kinds.all(|other| Some(other) == kind),
            "an issue and a PR #{number} are both linked; put issue or pr before the number"
        );
        Ok(kind)
    }
}

#[cfg(test)]
mod tests;
