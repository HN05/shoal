//! Showing reviews: `review list`, `view review`, `watch review`, and the
//! latest review `shoal view` adds after a workspace's linked items.
use anyhow::Result;

use crate::client::reviews;
use crate::client::space_names::SpaceSelector;
use crate::{
    cli::{
        context::Context,
        installed::arguments,
        output::{Palette, Style},
        ui::{self, Fallback},
    },
    installed::Executable,
    reviews::{Finding, Posted, Review, ReviewId, ReviewState, ReviewTarget, Severity},
};

/// List the reviews of the space, a chat's those of its linked PRs, else
/// the repository's, else every one, newest first.
pub(in crate::cli::commands) async fn list(
    ctx: &Context,
    workspace: Option<SpaceSelector>,
    repository: Option<String>,
) -> Result<i32> {
    if repository.is_none()
        && let Some(chat) = super::chat(ctx, workspace.as_ref()).await?
    {
        let reviews = super::chat_reviews(ctx, &chat).await?;
        ctx.show(&reviews, |reviews| render_list(reviews, ctx.json))?;
        return Ok(0);
    }
    let workspace = match (workspace, &repository) {
        (Some(workspace), _) => Some(workspace),
        (None, Some(_)) => None,
        (None, None) => ui::select_workspace(ctx, None, Fallback::CurrentDirectoryOnly)
            .await
            .ok(),
    };
    let repository = match repository {
        Some(repository) => Some(ui::repository_selector(repository)?),
        None => None,
    };
    let reviews = reviews::list(&ctx.paths, workspace, repository).await?;
    ctx.show(&reviews, |reviews| render_list(reviews, ctx.json))?;
    Ok(0)
}

/// `shoal view review [ID]`: one review in full, by default the workspace's
/// latest.
pub(in crate::cli::commands) async fn view(
    ctx: &Context,
    id: Option<ReviewId>,
    workspace: Option<SpaceSelector>,
) -> Result<i32> {
    let workspace = selected(ctx, id, workspace).await?;
    let review = reviews::show(&ctx.paths, workspace, id).await?;
    ctx.show(&review, |review| render(review, Palette::stdout(ctx.json)))?;
    Ok(0)
}

/// `shoal watch review [ID]`: wait until the review, by default the
/// workspace's latest, is neither queued nor running, then show it. The wait
/// outlasts a daemon restart, and an upgrade's protocol change continues it
/// from the installed binary.
pub(in crate::cli::commands) async fn watch(
    ctx: &Context,
    id: Option<ReviewId>,
    workspace: Option<SpaceSelector>,
) -> Result<i32> {
    let workspace = selected(ctx, id, workspace).await?;
    let executable = Executable::current();
    let id = reviews::await_ended(&ctx.paths, workspace, id)
        .await
        .map_err(|error| executable.resume_or(error, arguments))?;
    let review = reviews::show(&ctx.paths, None, Some(id)).await?;
    ctx.show(&review, |review| render(review, Palette::stdout(ctx.json)))?;
    Ok(0)
}

/// The workspace's newest review for `shoal view`, if it has one; a lookup
/// that fails leaves it out.
pub(in crate::cli::commands) async fn latest(ctx: &Context, workspace: &str) -> Option<Review> {
    reviews::show(&ctx.paths, Some(SpaceSelector::parse(workspace)), None)
        .await
        .ok()
}

/// The workspace a request without a review ID names.
async fn selected(
    ctx: &Context,
    id: Option<ReviewId>,
    workspace: Option<SpaceSelector>,
) -> Result<Option<SpaceSelector>> {
    Ok(match id {
        Some(_) => None,
        None => Some(ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?),
    })
}

fn render_list(reviews: &[Review], json: bool) {
    let palette = Palette::stdout(json);
    if reviews.is_empty() {
        outln!("No reviews.");
        return;
    }
    for review in reviews {
        let unresolved = review.unresolved().count();
        let unresolved = match unresolved {
            0 => String::new(),
            count => format!(", {count} unresolved"),
        };
        let outdated = match review.outdated {
            Some(true) => " (outdated)",
            _ => "",
        };
        let status = palette.paint(state_style(review), review.status());
        outln!(
            "{:>5}  {}  {status}{unresolved}{outdated}  {}",
            review.id,
            review.workspace,
            review.reviewer.as_deref().unwrap_or(&review.agent)
        );
    }
}

