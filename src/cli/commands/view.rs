//! Show issues and PRs as the forge reports them, without gh or fj.
use anyhow::Result;

use crate::{
    cli::{
        ItemArgs, client,
        context::Context,
        output::{Palette, Style},
        ui,
    },
    forge::{
        link::{ItemKind, Selection},
        view::{Details, ItemView, PrDetails, Review, Selected},
    },
    protocol::Method,
};

/// Exits 1 when an item could not be read; its error is in the output.
pub(super) async fn view(ctx: &Context, items: ItemArgs, no_comments: bool) -> Result<i32> {
    let selection = Selection::parse(items.kind_or_url, items.item)?;
    let workspace =
        ui::select_workspace(ctx, items.workspace, ui::Fallback::CurrentDirectory).await?;
    let selected: Selected = client::request(
        &ctx.paths,
        Method::SelectItems {
            workspace,
            selection,
        },
    )
    .await?;
    let views = ctx
        .progress("Reading items", selected.view(!no_comments))
        .await?;
    let palette = Palette::stdout(ctx.json);
    ctx.show(&views, |views| {
        for (index, view) in views.iter().enumerate() {
            if index > 0 {
                println!();
            }
            render(view, palette);
        }
    })?;
    Ok(i32::from(views.iter().any(|view| view.details.is_none())))
}

fn render(view: &ItemView, palette: Palette) {
    let kind = match view.kind {
        ItemKind::Issue => "Issue",
        ItemKind::Pr => "PR",
    };
    match &view.details {
        Some(details) => {
            let heading = format!("{kind} #{}: {}", details.number, details.title);
            println!("{}", palette.paint(Style::Heading, heading));
            println!("URL:      {}", view.url);
            render_details(details, palette);
        }
        None => {
            println!("{}", palette.paint(Style::Heading, kind));
            println!("URL:      {}", view.url);
        }
    }
    for error in &view.errors {
        println!("{}", palette.paint(Style::Warning, error));
    }
}

fn render_details(details: &Details, palette: Palette) {
    match &details.pr {
        Some(pr) => render_pr(&details.state, pr, palette),
        None => println!("State:    {}", details.state),
    }
    println!("Author:   {}", details.author);
    println!("Created:  {}", details.created_at);
    if !details.labels.is_empty() {
        println!("Labels:   {}", details.labels.join(", "));
    }
    if !details.body.trim().is_empty() {
        println!();
        print_indented(&details.body, 2);
    }
    if let Some(comments) = &details.comments {
        println!();
        println!("Comments: {}", comments.len());
        for comment in comments {
            println!("  {} · {}", comment.author, comment.created_at);
            print_indented(&comment.body, 4);
        }
    }
    if let Some(reviews) = &details.reviews {
        println!();
        println!("Reviews:  {}", reviews.len());
        for review in reviews {
            render_review(review);
        }
    }
}

fn render_pr(state: &str, pr: &PrDetails, palette: Palette) {
    let conflicts = match pr.merge_conflicts {
        Some(true) => format!(", {}", palette.paint(Style::Error, "merge conflicts")),
        Some(false) => ", no merge conflicts".into(),
        None => String::new(),
    };
    let draft = if pr.draft { ", draft" } else { "" };
    println!("State:    {state}{draft}{conflicts}");
    println!("Branch:   {} → {}", pr.head, pr.base);
    if let Some(review) = pr.review {
        println!("Review:   {}", palette.review_state(review));
    }
    println!("Checks:   {}", pr.checks.len());
    for check in &pr.checks {
        println!("  {}: {}", check.name, palette.check_result(&check.result));
    }
}

fn render_review(review: &Review) {
    println!(
        "  {} · {} · {}",
        review.author, review.state, review.submitted_at
    );
    print_indented(&review.body, 4);
    for comment in &review.comments {
        let line = comment
            .line
            .map_or_else(String::new, |line| format!(":{line}"));
        let resolved = if comment.resolved == Some(true) {
            " (resolved)"
        } else {
            ""
        };
        println!(
            "    {}{line} · {} · {}{resolved}",
            comment.path, comment.author, comment.created_at
        );
        print_indented(&comment.body, 6);
    }
}

fn print_indented(text: &str, indent: usize) {
    for line in text.trim_end().lines() {
        if line.trim().is_empty() {
            println!();
        } else {
            println!("{:indent$}{line}", "");
        }
    }
}
