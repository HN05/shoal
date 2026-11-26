//! `review ls`, `view review`, `watch review`, and the latest review `shoal
//! view` shows.
use anyhow::Result;

use crate::client::reviews;
use crate::{
    cli::{
        context::Context,
        output::{Palette, Style},
        ui::{self, Fallback},
    },
    installed::{Executable, arguments::arguments},
    reviews::{Posted, Review, ReviewId, ReviewState, ReviewTarget, Severity},
};
use shoal_protocol::{space_names::SpaceSelector, threads::Thread};

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
    let reviews = reviews::list(workspace, repository)
        .send(&ctx.paths)
        .await?;
    ctx.show(&reviews, |reviews| render_list(reviews, ctx.json))?;
    Ok(0)
}

pub(in crate::cli::commands) async fn view(
    ctx: &Context,
    id: Option<ReviewId>,
    workspace: Option<SpaceSelector>,
) -> Result<i32> {
    let workspace = selected(ctx, id, workspace).await?;
    let review = reviews::show(workspace, id).send(&ctx.paths).await?;
    ctx.show(&review, |review| render(review, Palette::stdout(ctx.json)))?;
    Ok(0)
}

/// The wait outlasts a daemon restart, and an upgrade's protocol change
/// continues it from the installed binary.
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
    let review = reviews::show(None, Some(id)).send(&ctx.paths).await?;
    ctx.show(&review, |review| render(review, Palette::stdout(ctx.json)))?;
    Ok(0)
}

/// A lookup that fails leaves it out.
pub(in crate::cli::commands) async fn latest(
    ctx: &Context,
    workspace: SpaceSelector,
) -> Option<Review> {
    reviews::show(Some(workspace), None)
        .send(&ctx.paths)
        .await
        .ok()
}

async fn selected(
    ctx: &Context,
    id: Option<ReviewId>,
    workspace: Option<SpaceSelector>,
) -> Result<Option<SpaceSelector>> {
    Ok(match id {
        Some(_) => None,
        None => Some(
            ui::select_workspace(
                ctx,
                workspace,
                Fallback::CurrentDirectory(&super::showing()),
            )
            .await?,
        ),
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
            review.reviewed_by()
        );
    }
}

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
    match (&review.reviewer, review.agent()) {
        (Some(reviewer), Some(agent)) if reviewer != agent => {
            outln!("Reviewer: {reviewer}, after {agent} failed")
        }
        _ => outln!("Reviewer: {}", review.reviewed_by()),
    }
    if let (Some(head), Some(base)) = (&review.head, &review.base) {
        let outdated = match review.outdated {
            Some(true) => ", outdated",
            _ => "",
        };
        outln!(
            "Commit:   {} against {}{outdated}",
            head.short(),
            base.short()
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
    if matches!(review.state, ReviewState::Finished | ReviewState::Pending) {
        let unresolved = review.unresolved().count();
        let threads = match review.agent() {
            Some(_) => "Findings",
            None => "Comments",
        };
        outln!();
        outln!(
            "{threads}: {}, {unresolved} unresolved",
            review.threads.len()
        );
        for thread in &review.threads {
            render_thread(thread, palette);
        }
    }
    if !review.reopened.is_empty() {
        outln!();
        outln!("Reopened, still present: {}", review.reopened.len());
        for thread in &review.reopened {
            render_thread(thread, palette);
        }
    }
}

fn render_thread(thread: &Thread, palette: Palette) {
    let severity = thread.finding.and_then(|finding| finding.severity);
    let style = match severity {
        Some(Severity::Blocker) => Style::Error,
        Some(Severity::ShouldFix) => Style::Warning,
        Some(Severity::Nit) | None => Style::Muted,
    };
    let severity = severity.map_or("", Severity::as_str);
    let place = thread.about().map_or_else(String::new, |code| code.place());
    let outcome = (thread.resolution.as_ref()).map_or_else(
        || "unresolved".to_owned(),
        |resolution| resolution.outcome.describe(),
    );
    outln!(
        "  {} · {} · {place} · {outcome}",
        thread.number,
        palette.paint(style, severity),
    );
    outln!("    {}", thread.one_line());
    for post in thread.posts.iter().filter(|post| post.speaks()) {
        outln!(
            "    {}, {}:",
            post.author(),
            post.created_at.local_minutes()
        );
        print_indented(&post.post.text, 6);
    }
}

fn state_style(review: &Review) -> Style {
    match review.state {
        ReviewState::Queued | ReviewState::Running | ReviewState::Pending => Style::Active,
        ReviewState::Finished if review.unresolved().next().is_some() => Style::Attention,
        ReviewState::Finished => Style::Success,
        ReviewState::Failed => Style::Error,
        ReviewState::Cancelled => Style::Muted,
    }
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
