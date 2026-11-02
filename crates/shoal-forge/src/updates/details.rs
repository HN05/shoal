//! What an item's read reports about it besides its state, such as its
//! title and labels, and the edits between two observations of it.
use serde::{Deserialize, Serialize};

use crate::items::item::Current;

/// The parts of an issue or PR that people edit, as one read of the item
/// reports them. Labels and assignees are sorted, so a forge listing them
/// in another order changes nothing.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ItemDetails {
    pub(crate) title: String,
    pub(crate) body: String,
    pub(crate) labels: Vec<String>,
    pub(crate) assignees: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) milestone: Option<String>,
    /// Whether a PR is a draft; `None` for an issue.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) draft: Option<bool>,
    /// A PR's base branch; `None` for an issue.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) base: Option<String>,
}

/// What an action changed of an item's details: the item as it was read
/// before the action and after it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Edited {
    pub(crate) before: ItemDetails,
    pub(crate) after: ItemDetails,
}

impl ItemDetails {
    pub(crate) fn of(current: &Current) -> Self {
        let item = &current.item;
        Self {
            title: item.title.clone(),
            body: current.body.clone(),
            labels: sorted(&item.labels),
            assignees: sorted(&item.assignees),
            milestone: item.milestone.clone(),
            draft: item.pr.as_ref().map(|pr| pr.draft),
            base: item.pr.as_ref().map(|pr| pr.base.clone()),
        }
    }

    /// The edits from `self` to `next`, such as `Labels added: bug; title
    /// changed to "Fix"`; `None` when nothing changed.
    pub(crate) fn summary(&self, next: &Self) -> Option<String> {
        let mut edits = Vec::new();
        if next.title != self.title {
            edits.push(format!("title changed to \"{}\"", next.title));
        }
        if next.body != self.body {
            edits.push("description edited".into());
        }
        edits.extend(set_changes("labels", &self.labels, &next.labels));
        edits.extend(set_changes("assignees", &self.assignees, &next.assignees));
        if next.milestone != self.milestone {
            edits.push(match &next.milestone {
                Some(milestone) => format!("milestone set to {milestone}"),
                None => "milestone removed".into(),
            });
        }
        if let (Some(was), Some(draft)) = (self.draft, next.draft)
            && was != draft
        {
            edits.push(
                if draft {
                    "marked as draft"
                } else {
                    "marked ready for review"
                }
                .into(),
            );
        }
        if let (Some(was), Some(base)) = (&self.base, &next.base)
            && was != base
        {
            edits.push(format!("base branch changed to {base}"));
        }
        let mut summary = edits.join("; ");
        let first = summary.chars().next()?;
        summary.replace_range(..first.len_utf8(), &first.to_uppercase().to_string());
        Some(summary)
    }

    /// Take each part `edited` changed as it is after the action, and only
    /// the names it added to or removed from a list, so a name someone else
    /// changed since the cursor last saw the item is still reported.
    fn adopt(&mut self, edited: &Edited) {
        let Edited { before, after } = edited;
        adopt(&mut self.title, &before.title, &after.title);
        adopt(&mut self.body, &before.body, &after.body);
        adopt_names(&mut self.labels, &before.labels, &after.labels);
        adopt_names(&mut self.assignees, &before.assignees, &after.assignees);
        adopt(&mut self.milestone, &before.milestone, &after.milestone);
        adopt(&mut self.draft, &before.draft, &after.draft);
        adopt(&mut self.base, &before.base, &after.base);
    }
}

impl Edited {
    /// A PR's change of base branch from `from` to `to`.
    pub fn retarget(from: &str, to: &str) -> Self {
        let base = |branch: &str| ItemDetails {
            base: Some(branch.into()),
            ..ItemDetails::default()
        };
        Self {
            before: base(from),
            after: base(to),
        }
    }

    /// Whether the action changed none of the item's details.
    pub fn is_empty(&self) -> bool {
        self.before == self.after
    }
}

impl super::Snapshot {
    /// Take the details an own action changed as already seen; an item not
    /// observed yet takes them with its first observation.
    pub fn adopt_edited(&mut self, edited: &Edited) {
        if let Some(details) = &mut self.details {
            details.adopt(edited);
        }
    }

    /// Take the details `previous` saw, from a poll that may have read the
    /// item before an own action changed them.
    pub fn adopt_details(&mut self, previous: &Self) {
        if previous.details.is_some() {
            self.details.clone_from(&previous.details);
        }
    }
}

/// `seen` as `after` when the action changed it from `before`.
fn adopt<T: Clone + PartialEq>(seen: &mut T, before: &T, after: &T) {
    if before != after {
        seen.clone_from(after);
    }
}

/// `seen` with the names the action added from `before` to `after` and
/// without those it removed.
fn adopt_names(seen: &mut Vec<String>, before: &[String], after: &[String]) {
    seen.retain(|name| after.contains(name) || !before.contains(name));
    for name in after {
        if !before.contains(name) && !seen.contains(name) {
            seen.push(name.clone());
        }
    }
    seen.sort();
}

fn sorted(names: &[String]) -> Vec<String> {
    let mut names = names.to_vec();
    names.sort();
    names
}

/// The names `next` adds to and removes from `previous`, such as
/// `labels added: bug, docs`.
fn set_changes(noun: &str, previous: &[String], next: &[String]) -> Vec<String> {
    let missing = |from: &[String], set: &[String]| {
        from.iter()
            .filter(|name| !set.contains(name))
            .cloned()
            .collect::<Vec<_>>()
    };
    [
        ("added", missing(next, previous)),
        ("removed", missing(previous, next)),
    ]
    .into_iter()
    .filter(|(_, names)| !names.is_empty())
    .map(|(change, names)| format!("{noun} {change}: {}", names.join(", ")))
    .collect()
}
