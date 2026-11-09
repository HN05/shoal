//! The issues, PRs and CI runs a space links, and selections of them.
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Canonical issue and pull-request links associated with a space, a
/// workspace or chat.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct SpaceLinks {
    /// The assigned issue.
    pub issue: Option<String>,
    /// The linked issue's title when the linking caller knew it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issue_title: Option<String>,
    /// The assigned PRs.
    pub prs: Vec<String>,
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
        use crate::forge::items::link::LinkKind;
        let issues = |items: Vec<LinkedItem>| {
            items
                .into_iter()
                .filter(|item| item.kind == LinkKind::Issue)
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
        use crate::forge::items::link::LinkKind;
        let issue = self.issue.iter().map(|url| LinkedItem {
            kind: LinkKind::Issue,
            url: url.clone(),
            title: self.issue_title.clone(),
        });
        let prs = self.prs.iter().map(|url| LinkedItem {
            kind: LinkKind::Pr,
            url: url.clone(),
            title: None,
        });
        let assigned = issue.chain(prs).map(|item| (Relation::Assigned, item));
        let opened = (self.opened.iter().cloned()).map(|item| (Relation::Opened, item));
        let related = (self.related.iter().cloned()).map(|item| (Relation::Related, item));
        assigned.chain(opened).chain(related).collect()
    }

    /// The relations under which the item at `url` is linked.
    pub fn relations_of(&self, url: &str) -> Vec<Relation> {
        (self.relations().into_iter())
            .filter(|(_, item)| item.url == url)
            .map(|(relation, _)| relation)
            .collect()
    }

    /// Each linked item once, the first relation's title kept unless a later
    /// one names it.
    pub fn items(&self) -> Vec<LinkedItem> {
        let mut items: Vec<LinkedItem> = Vec::new();
        for (_, item) in self.relations() {
            match items.iter_mut().find(|known| known.url == item.url) {
                Some(known) => known.title = known.title.take().or(item.title),
                None => items.push(item),
            }
        }
        items
    }
}

crate::state::states!(
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

/// An issue, PR or CI run a workspace records without lifecycle effect.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LinkedItem {
    pub kind: crate::forge::items::link::LinkKind,
    pub url: String,
    /// The item's title when the recording caller knew it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

impl LinkedItem {
    /// Whether the item belongs to the repository `remote` names, as an
    /// issue opened elsewhere does not.
    pub fn in_repository(&self, remote: &str) -> bool {
        crate::forge::items::link::item(self.kind, &self.url)
            .is_ok_and(|(repository, _)| repository.is_remote(remote))
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
    pub url: String,
    pub kind: crate::forge::items::link::LinkKind,
    /// The relations under which the space links the item; none for an
    /// item it does not link.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub relations: Vec<Relation>,
}

impl Selected {
    /// The kind of the selected item numbered `number` in the repository
    /// `remote` names, when one is selected; an issue and a PR that share the
    /// number are ambiguous.
    pub fn kind_of(
        &self,
        remote: &str,
        number: u64,
    ) -> anyhow::Result<Option<crate::forge::items::link::LinkKind>> {
        // A number alone never names a run.
        let mut kinds = self
            .items
            .iter()
            .filter(|item| item.kind.item().is_some())
            .filter(|item| {
                crate::forge::items::link::names_item(item.kind, &item.url, remote, number)
            })
            .map(|item| item.kind);
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
