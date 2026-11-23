//! Issue, PR and CI run activity as snapshots, and the updates between two
//! of them.
use std::{
    collections::{BTreeMap, BTreeSet},
    time::Duration,
};

use anyhow::Result;
use serde::{Deserialize, Serialize};
use shoal_refs::CommitId;
use shoal_time::UnixTime;

use crate::transport::limits::{RateLimited, rate_limited};
use shoal_forge_types::{
    activity::status::CheckResult,
    ci::check_state::{self, CheckState},
    items::{item::Current, link::LinkKind},
    updates::{SnapshotKind, SnapshotState, Update, UpdateKind},
};

/// How long after a source's failure was reported another is not, however its
/// error changes or it passes in between, so a source failing intermittently
/// cannot end a wait on every poll.
const FAILURE_REPORT_INTERVAL: Duration = Duration::from_secs(600);

/// The failed jobs a finished run's update names; `view ci` lists them all.
pub(crate) const MAX_LISTED_JOBS: usize = 10;

/// The characters of a failed job's name or step its run's update keeps.
const MAX_JOB_TEXT: usize = 120;

/// The comment source of an item's discussion, where Shoal posts comments.
pub(crate) const DISCUSSION: &str = "discussion";

mod comments;
pub(crate) use comments::{Entry, verdict_entries};
mod commits;
mod details;
pub use details::{Edited, ItemDetails, saved_labels};
mod held;
mod watch;
pub use watch::WatchRecord;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Snapshot {
    /// What the snapshot read; `None` before a first observation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) kind: Option<SnapshotKind>,
    pub(crate) comments: BTreeMap<String, Entry>,
    pub(crate) checks: BTreeMap<String, Check>,
    /// The PR's head commit, which its checks run on; `None` for an issue.
    #[serde(default, with = "commits", skip_serializing_if = "Option::is_none")]
    pub(crate) head: Option<CommitId>,
    /// The PR's base commit, which its mergeability compares against; `None`
    /// for an issue or when the forge does not say.
    #[serde(default, with = "commits", skip_serializing_if = "Option::is_none")]
    pub(crate) base: Option<CommitId>,
    /// The commit a merged PR's merge left at the tip of its base branch, as
    /// the forge reports it; `None` for an open PR, an issue, a run, or a
    /// forge that does not say.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) merge_commit: Option<CommitId>,
    pub(crate) conflict: Option<bool>,
    /// The head and base commits the conflict observation covers.
    #[serde(default)]
    pub(crate) conflict_at: MergeRevision,
    /// The revision of the last reported conflict, cleared once the item is
    /// seen mergeable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) reported_conflict: Option<MergeRevision>,
    /// Replaces the PR conflict message for a local branch check.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) conflict_message: Option<String>,
    /// Whether the conflict observation is exact when made, as a local merge
    /// check is, rather than what the forge last computed.
    #[serde(skip)]
    pub(crate) conflict_exact: bool,
    /// The item's or run's state; `None` for a branch, and before a first
    /// observation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) state: Option<SnapshotState>,
    /// What people edit of an issue or PR; `None` for a CI run or branch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) details: Option<ItemDetails>,
    /// A failed run's failed jobs.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) failed_jobs: Vec<FailedJob>,
    /// What tells a run's latest finish from an earlier attempt's, where
    /// the forge says, as [`Run::finish`](shoal_forge_types::ci::run_action::Run::finish)
    /// holds it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) finish: Option<String>,
    pub(crate) errors: BTreeMap<String, String>,
    /// The failure last reported of each source that failed within the
    /// report interval, or fails with that error still.
    #[serde(default)]
    pub(crate) reported: BTreeMap<String, ReportedFailure>,
    /// The rate limit a source's lookup ran into, which leaves the snapshot
    /// incomplete without that source having failed.
    #[serde(skip)]
    pub(crate) limited: Option<RateLimited>,
    /// Whether the lookup read nothing of the item, so the previous
    /// observation stands beside the failure in `errors`.
    #[serde(skip)]
    pub(crate) unread: bool,
}

/// When a source's failure was reported, and its error until the source
/// is read again.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ReportedFailure {
    pub(crate) at: UnixTime,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) error: Option<String>,
}

/// A failed job of a run, with the step that failed where its log names one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct FailedJob {
    pub(crate) name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) step: Option<String>,
}

