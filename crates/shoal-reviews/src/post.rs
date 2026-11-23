//! Posting a finished PR review on the PR as a comment review by the
//! workspace's agent account: findings whose last line the PR's diff shows
//! become line comments, the rest go in the body with the summary.
use std::fmt::Write as _;

use anyhow::Result;
use shoal_forge::items::{
    item::ItemRequests,
    review::{HeadMoved, diff::DiffRequests},
};
use shoal_refs::CommitId;

use crate::{
    Reviews,
    daemon::Daemon,
    forge::items::{
        link::ItemKind,
        reference::ItemRef,
        review::{LineComment, NewReview, ReviewVerdict, Side},
    },
    log,
    model::Workspace,
    reviews::{AUTOMATED_NOTE, Posted, Review, ReviewTarget, Severity, severity_counts},
    store,
};
use shoal_protocol::{code::CodeAt, threads::Thread};

impl Reviews {
    /// Post finished review `review` on its PR when its target asks to, and
    /// record what happened. It never approves or requests changes.
    pub(crate) async fn post(&self, daemon: &dyn Daemon, review: &Review, workspace: &Workspace) {
        let (
            ReviewTarget::Pr {
                url, post: true, ..
            },
            Some(head),
        ) = (&review.target, &review.head)
        else {
            return;
        };
        let posted = match submit(daemon, review, workspace, url, head).await {
            Ok(url) => Posted::Posted { url },
            Err(error) => {
                log!("posting review {}: {error:#}", review.id);
                Posted::Failed {
                    error: format!("{error:#}"),
                }
            }
        };
        let id = review.id;
        if let Err(error) = self
            .store
            .run(move |db| store::set_posted(db, id, &posted))
            .await
        {
            log!("posting review {id}: {error:#}");
        }
    }
}

/// Submit the review for the commit it covered, `head`, bound to it: when
/// the PR's head moved, before Shoal read it or before the forge was asked,
/// every finding goes in the body with a note that it is outdated. Any
/// other failure is not retried, since the review may have reached the
/// forge.
async fn submit(
    daemon: &dyn Daemon,
    review: &Review,
    workspace: &Workspace,
    pull: &ItemRef,
    head: &CommitId,
) -> Result<String> {
    let (forge, account) = daemon.forge_account(workspace).await?;
    pull.belongs_to(&forge)?;
    let current = forge
        .read(&account, ItemKind::Pr, pull.pull()?.get())
        .await?;
    let moved = current.head_sha.as_ref() != Some(head);
    let diff = match moved {
        true => None,
        false => Some(forge.pr_diff(&account, &current).await?),
    };
    let shows = |thread: &Thread| match (reviewed(thread), &diff) {
        (Some((path, Some(line))), Some(diff)) => diff.shows(path, line),
        _ => false,
    };
    let url = pull.to_string();
    let posted = new_review(review, shows, moved.then_some(head));
    match daemon.post(workspace, pull, posted).await {
        Ok(()) => Ok(url),
        // Nothing was sent, and the lines no longer match.
        Err(error) if error.downcast_ref::<HeadMoved>().is_some() => {
            let posted = new_review(review, |_| false, Some(head));
            daemon.post(workspace, pull, posted).await?;
            Ok(url)
        }
        Err(error) => Err(error),
    }
}

/// The comment review of `review`: line comments for the findings `inline`
/// accepts, the summary and every other finding in the body, and the
/// automated-review note when there are findings. A review of `outdated`, a
/// commit newer ones were pushed over, says so; any other is bound to the
/// reviewed commit.
fn new_review(
    review: &Review,
    inline: impl Fn(&Thread) -> bool,
    outdated: Option<&CommitId>,
) -> NewReview {
    let (placed, elsewhere): (Vec<&Thread>, Vec<&Thread>) =
        review.threads.iter().partition(|thread| inline(thread));
    let mut body = review.summary.clone().unwrap_or_default().trim().to_owned();
    if !elsewhere.is_empty() {
        let heading = match placed.is_empty() {
            true => "Findings:",
            false => "Findings outside the diff:",
        };
        let _ = write!(body, "\n\n{heading}");
        for thread in &elsewhere {
            let _ = write!(body, "\n\n{}", render(thread, true));
        }
    }
    let tally = match severity_counts(&review.threads) {
        counts if counts.is_empty() => "no findings".to_owned(),
        counts => counts,
    };
    let reviewer = review.reviewed_by();
    let _ = write!(body, "\n\n_Reviewed by {reviewer}: {tally}._");
    if let Some(head) = outdated {
        let _ = write!(
            body,
            "\n\n_Reviewed commit {}; newer commits were pushed during the review._",
            head.short()
        );
    }
    if !review.threads.is_empty() {
        let _ = write!(body, "\n\n_{AUTOMATED_NOTE}_");
    }
    NewReview {
        verdict: ReviewVerdict::Comment,
        body,
        // A review of the current head is bound to it, so a push before the
        // forge receives it refuses it; one already outdated says so.
        commit: review.head.clone().filter(|_| outdated.is_none()),
        comments: placed
            .into_iter()
            .filter_map(|thread| {
                let (path, line) = reviewed(thread)?;
                Some(LineComment {
                    path: path.to_owned(),
                    line: line?,
                    side: Side::New,
                    body: render(thread, false),
                })
            })
            .collect(),
    }
}

/// The file a finding's thread is about in the reviewed commit, and the
/// last line of its lines there, counted from 1, which a line comment
/// takes.
fn reviewed(thread: &Thread) -> Option<(&str, Option<u64>)> {
    let CodeAt { path, lines, .. } = &thread.about()?.original;
    Some((
        path,
        lines.map(|lines| u64::from(lines.end.max(lines.start + 1))),
    ))
}

/// A finding's thread as the forge shows it: its severity, title and
/// opening post, with its location when it is not a comment on that line.
fn render(thread: &Thread, located: bool) -> String {
    let label = match thread.finding.and_then(|finding| finding.severity) {
        Some(Severity::Blocker) => "Blocker",
        Some(Severity::ShouldFix) => "Should fix",
        Some(Severity::Nit) | None => "Nit",
    };
    let location = match (located, thread.about()) {
        (true, Some(code)) => format!(" `{}`", code.original),
        _ => String::new(),
    };
    let body = thread
        .posts
        .first()
        .map_or("", |post| post.post.text.as_str());
    format!("**{label}**{location} — {}\n\n{body}", thread.name)
}

#[cfg(test)]
mod tests;
