//! Posting a finished PR review on the PR as a comment review by the
//! workspace's agent account: findings on lines the PR's diff shows become
//! line comments, the rest go in the body with the summary.
use std::fmt::Write as _;

use anyhow::Result;

use super::store;
use crate::{
    daemon::{log, workspace::Manager},
    forge::{
        items::{action::Action, link::ItemKind},
        review::submit::{HeadMoved, LineComment, NewReview, ReviewVerdict, Side},
    },
    model::Workspace,
    protocol::ItemContext,
    reviews::{AUTOMATED_NOTE, Finding, Posted, Review, ReviewTarget, Severity, severity_counts},
};

impl Manager {
    /// Post finished review `review` on its PR when its target asks to, and
    /// record what happened. It never approves or requests changes.
    pub(super) async fn post_review(&self, review: &Review, workspace: &Workspace) {
        let (
            ReviewTarget::Pr {
                url, post: true, ..
            },
            Some(head),
        ) = (&review.target, &review.head)
        else {
            return;
        };
        let posted = match self.submit_review(review, workspace, url, head).await {
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

    /// Submit the review for the commit it covered, `head`, bound to it:
    /// when the PR's head moved, before Shoal read it or before the forge was
    /// asked, every finding goes in the body with a note that it is outdated.
    /// Any other failure is not retried, since the review may have reached
    /// the forge.
    async fn submit_review(
        &self,
        review: &Review,
        workspace: &Workspace,
        url: &str,
        head: &str,
    ) -> Result<String> {
        let (forge, account) = self.workspace_forge_account(workspace).await?;
        let (number, url) = forge.pull(url)?;
        let current = forge.read(&account, ItemKind::Pr, number).await?;
        let moved = current.head_sha.as_deref() != Some(head);
        let diff = match moved {
            true => None,
            false => Some(forge.pr_diff(&account, &current).await?),
        };
        let shows = |finding: &Finding| {
            diff.as_ref()
                .is_some_and(|diff| diff.shows(&finding.path, u64::from(finding.line)))
        };
        let context = ItemContext::Workspace(workspace.id.clone());
        let action = Action::Review(new_review(review, shows, moved.then_some(head)));
        match self
            .item_action(&context, ItemKind::Pr, Some(url.clone()), action)
            .await
        {
            Ok(_) => Ok(url),
            // Nothing was sent, and the lines no longer match.
            Err(error) if error.downcast_ref::<HeadMoved>().is_some() => {
                let action = Action::Review(new_review(review, |_| false, Some(head)));
                self.item_action(&context, ItemKind::Pr, Some(url.clone()), action)
                    .await?;
                Ok(url)
            }
            Err(error) => Err(error),
        }
    }
}

/// The comment review of `review`: line comments for the findings `inline`
/// accepts, the summary and every other finding in the body, and the
/// automated-review note when there are findings. A review of `outdated`, a
/// commit newer ones were pushed over, says so; any other is bound to the
/// reviewed commit.
pub(super) fn new_review(
    review: &Review,
    inline: impl Fn(&Finding) -> bool,
    outdated: Option<&str>,
) -> NewReview {
    let (placed, elsewhere): (Vec<&Finding>, Vec<&Finding>) =
        review.findings.iter().partition(|finding| inline(finding));
    let mut body = review.summary.clone().unwrap_or_default().trim().to_owned();
    if !elsewhere.is_empty() {
        let heading = match placed.is_empty() {
            true => "Findings:",
            false => "Findings outside the diff:",
        };
        let _ = write!(body, "\n\n{heading}");
        for finding in &elsewhere {
            let _ = write!(body, "\n\n{}", render(finding, true));
        }
    }
    let tally = match severity_counts(&review.findings) {
        counts if counts.is_empty() => "no findings".to_owned(),
        counts => counts,
    };
    let reviewer = review.reviewer.as_deref().unwrap_or(&review.agent);
    let _ = write!(body, "\n\n_Reviewed by {reviewer}: {tally}._");
    if let Some(head) = outdated {
        let _ = write!(
            body,
            "\n\n_Reviewed commit {}; newer commits were pushed during the review._",
            &head[..head.len().min(12)]
        );
    }
    if !review.findings.is_empty() {
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
            .map(|finding| LineComment {
                path: finding.path.clone(),
                line: u64::from(finding.line),
                side: Side::New,
                body: render(finding, false),
            })
            .collect(),
    }
}

/// A finding as the forge shows it, with its location when it is not a
/// comment on that line.
fn render(finding: &Finding, located: bool) -> String {
    let label = match finding.severity {
        Severity::Blocker => "Blocker",
        Severity::ShouldFix => "Should fix",
        Severity::Nit => "Nit",
    };
    let location = match located {
        true => format!(" `{}:{}`", finding.path, finding.line),
        false => String::new(),
    };
    format!(
        "**{label}**{location} — {}\n\n{}",
        finding.title, finding.body
    )
}

#[cfg(test)]
mod tests;