/// The commits a merge check compares; `None` when the forge does not say.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct MergeRevision {
    #[serde(with = "commits")]
    pub(crate) head: Option<CommitId>,
    #[serde(with = "commits")]
    pub(crate) base: Option<CommitId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Check {
    pub(crate) name: String,
    #[serde(deserialize_with = "check_state::saved")]
    pub(crate) state: CheckState,
    pub(crate) revision: String,
}

/// A CI check on a PR's head, by its name, as a forge lists it; [`Check`]
/// adds the revision it ran at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HeadCheck {
    pub name: String,
    pub state: CheckState,
}

/// A local merge of `head` into `base`; `conflict` reports its conflicts
/// and is empty when it merges cleanly.
pub struct MergeCheck {
    pub head: CommitId,
    pub base: CommitId,
    pub conflict: Option<String>,
}

impl Snapshot {
    /// Retain unavailable activity and schedule reports against the preceding
    /// observation at `now`.
    pub fn normalize(&mut self, previous: &Self, now: UnixTime) {
        if self.unread {
            self.keep_previous(previous);
        } else {
            self.retain_failed_checks(previous);
            self.schedule_conflict_reports(previous);
        }
        self.schedule_failure_reports(previous, now);
    }

    /// Whether the lookup read nothing of the item, only its failure.
    pub fn unread(&self) -> bool {
        self.unread
    }

    /// Take everything but this lookup's failures from `previous`.
    fn keep_previous(&mut self, previous: &Self) {
        *self = Self {
            kind: self.kind,
            errors: std::mem::take(&mut self.errors),
            unread: true,
            ..previous.clone()
        };
    }

    /// A local merge check, observed like PR mergeability.
    pub fn merge_check(check: Result<MergeCheck>) -> Self {
        let mut snapshot = Self::of(SnapshotKind::Branch, None);
        match check {
            Ok(check) => {
                snapshot.conflict = Some(check.conflict.is_some());
                snapshot.conflict_at = MergeRevision {
                    head: Some(check.head),
                    base: Some(check.base),
                };
                snapshot.conflict_message = check.conflict;
                snapshot.conflict_exact = true;
            }
            Err(error) => {
                snapshot
                    .errors
                    .insert("merge conflicts".into(), format!("{error:#}"));
            }
        }
        snapshot
    }

    pub fn changes(&self, next: &Self, url: &str) -> Vec<Update> {
        let mut updates = Vec::new();
        let mut emit = |kind, message, check| {
            updates.push(Update {
                url: url.into(),
                kind,
                message,
                delivery: String::new(),
                check,
                workspace: None,
            })
        };
        if let Some(summary) = comments::summary(&self.comments, &next.comments) {
            emit(
                UpdateKind::Comment,
                format!("{summary}; read them with shoal view {url}"),
                None,
            );
        }
        for (id, check) in &next.checks {
            if check.state.complete() && self.checks.get(id) != Some(check) {
                emit(
                    UpdateKind::CiCompleted,
                    format!("{}: {}", check.name, check.state),
                    Some(check.result()),
                );
            }
        }
        if let Some(message) = self.new_conflict(next) {
            emit(UpdateKind::MergeConflict, message, None);
        }
        if let (Some(seen), Some(details)) = (&self.details, &next.details)
            && let Some(summary) = seen.summary(details)
        {
            emit(UpdateKind::Edited, summary, None);
        }
        if let Some((kind, message)) = self.state_change(next, url) {
            emit(kind, message, None);
        }
        for (source, error) in &next.errors {
            if self.reported.get(source) != next.reported.get(source) {
                emit(
                    UpdateKind::LookupFailed,
                    format!("{source} lookup failed: {error}"),
                    None,
                );
            }
        }
        updates
    }

    /// The update for the state `next` saw, of the item or run at `url`,
    /// when it reports a change from `self`'s.
    fn state_change(&self, next: &Self, url: &str) -> Option<(UpdateKind, String)> {
        use SnapshotState::*;
        if next.state == self.state && !self.finished_again(next) {
            return None;
        }
        let change = match (next.kind, next.state?) {
            (Some(SnapshotKind::Issue), Closed) => (UpdateKind::Closed, "Issue closed".into()),
            (Some(SnapshotKind::Pr), Closed) => {
                (UpdateKind::Closed, "PR closed without merging".into())
            }
            (_, Open) if self.state == Some(Closed) => {
                (UpdateKind::Reopened, "Item reopened".into())
            }
            (_, Merged) => (UpdateKind::Merged, "PR merged".into()),
            (_, state @ (Failed | Finished | Cancelled)) => (
                UpdateKind::CiCompleted,
                format!(
                    "Run {state}{}; read it with shoal view ci {url}",
                    failed_jobs_text(&next.failed_jobs)
                ),
            ),
            _ => return None,
        };
        Some(change)
    }

