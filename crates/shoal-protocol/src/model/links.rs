//! The issues, PRs and CI runs a space links, and selections of them.
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::forge::items::{link::LinkKind, reference::ItemRef};

/// Canonical issue and pull-request links associated with a space, a
/// workspace or chat.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct SpaceLinks {
    /// The assigned issues in the order they were assigned, each with its
    /// title when the linking caller knew it.
    pub issues: Vec<LinkedItem>,
    /// The assigned PRs, each with its title when known.
    pub prs: Vec<LinkedItem>,
    /// Issues and PRs the space created through Shoal, oldest first.
    pub opened: Vec<LinkedItem>,
    /// Issues and PRs linked as related to the work, oldest first.
    pub related: Vec<LinkedItem>,
    /// The forge assignees the assigned issues stand for, which a restore
    /// or a move assigns again.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub assignees: Vec<IssueAssignee>,
}

shoal_states::states!(
    /// Who assigns an issue to a space, and so whose account it stands for:
    /// the user, from the user's terminal, or an agent, as a scoped caller,
    /// whose agent then works it.
    Assigner {
        User => "user",
        Agent => "agent",
    }
);

/// The forge assignee an assigned issue stands for: the user's own login,
/// with whether Shoal added it, or the agent account. Shoal takes back a
/// login it added once no space links the issue.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct IssueAssignee {
    pub url: ItemRef,
    pub account: Assigner,
    pub added: bool,
}

impl SpaceLinks {
    /// Whether it links nothing.
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// The links a copy of the space keeps: its issues, without the PRs and
    /// CI runs of the original's branch, or the original's assignees, which
    /// the copy's own links assign again.
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
            assignees: Vec::new(),
            ..self
        }
    }

    /// The first assigned issue, which names a new branch, titles the PR and
    /// starts the agent.
    pub fn issue(&self) -> Option<&LinkedItem> {
        self.issues.first()
    }

    /// The first assigned issue's title, which lists show as the space's
    /// work.
    pub fn issue_title(&self) -> Option<&str> {
        self.issue()?.title.as_deref()
    }

    /// Every link with its relation: the assigned issues and PRs, then the
    /// opened and related items.
    pub fn relations(&self) -> Vec<(Relation, LinkedItem)> {
        let assigned =
            (self.issues.iter().chain(&self.prs).cloned()).map(|item| (Relation::Assigned, item));
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
/// names their repository, unless they were selected from none.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Selected {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
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
