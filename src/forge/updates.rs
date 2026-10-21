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
    /// The head and base commits the conflict observation covers.
    #[serde(default)]
    conflict_at: MergeRevision,
    /// The revision of the last reported conflict, cleared once the item is
    /// seen mergeable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reported_conflict: Option<MergeRevision>,
    /// Replaces the PR conflict message for a local branch check.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    conflict_message: Option<String>,
    state: String,
    errors: BTreeMap<String, String>,
    /// Unix seconds when each failing source was last reported.
    #[serde(default)]
    reported: BTreeMap<String, u64>,
}

/// The commits a merge check compares; empty when the forge does not say.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct MergeRevision {
    head: String,
    base: String,
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
                snapshot.conflict_at = MergeRevision {
                    head: check.head,
                    base: check.target_commit,
                };
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
        if let Some(message) = self.new_conflict(next) {
            emit(UpdateKind::MergeConflict, message, None);
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

    /// The message for a conflict `next` reports that `self` had not.
    fn new_conflict(&self, next: &Self) -> Option<String> {
        let reported = next.reported_conflict.as_ref()?;
        if self.reported_conflict.as_ref() == Some(reported) {
            return None;
        }
        if let Some(message) = &next.conflict_message {
            return Some(message.clone());
        }
        Some(
            match &self.reported_conflict {
                Some(previous) if previous.head != reported.head => {
                    "PR still has merge conflicts after the push"
                }
                Some(_) => "PR has merge conflicts with new commits on its base branch",
                None => "PR has merge conflicts",
            }
            .into(),
        )
    }

    /// Takes a conflict as reported at once when none was reported since the
    /// item was last mergeable. A conflict that persists across a new head or
    /// base is taken once two polls in a row see it there, since the forge may
    /// still show its result for the previous one. Unknown mergeability keeps
    /// the previous observation.
    pub(super) fn schedule_conflict_reports(&mut self, previous: &Self) {
        if self.conflict.is_none() {
            self.conflict = previous.conflict;
            self.conflict_at = previous.conflict_at.clone();
            self.reported_conflict = previous.reported_conflict.clone();
            return;
        }
        let confirmed = previous.conflict == Some(true) && previous.conflict_at == self.conflict_at;
        self.reported_conflict = match (&previous.reported_conflict, self.conflict) {
            (_, Some(false)) => None,
            (Some(reported), _) if !confirmed => Some(reported.clone()),
            _ => Some(self.conflict_at.clone()),
        };
    }

    pub(super) fn retain_failed_checks(&mut self, previous: &Self) {
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

    /// Take entries in `authored` that `next` shows new or changed as already
    /// seen, and stop tracking each one taken.
    pub(crate) fn adopt_authored(&mut self, next: &Self, authored: &mut BTreeSet<String>) {
        authored.retain(|key| match next.comments.get(key) {
            Some(comment) if self.comments.get(key) != Some(comment) => {
                self.comments.insert(key.clone(), comment.clone());
                false
            }
            _ => true,
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

/// The activity key of `source`'s comment or review `id`.
pub(super) fn activity_key(source: &str, id: &serde_json::Value) -> String {
    format!("{source}:{}", id_text(id))
}

/// The activity key of the discussion comment `id`.
pub(super) fn discussion_key(id: &serde_json::Value) -> String {
    activity_key(DISCUSSION, id)
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
        reviewed.schedule_conflict_reports(&pending);
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
    fn conflicts_are_reported_again_for_a_new_head_or_base_once_confirmed() {
        let poll = |previous: &Snapshot, head: &str, base: &str, conflict| {
            let mut next = Snapshot {
                conflict,
                conflict_at: MergeRevision {
                    head: head.into(),
                    base: base.into(),
                },
                ..Default::default()
            };
            next.schedule_conflict_reports(previous);
            let messages = previous
                .changes(&next, "pr")
                .into_iter()
                .map(|update| update.message)
                .collect::<Vec<_>>();
            (next, messages)
        };
        let (first, messages) = poll(&Snapshot::default(), "a", "m1", Some(true));
        assert_eq!(messages, ["PR has merge conflicts"]);
        // Right after a push the forge may still show the old head's result.
        let (pushed, messages) = poll(&first, "b", "m1", Some(true));
        assert!(messages.is_empty());
        let (confirmed, messages) = poll(&pushed, "b", "m1", Some(true));
        assert_eq!(messages, ["PR still has merge conflicts after the push"]);
        let (persisting, messages) = poll(&confirmed, "b", "m1", Some(true));
        assert!(messages.is_empty());
        let (moved, messages) = poll(&persisting, "b", "m2", Some(true));
        assert!(messages.is_empty());
        let (unknown, messages) = poll(&moved, "", "", None);
        assert!(messages.is_empty());
        let (_, messages) = poll(&unknown, "b", "m2", Some(true));
        assert_eq!(
            messages,
            ["PR has merge conflicts with new commits on its base branch"]
        );
        // A conflict on a mergeable PR is reported at once.
        let (mergeable, messages) = poll(&moved, "c", "m2", Some(false));
        assert!(messages.is_empty());
        let (_, messages) = poll(&mergeable, "c", "m3", Some(true));
        assert_eq!(messages, ["PR has merge conflicts"]);
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
    fn an_authored_reply_to_an_existing_review_is_taken_once_it_changes() {
        let mut previous = Snapshot::default();
        previous.record_comments("reviews", Ok(vec![("7".into(), "1 comment".into())]));
        let mut authored = BTreeSet::from(["reviews:7".to_owned()]);
        // A poll that read the review before the reply keeps waiting for it.
        previous.adopt_authored(&previous.clone(), &mut authored);
        assert_eq!(authored.len(), 1);
        let mut replied = Snapshot::default();
        replied.record_comments("reviews", Ok(vec![("7".into(), "2 comments".into())]));
        previous.adopt_authored(&replied, &mut authored);
        assert!(authored.is_empty());
        assert!(previous.changes(&replied, "pr").is_empty());
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
