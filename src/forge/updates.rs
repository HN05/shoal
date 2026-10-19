//! Read-only issue and PR activity through the user's existing forge login.
use std::collections::{BTreeMap, BTreeSet};

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::model::ConflictCheck;

mod queries;

/// How long a lookup may keep failing with the same error before it is
/// reported again, so a failing source cannot silence a wait indefinitely.
const FAILURE_REPORT_INTERVAL: u64 = 600;

/// The comment source of an item's discussion, where Shoal posts comments.
pub(super) const DISCUSSION: &str = "discussion";

/// Conflicted paths named in a branch conflict message; `shoal conflicts` lists all.
const MAX_LISTED_FILES: usize = 10;

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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub check: Option<super::pr::state::CheckResult>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct Snapshot {
    comments: BTreeMap<String, String>,
    checks: BTreeMap<String, Check>,
    conflict: Option<bool>,
    /// Replaces the PR conflict message for a local branch check.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    conflict_message: Option<String>,
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
    /// A workspace branch's local merge check, observed like PR mergeability.
    pub(crate) fn branch_conflicts(check: Result<ConflictCheck>) -> Self {
        let mut snapshot = Self::default();
        match check {
            Ok(check) => {
                snapshot.conflict = Some(check.conflicts);
                snapshot.conflict_message = check.conflicts.then(|| {
                    let mut files =
                        check.files[..check.files.len().min(MAX_LISTED_FILES)].join(", ");
                    if check.files.len() > MAX_LISTED_FILES {
                        files.push_str(&format!(
                            " and {} more",
                            check.files.len() - MAX_LISTED_FILES
                        ));
                    }
                    format!("Conflicts with {}: {files}", check.target)
                });
            }
            Err(error) => {
                snapshot
                    .errors
                    .insert("merge conflicts".into(), format!("{error:#}"));
            }
        }
        snapshot
    }

    pub(super) fn changes(&self, next: &Self, url: &str) -> Vec<Update> {
        let mut updates = Vec::new();
        let mut emit = |kind, message, check| {
            updates.push(Update {
                url: url.into(),
                kind,
                message,
                delivery: String::new(),
                check,
            })
        };
        let added_or_edited = next
            .comments
            .iter()
            .any(|(id, comment)| !comment.is_empty() && self.comments.get(id) != Some(comment));
        let deleted = self
            .comments
            .iter()
            .any(|(id, comment)| !comment.is_empty() && !next.comments.contains_key(id));
        if added_or_edited || deleted {
            emit(
                UpdateKind::Comment,
                format!("Comments or reviews changed; read them with shoal view {url}"),
                None,
            );
        }
        for (id, check) in &next.checks {
            if check.complete && self.checks.get(id) != Some(check) {
                emit(
                    UpdateKind::CiCompleted,
                    format!("{}: {}", check.name, check.state),
                    Some(super::pr::state::CheckResult {
                        name: check.name.clone(),
                        result: check.state.to_ascii_lowercase(),
                    }),
                );
            }
        }
        if next.conflict == Some(true) && self.conflict != Some(true) {
            let message = next.conflict_message.as_deref();
            emit(
                UpdateKind::MergeConflict,
                message.unwrap_or("PR has merge conflicts").into(),
                None,
            );
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
                    None,
                ),
                "open" if self.state == "closed" => {
                    emit(UpdateKind::Reopened, "Item reopened".into(), None)
                }
                "merged" => emit(UpdateKind::Merged, "PR merged".into(), None),
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
                    None,
                );
            }
        }
        updates
    }

    pub(super) fn retain_failed_checks(&mut self, previous: &Self) {
        if self.conflict.is_none() {
            self.conflict = previous.conflict;
        }
        if self.errors.contains_key("CI") {
            self.checks = previous.checks.clone();
        }
        for source in [DISCUSSION, "reviews"] {
            if self.errors.contains_key(source) {
                let prefix = format!("{source}:");
                self.comments.extend(
                    previous
                        .comments
                        .iter()
                        .filter(|(key, _)| key.starts_with(&prefix))
                        .map(|(key, comment)| (key.clone(), comment.clone())),
                );
            }
        }
    }

    /// Take comments in `authored` that `next` shows for the first time as
    /// already seen, and stop tracking each one `next` shows.
    pub(crate) fn adopt_authored(&mut self, next: &Self, authored: &mut BTreeSet<String>) {
        authored.retain(|key| match next.comments.get(key) {
            Some(comment) => {
                self.comments
                    .entry(key.clone())
                    .or_insert_with(|| comment.clone());
                false
            }
            None => true,
        });
    }

    pub(crate) fn state(&self) -> &str {
        &self.state
    }

    /// Take `state` as already seen, even before a first observation, whose
    /// other activity is still reported.
    pub(crate) fn adopt_state(&mut self, state: &str) {
        state.clone_into(&mut self.state);
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

    /// Record each comment of `source` by its ID.
    fn record_comments(&mut self, source: &str, result: Result<Vec<(String, String)>>) {
        match result {
            Ok(comments) => {
                self.comments.extend(
                    comments
                        .into_iter()
                        .map(|(id, comment)| (format!("{source}:{id}"), comment)),
                );
            }
            Err(error) => {
                self.errors.insert(source.into(), format!("{error:#}"));
            }
        }
    }
}