/// One review in full.
pub(in crate::cli::commands) fn render(review: &Review, palette: Palette) {
    let heading = format!("Review {} of {}", review.id, review.workspace);
    outln!("{}", palette.paint(Style::Heading, heading));
    outln!(
        "State:    {}",
        palette.paint(state_style(review), review.status())
    );
    if let Some(reason) = review.failure {
        outln!("Failure:  {}", reason.describe());
    }
    match &review.target {
        ReviewTarget::Workspace => outln!("Target:   committed changes"),
        ReviewTarget::Pr { url, .. } => outln!("Target:   {url}"),
    }
    match &review.reviewer {
        Some(reviewer) if *reviewer != review.agent => {
            outln!("Reviewer: {reviewer}, after {} failed", review.agent)
        }
        Some(reviewer) => outln!("Reviewer: {reviewer}"),
        None => outln!("Reviewer: {}", review.agent),
    }
    if let (Some(head), Some(base)) = (&review.head, &review.base) {
        let outdated = match review.outdated {
            Some(true) => ", outdated",
            _ => "",
        };
        outln!(
            "Commit:   {} against {}{outdated}",
            short(head),
            short(base)
        );
    }
    if let Some(started) = review.started_at {
        let took = review
            .duration()
            .map_or_else(String::new, |took| format!(", took {}", duration(took)));
        outln!("Started:  {}{took}", started.local_minutes());
    }
    if let Some(verdict) = review.verdict {
        outln!("Verdict:  {verdict}");
    }
    match &review.posted {
        Some(Posted::Posted { url }) => outln!("Posted:   {url}"),
        Some(Posted::Failed { error }) => outln!(
            "Posted:   {}",
            palette.paint(Style::Warning, format!("not posted: {error}"))
        ),
        None => {}
    }
    if let Some(summary) = &review.summary {
        outln!();
        print_indented(summary, 2);
    }
    if review.state == ReviewState::Finished {
        let unresolved = review.unresolved().count();
        outln!();
        outln!(
            "Findings: {}, {unresolved} unresolved",
            review.findings.len()
        );
        for finding in &review.findings {
            render_finding(finding, palette);
        }
    }
}

fn render_finding(finding: &Finding, palette: Palette) {
    let style = match finding.severity {
        Severity::Blocker => Style::Error,
        Severity::ShouldFix => Style::Warning,
        Severity::Nit => Style::Muted,
    };
    let outcome = finding
        .outcome
        .as_ref()
        .map_or_else(|| "unresolved".to_owned(), |outcome| outcome.describe());
    outln!(
        "  {} · {} · {}:{} · {outcome}",
        finding.id,
        palette.paint(style, finding.severity.as_str()),
        finding.path,
        finding.line
    );
    outln!("    {}", finding.title);
    print_indented(&finding.body, 4);
    for reply in &finding.replies {
        outln!("    Reply, {}:", reply.created_at.local_minutes());
        print_indented(&reply.body, 6);
    }
    if let Some(check) = finding.check {
        outln!(
            "    Review {} checked the fix: {}",
            check.review,
            check.result
        );
    }
}

fn state_style(review: &Review) -> Style {
    match review.state {
        ReviewState::Queued | ReviewState::Running => Style::Active,
        ReviewState::Finished if review.unresolved().next().is_some() => Style::Attention,
        ReviewState::Finished => Style::Success,
        ReviewState::Failed => Style::Error,
        ReviewState::Cancelled => Style::Muted,
    }
}

fn short(commit: &str) -> &str {
    &commit[..commit.len().min(12)]
}

/// A duration in minutes and seconds, such as `3m 12s`.
fn duration(took: std::time::Duration) -> String {
    match took.as_secs() {
        seconds @ ..60 => format!("{seconds}s"),
        seconds => format!("{}m {}s", seconds / 60, seconds % 60),
    }
}

fn print_indented(text: &str, indent: usize) {
    for line in text.trim_end().lines() {
        if line.trim().is_empty() {
            outln!();
        } else {
            outln!("{:indent$}{line}", "");
        }
    }
}

#[cfg(test)]
mod tests;
