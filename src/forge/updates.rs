//! Read-only PR activity through the forge CLI's existing login.
use std::collections::BTreeMap;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use super::strip_bidi_isolates;

mod queries;

/// How long a lookup may keep failing with the same error before it is
/// reported again, so a failing source cannot silence a wait indefinitely.
const FAILURE_REPORT_INTERVAL: u64 = 600;

crate::state::states!(UpdateKind {
    Comment => "comment",
    CiCompleted => "ci_completed",
    MergeConflict => "merge_conflict",
    Closed => "closed",
    Merged => "merged",
    Reopened => "reopened",
    LookupFailed => "lookup_failed",
    /// The base workspace's PRs merged; rebase onto their target branch.
    BaseMerged => "base_merged",
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
    /// Unix seconds when each failing source was last reported.
    #[serde(default)]
    reported: BTreeMap<String, u64>,
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
            emit(
                UpdateKind::Comment,
                format!("Comments or reviews changed; read them with shoal view {url}"),
            );
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
                "closed" => emit(
                    UpdateKind::Closed,
                    if url.contains("/issues/") {
                        "Issue closed"
                    } else {
                        "PR closed without merging"
                    }
                    .into(),
                ),
                "open" if self.state == "closed" => {
                    emit(UpdateKind::Reopened, "Item reopened".into())
                }
                "merged" => emit(UpdateKind::Merged, "PR merged".into()),
                _ => {}
            }
        }
        for (source, error) in &next.errors {
            if self.errors.get(source) != Some(error)
                || self.reported.get(source) != next.reported.get(source)
            {
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

    /// Keeps each unchanged failure's report time until the interval passes,
    /// so `changes` reports a persisting failure again once per interval.
    pub(super) fn schedule_failure_reports(&mut self, previous: &Self, now: u64) {
        self.reported = self
            .errors
            .iter()
            .map(|(source, error)| {
                let reported = previous
                    .reported
                    .get(source)
                    .copied()
                    .filter(|&at| {
                        previous.errors.get(source) == Some(error)
                            && now.saturating_sub(at) < FAILURE_REPORT_INTERVAL
                    })
                    .unwrap_or(now);
                (source.clone(), reported)
            })
            .collect();
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

    #[test]
    fn persisting_lookup_failures_are_reported_again_after_the_interval() {
        let failing = |error: &str| {
            let mut snapshot = Snapshot::default();
            snapshot
                .errors
                .insert("CI and merge conflicts".into(), error.into());
            snapshot
        };
        let poll = |previous: &Snapshot, error: &str, now| {
            let mut next = failing(error);
            next.schedule_failure_reports(previous, now);
            let kinds = previous
                .changes(&next, "pr")
                .iter()
                .map(|update| update.kind)
                .collect::<Vec<_>>();
            (next, kinds)
        };
        let (first, kinds) = poll(&Snapshot::default(), "unparsable", 1000);
        assert_eq!(kinds, [UpdateKind::LookupFailed]);
        let (quiet, kinds) = poll(&first, "unparsable", 1000 + FAILURE_REPORT_INTERVAL - 1);
        assert!(kinds.is_empty());
        let (again, kinds) = poll(&quiet, "unparsable", 1000 + FAILURE_REPORT_INTERVAL);
        assert_eq!(kinds, [UpdateKind::LookupFailed]);
        let (changed, kinds) = poll(&again, "timed out", 1000 + FAILURE_REPORT_INTERVAL + 1);
        assert_eq!(kinds, [UpdateKind::LookupFailed]);
        let mut recovered = Snapshot::default();
        recovered.schedule_failure_reports(&changed, 5000);
        assert!(changed.changes(&recovered, "pr").is_empty());
        assert!(recovered.reported.is_empty());
    }

    #[test]
    fn issue_closure_and_reopening_have_item_messages() {
        let mut opened = Snapshot {
            state: "open".into(),
            ..Default::default()
        };
        let closed = Snapshot {
            state: "closed".into(),
            ..Default::default()
        };
        let updates = opened.changes(&closed, "https://forge.example/team/repo/issues/1");
        assert_eq!(updates[0].kind, UpdateKind::Closed);
        assert_eq!(updates[0].message, "Issue closed");
        assert_eq!(
            closed.changes(&opened, "issue")[0].kind,
            UpdateKind::Reopened
        );
        opened.state.clear();
        assert!(
            opened
                .changes(
                    &Snapshot {
                        state: "open".into(),
                        ..Default::default()
                    },
                    "issue"
                )
                .is_empty()
        );
    }
}
