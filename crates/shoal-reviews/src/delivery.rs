//! Telling a review's workspace that it ended: its agents receive the
//! threads of its findings in one alert, and whoever coordinates the
//! workspace a one-line notice.
use std::fmt::Write as _;

use crate::{
    Reviews,
    daemon::Daemon,
    log,
    reviews::{AUTOMATED_NOTE, Review, ReviewId, ReviewState, severity_counts},
    store,
    validate::MAX_AGENT_MESSAGE_LEN,
};
use shoal_protocol::{alerts::AlertKind, threads::Thread};

impl Reviews {
    /// Deliver review `id`, which has ended: to the workspace's agents,
    /// unless a process of the workspace cancelled it, and as a notice to
    /// its coordinator.
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
    let mut message = format!("Review {id} finished: {counts}. {summary}");
    let ending = format!(
        "\n\nEach finding is a thread you hold. Before fixing one, run shoal thread switch \
         <thread> so the changes you make are its; then resolve it with shoal thread resolve \
         <thread> fixed, false-positive or wont-fix with --reason, or deferred with --issue. \
         Discuss one with the reviewer with shoal send <thread>; leave one that needs the \
         user's decision unresolved and ask in a thread with shoal thread open.\n\n\
         {AUTOMATED_NOTE}"
    );
    let omitted = format!("\n\nshoal view review {id} shows the remaining findings.");
    let reopened = review.reopened.iter().map(|thread| (thread, true));
    let entries: Vec<(&Thread, bool)> = (review.threads.iter().map(|thread| (thread, false)))
        .chain(reopened)
        .collect();
    for (shown, (thread, reopened)) in entries.iter().enumerate() {
        let entry = entry(thread, *reopened);
        // What is left once the ending, the pointer to the rest and the
        // count of findings left out fit.
        let room = MAX_AGENT_MESSAGE_LEN - ending.len() - omitted.len() - 64;
        if message.len() + entry.len() > room {
            let left = entries.len() - shown;
            let _ = write!(message, "\n\n{left} more findings are not shown.{omitted}");
            break;
        }
        message.push_str(&entry);
    }
    message.push_str(&ending);
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
