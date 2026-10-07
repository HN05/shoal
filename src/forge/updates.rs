//! Read-only PR activity through the forge CLI's existing login.
use std::collections::BTreeMap;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use super::strip_bidi_isolates;

mod queries;

crate::state::states!(UpdateKind {
    Comment => "comment",
    CiCompleted => "ci_completed",
    MergeConflict => "merge_conflict",
    Closed => "closed",
    Merged => "merged",
    LookupFailed => "lookup_failed",
});

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Update {
    pub url: String,
    pub kind: UpdateKind,
    pub message: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub delivery: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct Snapshot {
    comments: BTreeMap<String, String>,
    checks: BTreeMap<String, Check>,
    conflict: Option<bool>,
    state: String,
    errors: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Check {
    name: String,
    state: String,
    revision: String,
    complete: bool,
}

impl Snapshot {
    pub(super) fn changes(&self, next: &Self, url: &str) -> Vec<Update> {
        let mut updates = Vec::new();
        let mut emit = |kind, message| {
            updates.push(Update {
                url: url.into(),
                kind,
                message,
                delivery: String::new(),
            })
        };
        if next
            .comments
            .iter()
            .any(|(id, comment)| !comment.is_empty() && self.comments.get(id) != Some(comment))
        {
            emit(UpdateKind::Comment, "PR comments or review changed".into());
        }
        for (id, check) in &next.checks {
            if check.complete && self.checks.get(id) != Some(check) {
                emit(
                    UpdateKind::CiCompleted,
                    format!("{}: {}", check.name, check.state),
                );
            }
        }
        if next.conflict == Some(true) && self.conflict != Some(true) {
            emit(UpdateKind::MergeConflict, "PR has merge conflicts".into());
        }
        if next.state != self.state {
            match next.state.as_str() {
                "closed" => emit(UpdateKind::Closed, "PR closed without merging".into()),
                "merged" => emit(UpdateKind::Merged, "PR merged".into()),
                _ => {}
            }
        }
        for (source, error) in &next.errors {
            if self.errors.get(source) != Some(error) {
                emit(
                    UpdateKind::LookupFailed,
                    format!("{source} lookup failed: {error}"),
                );
            }
        }
        updates
    }

    pub(super) fn retain_failed_checks(&mut self, previous: &Self) {
        if self.conflict.is_none() {
            self.conflict = previous.conflict;
        }
        if self.errors.contains_key("CI and merge conflicts") {
            self.checks = previous.checks.clone();
            self.conflict = previous.conflict;
        }
        if self.errors.contains_key("revision") {
            self.checks = previous.checks.clone();
        }
        for source in ["discussion", "reviews"] {
            if self.errors.contains_key(source)
                && let Some(comment) = previous.comments.get(source)
            {
                self.comments.insert(source.into(), comment.clone());
            }
        }
    }

    fn record_comments(&mut self, source: &str, result: Result<String>) {
        match result {
            Ok(comments) => {
                let comments = strip_bidi_isolates(&comments);
                self.comments.insert(
                    source.into(),
                    if comments.trim() == "No reviews." {
                        String::new()
                    } else {
                        comments
                    },
                );
            }
            Err(error) => {
                self.errors.insert(source.into(), format!("{error:#}"));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn activity_changes_report_finished_checks_without_waiting_for_the_set() {
        let mut pending = Snapshot::default();
        let check = |name: &str, complete: bool| Check {
            name: name.into(),
            state: if complete { "success" } else { "pending" }.into(),
            revision: "1".into(),
            complete,
        };
        pending.checks.insert("rust".into(), check("rust", false));
        pending
            .checks
            .insert("review".into(), check("review", false));
        assert!(Snapshot::default().changes(&pending, "pr").is_empty());
        let mut reviewed = pending.clone();
        reviewed
            .checks
            .insert("review".into(), check("review", true));
        reviewed.comments.insert("review".into(), "finding".into());
        reviewed.conflict = Some(true);
        let updates = pending.changes(&reviewed, "pr");
        assert_eq!(
            updates.iter().map(|u| u.kind).collect::<Vec<_>>(),
            [
                UpdateKind::Comment,
                UpdateKind::CiCompleted,
                UpdateKind::MergeConflict
            ]
        );
        assert_eq!(updates[1].message, "review: success");
        assert!(reviewed.changes(&reviewed, "pr").is_empty());
        let mut closed = reviewed.clone();
        closed.state = "closed".into();
        assert_eq!(reviewed.changes(&closed, "pr")[0].kind, UpdateKind::Closed);
    }
}
