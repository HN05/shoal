//! Show issues and PRs as the forge reports them, without gh or fj.
use anyhow::{Context as _, Result};

use crate::{
    cli::{
        args::items::ItemArgs,
        client,
        context::Context,
        forge_hosts::ForgeHosts,
        output::{Palette, Style},
    },
    forge::{
        ForgeRepo,
        account::Account,
        capability::Capability,
        ci::checks::CheckLog,
        items::link::{ItemKind, Selection},
        review::view::{Details, ItemView, PrDetails, Review},
    },
    model::Selected,
    protocol::{CiMethod, ItemMethod},
    repository,
};

/// Exits 1 when an item could not be read; its error is in the output.
pub(super) async fn view(
    ctx: &Context,
    items: ItemArgs,
    no_comments: bool,
    check: Option<String>,
) -> Result<i32> {
    let (workspace, selection) = super::links::select(ctx, items).await?;
    if let Some(check) = check {
        let log: CheckLog = ctx
            .progress(
                "Reading check log",
                client::request(
                    &ctx.paths,
                    CiMethod::CheckLog {
                        workspace,
                        selection,
                        check,
                    },
                ),
            )
            .await?;
        ctx.show(&log, render_log)?;
        return Ok(0);
    }
    let selected: Selected = client::request(
        &ctx.paths,
        ItemMethod::SelectItems {
            workspace: workspace.clone(),
            selection,
        },
    )
    .await?;
    let views = ctx
        .progress("Reading items", async {
            let hosts = ctx.forge_hosts()?;
            let mut views = read(&selected, &hosts, &ctx.forge_account()?, !no_comments).await?;
            read_failed_logs(ctx, &hosts, &workspace, &mut views).await;
            Ok::<_, anyhow::Error>(views)
        })
        .await?;
    let palette = Palette::stdout(ctx.json);
    ctx.show(&views, |views| {
        for (index, view) in views.iter().enumerate() {
            if index > 0 {
                outln!();
            }
            render(view, palette);
        }
    })?;
    Ok(i32::from(views.iter().any(|view| view.details.is_none())))
}

/// Each item as the forge reports it now, in selection order.
async fn read(
    selected: &Selected,
    hosts: &ForgeHosts<'_>,
    account: &Account,
    comments: bool,
) -> Result<Vec<ItemView>> {
    let remote = repository::remote_url_from_path(&selected.path)
        .await?
        .context("item lookup needs an origin remote")?;
    let forge = ForgeRepo::resolve(&remote, hosts).await?;
    Ok(futures_util::future::join_all(
        selected
            .items
            .iter()
            .map(|item| forge.view(account, item.kind, &item.url, comments)),
    )
    .await)
}

async fn read_failed_logs(
    ctx: &Context,
    hosts: &ForgeHosts<'_>,
    workspace: &str,
    views: &mut [ItemView],
) {
    for view in views {
        if !ForgeRepo::resolve_pull_url(&view.url, hosts)
            .await
            .is_ok_and(|forge| forge.supports(Capability::ReadJobLogs))
        {
            continue;
        }
        let Some(pr) = view
            .details
            .as_ref()
            .and_then(|details| details.pr.as_ref())
        else {
            continue;
        };
        for check in &pr.checks {
            if !check.failed() {
                continue;
            }
            let result = client::request::<CheckLog>(
                &ctx.paths,
                CiMethod::CheckLog {
                    workspace: workspace.into(),
                    selection: Selection {
                        kind: Some(ItemKind::Pr),
                        input: Some(view.url.clone()),
                    },
                    check: check.name.clone(),
                },
            )
            .await;
            match result {
                Ok(log) => view.check_logs.push(log),
                Err(error) => view
                    .errors
                    .push(format!("{}: log unavailable: {error:#}", check.name)),
            }
        }
    }
}

pub(super) fn render_log(log: &CheckLog) {
    outln!("{}: {}", log.url, log.name);
    if let Some(job) = &log.job_url {
        outln!("Job: {job}");
    }
    if let Some(step) = &log.failed_step {
        outln!("Failed step: {step}");
    }
    if log.truncated {
        outln!("[showing the last 8 KiB]");
    }
    out!("{}", log.text);
    if !log.text.ends_with('\n') {
        outln!();
    }
}

fn render(view: &ItemView, palette: Palette) {
    let kind = match view.kind {
        ItemKind::Issue => "Issue",
        ItemKind::Pr => "PR",
    };
    match &view.details {
        Some(details) => {
            let heading = format!("{kind} #{}: {}", details.number, details.title);
            outln!("{}", palette.paint(Style::Heading, heading));
            outln!("URL:      {}", view.url);
            render_details(details, palette);
        }
        None => {
            outln!("{}", palette.paint(Style::Heading, kind));
            outln!("URL:      {}", view.url);
        }
    }
    for log in &view.check_logs {
        outln!();
        render_log(log);
    }
    for error in &view.errors {
        outln!("{}", palette.paint(Style::Warning, error));
    }
}

fn render_details(details: &Details, palette: Palette) {
    match &details.pr {
        Some(pr) => render_pr(&details.state, pr, palette),
        None => outln!("State:    {}", details.state),
    }
    outln!("Author:   {}", details.author);
    outln!("Created:  {}", details.created_at);
    if !details.labels.is_empty() {
        outln!("Labels:   {}", details.labels.join(", "));
    }
    if !details.body.trim().is_empty() {
        outln!();
        print_indented(&details.body, 2);
    }
    if let Some(comments) = &details.comments {
        outln!();
        outln!("Comments: {}", comments.len());
        for comment in comments {
            outln!("  {} · {}", comment.author, comment.created_at);
            print_indented(&comment.body, 4);
        }
    }
    if let Some(reviews) = &details.reviews {
        outln!();
        outln!("Reviews:  {}", reviews.len());
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
    outln!("State:    {state}{draft}{conflicts}");
    outln!("Branch:   {} → {}", pr.head, pr.base);
    if let Some(review) = pr.review {
        outln!("Review:   {}", palette.review_state(review));
    }
    outln!("Checks:   {}", pr.checks.len());
    for check in &pr.checks {
        outln!("  {}: {}", check.name, palette.check_result(&check.result));
    }
}

fn render_review(review: &Review) {
    outln!(
        "  {} · {} · {}",
        review.author,
        review.state,
        review.submitted_at
    );
    print_indented(&review.body, 4);
    for comment in &review.comments {
        let line = comment
            .line
            .map_or_else(String::new, |line| format!(":{line}"));
        let thread = if comment.thread == comment.id {
            String::new()
        } else {
            format!(" in thread {}", comment.thread)
        };
        let resolved = match comment.resolved {
            Some(true) => " · resolved",
            Some(false) => " · unresolved",
            None => "",
        };
        outln!(
            "    {}{line} · {} · {} · comment {}{thread}{resolved}",
            comment.path,
            comment.author,
            comment.created_at,
            comment.id
        );
        print_indented(&comment.body, 6);
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