/// A forge ID as text: strings without JSON quotes, numbers as digits.
pub(super) fn id_text(id: &serde_json::Value) -> String {
    match id {
        serde_json::Value::String(id) => id.clone(),
        id => id.to_string(),
    }
}

/// The activity key of the discussion comment `id`.
pub(crate) fn discussion_key(id: &serde_json::Value) -> String {
    format!("{DISCUSSION}:{}", id_text(id))
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
            snapshot.errors.insert("CI".into(), error.into());
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
    fn deleting_one_of_several_comments_is_reported() {
        let mut both = Snapshot::default();
        both.record_comments(
            DISCUSSION,
            Ok(vec![("1".into(), "hi".into()), ("2".into(), "typo".into())]),
        );
        let mut one = Snapshot::default();
        one.record_comments(DISCUSSION, Ok(vec![("1".into(), "hi".into())]));
        assert_eq!(both.changes(&one, "pr")[0].kind, UpdateKind::Comment);
        assert!(one.changes(&one, "pr").is_empty());
    }

    #[test]
    fn a_failed_comment_lookup_keeps_that_sources_comments() {
        let mut previous = Snapshot::default();
        previous.record_comments("discussion", Ok(vec![("1".into(), "hi".into())]));
        previous.record_comments("reviews", Ok(vec![("2".into(), "lgtm".into())]));
        let mut failed = Snapshot::default();
        failed.record_comments("discussion", Err(anyhow::anyhow!("timed out")));
        failed.record_comments("reviews", Ok(Vec::new()));
        failed.retain_failed_checks(&previous);
        assert_eq!(failed.comments.keys().collect::<Vec<_>>(), ["discussion:1"]);
        let mut recovered = failed.clone();
        recovered.errors.clear();
        assert!(failed.changes(&recovered, "pr").is_empty());
    }

    #[test]
    fn authored_comments_wake_only_once_they_change() {
        let mut previous = Snapshot::default();
        previous.record_comments(DISCUSSION, Ok(vec![("1".into(), "hi".into())]));
        let mut authored = BTreeSet::from(["discussion:2".to_owned(), "discussion:3".to_owned()]);
        let mut next = previous.clone();
        next.record_comments(DISCUSSION, Ok(vec![("2".into(), "mine".into())]));
        previous.adopt_authored(&next, &mut authored);
        assert!(previous.changes(&next, "pr").is_empty());
        assert_eq!(authored, BTreeSet::from(["discussion:3".to_owned()]));
        let mut edited = next.clone();
        edited.record_comments(DISCUSSION, Ok(vec![("2".into(), "edited".into())]));
        next.adopt_authored(&edited, &mut authored);
        assert_eq!(next.changes(&edited, "pr")[0].kind, UpdateKind::Comment);
        let mut others = next.clone();
        others.record_comments(DISCUSSION, Ok(vec![("4".into(), "theirs".into())]));
        next.adopt_authored(&others, &mut authored);
        assert_eq!(next.changes(&others, "pr")[0].kind, UpdateKind::Comment);
    }

    #[test]
    fn adopted_states_do_not_wake_even_before_a_first_observation() {
        let open = Snapshot {
            state: "open".into(),
            ..Default::default()
        };
        let mut closed = Snapshot {
            state: "closed".into(),
            ..Default::default()
        };
        let mut seen = open.clone();
        seen.adopt_state("closed");
        assert!(seen.changes(&closed, "pr").is_empty());
        closed.record_comments(DISCUSSION, Ok(vec![("1".into(), "theirs".into())]));
        let mut unseen = Snapshot::default();
        unseen.adopt_state("closed");
        let kinds: Vec<_> = unseen
            .changes(&closed, "pr")
            .iter()
            .map(|update| update.kind)
            .collect();
        assert_eq!(kinds, [UpdateKind::Comment]);
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