    /// Whether `next` shows a later attempt of the finished run `self`
    /// finished too, which a rerun between two reads leaves in one state.
    fn finished_again(&self, next: &Self) -> bool {
        self.run_finished()
            && next.run_finished()
            && self.finish.is_some()
            && next.finish != self.finish
    }

    /// Whether `self`, a run read now, shows the finish `known` recorded.
    pub(crate) fn same_finish(&self, known: &Self) -> bool {
        known.run_finished() && self.finish.is_some() && known.finish == self.finish
    }

    /// Take how the run `known` recorded at the same finish stopped, and
    /// its failed jobs.
    pub(crate) fn keep_failure(&mut self, known: &Self) {
        self.state = known.state;
        self.failed_jobs = known.failed_jobs.clone();
    }

    /// Whether the snapshot shows a finished run.
    pub fn run_finished(&self) -> bool {
        matches!(
            self.state,
            Some(SnapshotState::Failed | SnapshotState::Finished | SnapshotState::Cancelled)
        )
    }

    /// Whether the item was seen merged or closed, ending its activity.
    pub fn concluded(&self) -> bool {
        matches!(
            self.state,
            Some(SnapshotState::Merged | SnapshotState::Closed)
        )
    }

    /// Whether the item's activity ended: a PR merged, or, once the
    /// assignment watching it is `done`, an item closed or a run finished.
    /// Until then a finished run may start another attempt.
    pub fn activity_ended(&self, done: bool) -> bool {
        self.state == Some(SnapshotState::Merged)
            || (done && (self.concluded() || self.run_finished()))
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

    /// Takes a conflict as reported once two polls in a row see it at the same
    /// head and base, since right after a push or base change the forge may
    /// still show its result for the previous commits, or report a PR it is
    /// still checking as not mergeable. An exact observation is taken at once.
    /// Unknown mergeability keeps the previous observation.
    fn schedule_conflict_reports(&mut self, previous: &Self) {
        if self.conflict.is_none() {
            self.conflict = previous.conflict;
            self.conflict_at = previous.conflict_at.clone();
            self.reported_conflict = previous.reported_conflict.clone();
            return;
        }
        let confirmed = self.conflict_exact
            || (previous.conflict == Some(true) && previous.conflict_at == self.conflict_at);
        self.reported_conflict = match self.conflict {
            Some(false) => None,
            _ if confirmed => Some(self.conflict_at.clone()),
            _ => previous.reported_conflict.clone(),
        };
    }

    fn retain_failed_checks(&mut self, previous: &Self) {
        if self.errors.contains_key("CI") {
            self.checks = previous.checks.clone();
        }
        // Each comment source records its entries under its own name.
        for source in self.errors.keys() {
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

    /// Take entries in `authored` that `next` shows new or changed as already
    /// seen, and stop tracking each one taken.
    pub(crate) fn adopt_authored(&mut self, next: &Self, authored: &mut BTreeSet<String>) {
        authored.retain(|key| match next.comments.get(key) {
            Some(comment)
                if self.comments.get(key).map(|seen| &seen.fingerprint)
                    != Some(&comment.fingerprint) =>
            {
                self.comments.insert(key.clone(), comment.clone());
                false
            }
            _ => true,
        });
    }

    pub fn state(&self) -> Option<SnapshotState> {
        self.state
    }

    /// Whether the snapshot holds no read of the item yet, as a watch's
    /// does before its first observation.
    pub(crate) fn before_first_read(&self) -> bool {
        self.kind.is_none()
    }

    /// The issue's or PR's title, once a read of it has been recorded.
    pub fn title(&self) -> Option<&str> {
        (self.details.as_ref()).map(|details| details.title.as_str())
    }

    /// Whether `next` shows the PR in or out of draft where `self` showed
    /// the other; false while either has not read its details.
    pub fn draft_changed(&self, next: &Self) -> bool {
        matches!((&self.details, &next.details), (Some(seen), Some(details)) if seen.draft_changed(details))
    }

    /// Whether `next` lacks an assignee `self` showed; false while either
    /// has not read its details.
    pub fn assignee_removed(&self, next: &Self) -> bool {
        matches!((&self.details, &next.details), (Some(seen), Some(details)) if seen.assignee_removed(details))
    }

    /// The PR's head commit, `None` for an issue.
    pub fn head(&self) -> Option<&CommitId> {
        self.head.as_ref()
    }

    /// The commit the PR's merge left at the tip of its base branch, where
    /// the forge reported one for the merged PR.
    pub fn merge_commit(&self) -> Option<&CommitId> {
        self.merge_commit.as_ref()
    }

    /// The PR's base branch, once a read of its details has been recorded.
    pub fn base_branch(&self) -> Option<&shoal_refs::BranchName> {
        (self.details.as_ref()).and_then(|details| details.base.as_ref())
    }

    /// Each check that ran on the head commit, by name. Failures kept from
    /// an earlier head are left out.
    pub fn head_checks(&self) -> impl Iterator<Item = (&str, CheckState)> {
        self.checks
            .values()
            .filter(|check| (self.head.as_ref()).is_none_or(|head| check.revision == head.as_str()))
            .map(|check| (check.name.as_str(), check.state))
    }

    /// Each source the observation could not read, such as CI checks, and why.
    pub fn errors(&self) -> impl Iterator<Item = (&str, &str)> {
        self.errors
            .iter()
            .map(|(source, error)| (source.as_str(), error.as_str()))
    }

    /// Whether the PR's head conflicts with its base; `None` when unknown,
    /// as it is while an observation kept from an earlier head or base stands.
    pub fn head_conflict(&self) -> Option<bool> {
        let covers = |observed: &Option<CommitId>, current: &Option<CommitId>| {
            current.is_none() || observed == current
        };
        self.conflict.filter(|_| {
            covers(&self.conflict_at.head, &self.head) && covers(&self.conflict_at.base, &self.base)
        })
    }

    /// Whether a check on the head commit has not finished.
    pub(crate) fn checks_pending(&self) -> bool {
        self.checks.values().any(|check| !check.state.complete())
    }

    /// Take `state` as already seen, even before a first observation, whose
    /// other activity is still reported.
    pub fn adopt_state(&mut self, state: Option<SnapshotState>) {
        self.state = state;
    }

    /// Keeps each source's reported failure while the source fails with that
    /// error, or until the interval passes, and reports a failing source
    /// without one at `now`; a source read meanwhile forgets the error, so
    /// it keeps only the interval. So `changes` reports a failure that
    /// repeats once, until its source is read or fails differently, since
    /// each repeat would only wake an idle agent with nothing new, and a
    /// source's failures at most once per interval.
    fn schedule_failure_reports(&mut self, previous: &Self, now: UnixTime) {
        let errors = &self.errors;
        self.reported = (previous.reported.iter())
            .filter_map(|(source, reported)| {
                let failing = errors.get(source);
                let repeats = failing.is_some() && failing == reported.error.as_ref();
                let recent = now.duration_since(reported.at) < FAILURE_REPORT_INTERVAL;
                let error = reported.error.clone().filter(|_| failing.is_some());
                let reported = ReportedFailure {
                    at: reported.at,
                    error,
                };
                (repeats || recent).then(|| (source.clone(), reported))
            })
            .collect();
        for (source, error) in &self.errors {
            (self.reported.entry(source.clone())).or_insert_with(|| ReportedFailure {
                at: now,
                error: Some(error.clone()),
            });
        }
    }

    /// Record each comment of `source` by its ID.
    pub(crate) fn record_comments(&mut self, source: &str, result: Result<Vec<(String, Entry)>>) {
        match result {
            Ok(comments) => {
                self.comments.extend(
                    comments
                        .into_iter()
                        .map(|(id, comment)| (format!("{source}:{id}"), comment)),
                );
            }
            Err(error) => self.record_error(source, error),
        }
    }

    /// Record `error` as the failure of `source`, unless a rate limit
    /// caused it.
    fn record_error(&mut self, source: &str, error: anyhow::Error) {
        match rate_limited(&error) {
            Some(limit) => self.limited = Some(limit.clone()),
            None => {
                self.errors.insert(source.into(), format!("{error:#}"));
            }
        }
    }

    /// This snapshot, or the rate limit that left it incomplete.
    pub(crate) fn complete(mut self) -> Result<Self> {
        match self.limited.take() {
            Some(limit) => Err(limit.into()),
            None => Ok(self),
        }
    }
}

/// The failed jobs of a run and the steps that failed, after a colon.
/// Only the first [`MAX_LISTED_JOBS`] are named, each name and step cut to
/// [`MAX_JOB_TEXT`] characters, so the update fits a protocol frame however
/// many jobs failed.
fn failed_jobs_text(jobs: &[FailedJob]) -> String {
    if jobs.is_empty() {
        return String::new();
    }
    let mut listed = jobs
        .iter()
        .take(MAX_LISTED_JOBS)
        .map(|job| {
            let name = shortened(&job.name);
            match &job.step {
                Some(step) => format!("{name} (failed step: {})", shortened(step)),
                None => name,
            }
        })
        .collect::<Vec<_>>();
    if jobs.len() > MAX_LISTED_JOBS {
        listed.push(format!("and {} more", jobs.len() - MAX_LISTED_JOBS));
    }
    format!(": {}", listed.join(", "))
}

/// `text` cut to [`MAX_JOB_TEXT`] characters, marking the cut.
fn shortened(text: &str) -> String {
    match text.char_indices().nth(MAX_JOB_TEXT) {
        Some((end, _)) => format!("{}…", &text[..end]),
        None => text.to_owned(),
    }
}

/// A forge ID as text: strings without JSON quotes, numbers as digits.
pub(crate) fn id_text(id: &serde_json::Value) -> String {
    match id {
        serde_json::Value::String(id) => id.clone(),
        id => id.to_string(),
    }
}

/// The activity key of `source`'s comment or review `id`.
pub(crate) fn activity_key(source: &str, id: &serde_json::Value) -> String {
    format!("{source}:{}", id_text(id))
}

/// The activity key of what a post's `response` describes, which holds
/// its ID in `field` and is recorded under `source`; none without an ID.
pub(crate) fn posted_key(
    (source, field): (&str, &str),
    response: &serde_json::Value,
) -> Option<String> {
    response
        .get(field)
        .filter(|id| id.is_string() || id.is_number())
        .map(|id| activity_key(source, id))
}

/// The activity key of the discussion comment `id`.
pub(crate) fn discussion_key(id: &serde_json::Value) -> String {
    activity_key(DISCUSSION, id)
}

impl Snapshot {
    /// A snapshot of a `kind` in `state`, before anything else is recorded.
    pub(crate) fn of(kind: SnapshotKind, state: Option<SnapshotState>) -> Self {
        Self {
            kind: Some(kind),
            state,
            ..Self::default()
        }
    }

    /// A snapshot of the `kind` item whose lookup failed with `error`.
    pub fn unread_item(kind: LinkKind, error: &anyhow::Error) -> Self {
        Self {
            errors: BTreeMap::from([(kind.noun().into(), format!("{error:#}"))]),
            unread: true,
            ..Self::of(kind.into(), None)
        }
    }

    /// A snapshot of the issue or PR `current`, with a merged PR's merge
    /// commit, before its discussion and checks are recorded.
    pub(crate) fn item(current: &Current) -> Self {
        let item = &current.item;
        Self {
            details: Some(ItemDetails::of(current)),
            merge_commit: current.merge_commit.clone(),
            ..Self::of(item.kind.into(), Some(item.state.into()))
        }
    }

    /// An open PR's mergeability and the CI checks on its head commit, each
    /// under its name. A failed check lookup keeps the previous checks.
    pub(crate) fn record_checks(&mut self, pull: &Current, checks: Result<Vec<HeadCheck>>) {
        let head = (pull.head_sha.as_ref()).map_or_else(String::new, CommitId::to_string);
        let checks = checks.map(|checks| {
            checks
                .into_iter()
                .map(|check| {
                    let entry = Check {
                        state: check.state,
                        name: check.name.clone(),
                        revision: head.clone(),
                    };
                    (check.name, entry)
                })
                .collect()
        });
        self.record_keyed_checks(pull, checks);
    }

    /// An open PR's mergeability and the CI checks on its head commit, each
    /// under its key. A failed check lookup keeps the previous checks.
    pub(crate) fn record_keyed_checks(
        &mut self,
        pull: &Current,
        checks: Result<Vec<(String, Check)>>,
    ) {
        self.head = pull.head_sha.clone();
        self.base = pull.base_sha.clone();
        self.conflict = pull.conflict;
        self.conflict_at = MergeRevision {
            head: pull.head_sha.clone(),
            base: pull.base_sha.clone(),
        };
        match checks {
            Ok(checks) => self.checks.extend(checks),
            Err(error) => self.record_error("CI", error),
        }
    }

    /// The checks as a PR status reports them.
    pub(crate) fn check_results(&self) -> Vec<CheckResult> {
        self.checks.values().map(Check::result).collect()
    }
}

impl Check {
    /// The check as a PR status reports it.
    fn result(&self) -> CheckResult {
        CheckResult {
            name: self.name.clone(),
            result: self.state,
        }
    }
}

#[cfg(test)]
mod tests;
