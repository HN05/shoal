//! Telling a review's workspace that it ended: its agents receive the
//! threads of its findings or comments in one alert, and whoever
//! coordinates the workspace a one-line notice, unless a space above wrote
//! the review.
use std::fmt::Write as _;

use crate::{
    Reviews,
    daemon::Daemon,
    log,
    reviews::{AUTOMATED_NOTE, Author, Review, ReviewId, ReviewState, Verdict, severity_counts},
    store,
    validate::MAX_AGENT_MESSAGE_LEN,
};
use shoal_protocol::{alerts::AlertKind, threads::Thread};
use shoal_refs::CommitId;

impl Reviews {
    /// Deliver review `id`, which has ended: to the workspace's agents,
    /// unless a process of the workspace cancelled it, and as a notice to
    /// its coordinator, unless a space wrote it, which coordinates the
    /// workspace or is above the one that does.
    pub(crate) async fn deliver(&self, daemon: &dyn Daemon, id: ReviewId) {
        let records = self.records.clone();
        let loaded = (self.store)
            .run(move |db| {
                Ok((
                    store::load(db, &*records, id)?,
                    store::cancelled_by(db, id)?,
                ))
            })
            .await;
        let (review, cancelled_by) = match loaded {
            Ok((Some(review), cancelled_by)) => (review, cancelled_by),
            Ok((None, _)) => return,
            Err(error) => {
                log!("delivering review {id}: {error:#}");
                return;
            }
        };
        if cancelled_by.as_deref() != Some(review.workspace_id.as_str())
            && let Err(error) = daemon
                .message(
                    &review.workspace_id,
                    AlertKind::ReviewFinished,
                    agent_message(&review),
                )
                .await
        {
            log!("delivering review {id}: {error:#}");
        }
        if matches!(review.author, Author::Space { .. }) {
            return;
        }
        let kind = AlertKind::ReviewFinished;
        (daemon.tell_coordinator(
            &review.workspace,
            &review.workspace_id,
            kind,
            notice(&review),
        ))
        .await;
    }
}

/// What the workspace's agents receive: a finished review's summary and
/// findings, with the commands that answer them, or why it ended without.
fn agent_message(review: &Review) -> String {
    let id = review.id;
    match review.state {
        ReviewState::Finished if review.agent().is_none() => comments_message(review),
        ReviewState::Finished => findings_message(review),
        ReviewState::Failed => {
            let reason = review.failure.map_or("", |reason| reason.describe());
            format!("Review {id} failed: {reason}. Run shoal review run to review again.")
        }
        _ => format!("Review {id} was cancelled. Run shoal review run to review again."),
    }
}

fn findings_message(review: &Review) -> String {
    let id = review.id;
    let summary = review.summary.as_deref().unwrap_or_default();
    if review.threads.is_empty() && review.reopened.is_empty() {
        return format!("Review {id} finished with no findings: {summary}");
    }
    let counts = match severity_counts(&review.threads) {
        counts if counts.is_empty() => "no new findings".to_owned(),
        counts => counts,
    };
    let message = format!("Review {id} finished: {counts}. {summary}");
    let ending = format!(
        "\n\nEach finding is a thread you hold. Before fixing one, run shoal thread switch \
         <thread> so the changes you make are its; then resolve it with shoal thread resolve \
         <thread> fixed, false-positive or wont-fix with --reason, or deferred with --issue. \
         Discuss one with the reviewer with shoal send <thread>; leave one that needs the \
         user's decision unresolved and ask in a thread with shoal thread open.\n\n\
         {AUTOMATED_NOTE}"
    );
    let reopened = review.reopened.iter().map(|thread| (thread, true));
    let entries: Vec<(&Thread, bool)> = (review.threads.iter().map(|thread| (thread, false)))
        .chain(reopened)
        .collect();
    listed(message, id, &entries, "findings", &ending)
}

/// What the workspace's agents receive of a review the user or a space
/// wrote: its verdict, summary and comments, which its writer resolves.
fn comments_message(review: &Review) -> String {
    let (id, by) = (review.id, review.author.label());
    let head = review.head.as_ref().map_or("", CommitId::short);
    let summary =
        (review.summary.as_deref()).map_or(String::new(), |summary| format!(" {summary}"));
    if review.threads.is_empty() {
        let verdict = match review.verdict {
            Some(Verdict::Approve) => "approved",
            _ => "reviewed",
        };
        return format!("Review {id}: {by} {verdict} your work at {head}.{summary}");
    }
    let approved = match review.verdict {
        Some(Verdict::Approve) => ", approving it once they are settled",
        _ => "",
    };
    let counts = severity_counts(&review.threads);
    let message =
        format!("Review {id}: {by} reviewed your work at {head}{approved}: {counts}.{summary}");
    let ending = format!(
        "\n\nEach comment is a thread you hold, which {by} resolves. Before working on one, run \
         shoal thread switch <thread> so the changes you make are its; then say what you did \
         with shoal send <thread> \"<text>\", or there why you would not change it."
    );
    let entries: Vec<(&Thread, bool)> = review
        .threads
        .iter()
        .map(|thread| (thread, false))
        .collect();
    listed(message, id, &entries, "comments", &ending)
}

/// `message` followed by as many of the threads `entries` as fit, each
/// marked when reopened, then `ending`, saying how many `things` of
/// review `id` are left out.
fn listed(
    mut message: String,
    id: ReviewId,
    entries: &[(&Thread, bool)],
    things: &str,
    ending: &str,
) -> String {
    let omitted = format!("\n\nshoal view review {id} shows the remaining {things}.");
    for (shown, (thread, reopened)) in entries.iter().enumerate() {
        let entry = entry(thread, *reopened);
        // What is left once the ending, the pointer to the rest and the
        // count of entries left out fit.
        let room = MAX_AGENT_MESSAGE_LEN - ending.len() - omitted.len() - 64;
        if message.len() + entry.len() > room {
            let left = entries.len() - shown;
            let _ = write!(message, "\n\n{left} more {things} are not shown.{omitted}");
            break;
        }
        message.push_str(&entry);
    }
    message.push_str(ending);
    message
}

/// A finding's thread as the alert lists it: its number, severity, place
/// and title, then its opening post, or for one `reopened` the post that
/// reopened it.
fn entry(thread: &Thread, reopened: bool) -> String {
    let (label, post) = match reopened {
        false => ("", thread.posts.first()),
        true => (" (reopened: still present)", thread.last_post()),
    };
    let text = post.map_or("", |post| post.post.text.as_str());
    format!("\n\n{}{label}\n{text}", thread.finding_line())
}

/// The coordinator's one line, such as "review 17 finished: 1 blocker".
fn notice(review: &Review) -> String {
    format!("review {} {}", review.id, review.status())
}

#[cfg(test)]
mod tests;
