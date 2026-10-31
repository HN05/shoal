//! Telling a review's workspace that it ended: its agents receive the
//! findings themselves, and whoever coordinates the workspace a one-line
//! notice.
use std::fmt::Write as _;

use super::store;
use crate::{
    daemon::{
        log,
        workspace::{Manager, Source},
    },
    reviews::{AUTOMATED_NOTE, Review, ReviewState, severity_counts},
    validate::MAX_AGENT_MESSAGE_LEN,
};

impl Manager {
    /// Deliver review `id`, which has ended: to the workspace's agents,
    /// unless a process of the workspace cancelled it, and as a notice to
    /// its coordinator.
    pub(super) async fn deliver_review(&self, id: i64) {
        let loaded = self
            .store
            .run(move |db| Ok((store::load(db, id)?, store::cancelled_by(db, id)?)))
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
            && let Err(error) = self
                .queue_agent_message(&review.workspace_id, agent_message(&review))
                .await
        {
            log!("delivering review {id}: {error:#}");
        }
        self.tell_coordinator(
            &review.workspace,
            Some(&review.workspace_id),
            None,
            notice(&review),
            Source::Daemon,
        )
        .await;
    }
}

/// What the workspace's agents receive: a finished review's summary and
/// findings, with the commands that answer them, or why it ended without.
pub(super) fn agent_message(review: &Review) -> String {
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
    if review.findings.is_empty() {
        return format!("Review {id} finished with no findings: {summary}");
    }
    let mut message = format!(
        "Review {id} finished: {}. {summary}",
        severity_counts(&review.findings)
    );
    let ending = format!(
        "\n\nResolve each finding with shoal review resolve <finding> <outcome>, or discuss \
         it with shoal review reply <finding>; leave one that needs the user's decision \
         unresolved and ask with shoal questions ask.\n\n{AUTOMATED_NOTE}"
    );
    let omitted = format!("\n\nshoal view review {id} shows the remaining findings.");
    for (shown, finding) in review.findings.iter().enumerate() {
        let entry = format!(
            "\n\n{} {} at {}:{}: {}\n{}",
            finding.id, finding.severity, finding.path, finding.line, finding.title, finding.body
        );
        // What is left once the ending, the pointer to the rest and the
        // count of findings left out fit.
        let room = MAX_AGENT_MESSAGE_LEN - ending.len() - omitted.len() - 64;
        if message.len() + entry.len() > room {
            let left = review.findings.len() - shown;
            let _ = write!(message, "\n\n{left} more findings are not shown.{omitted}");
            break;
        }
        message.push_str(&entry);
    }
    message.push_str(&ending);
    message
}

/// The coordinator's one line, such as "review 17 finished: 1 blocker".
pub(super) fn notice(review: &Review) -> String {
    format!("review {} {}", review.id, review.status())
}

#[cfg(test)]
mod tests;
