//! Show issues and PRs as the forge reports them, without gh or fj.
use anyhow::{Context as _, Result, bail, ensure};

use super::links;
use crate::{
    cli::{
        args::items::ItemArgs,
        browser, client,
        context::Context,
        forge_hosts::ForgeHosts,
        output::{self, Palette, Style},
    },
    env,
    forge::{
        ForgeRepo,
        account::Account,
        capability::Capability,
        ci::checks::CheckLog,
        items::{
            link::{ItemKind, Selection},
            reaction::Reaction,
        },
        review::view::{Details, ItemView, PrDetails, Review},
    },
    model::Selected,
    protocol::{CiMethod, ItemContext},
    repository,
};

/// Exits 1 when an item could not be read; its error is in the output.
pub(super) async fn view(
    ctx: &Context,
    items: ItemArgs,
    no_comments: bool,
    check: Option<String>,
) -> Result<i32> {
    let (context, selection) = links::select_in_context(ctx, items).await?;
    if let Some(check) = check {
        let ItemContext::Workspace(workspace) = context else {
            bail!("--check reads a log through a workspace; run this in one or pass --workspace");
        };
        return show_log(ctx, workspace, selection, check).await;
    }
    let selected = links::select_items(ctx, &context, selection).await?;
    let views = ctx
        .progress("Reading items", async {
            let hosts = ctx.forge_hosts()?;
            let mut views = read(&selected, &hosts, &ctx.forge_account()?, !no_comments).await?;
            if let ItemContext::Workspace(workspace) = &context {
                read_failed_logs(ctx, &hosts, workspace, &mut views).await;
            }
            Ok::<_, anyhow::Error>(views)
        })
        .await?;
    let palette = Palette::stdout(ctx.json);
    let emoji = output::stdout_shows_emoji(ctx.json);
    ctx.show(&views, |views| {
        for (index, view) in views.iter().enumerate() {
            if index > 0 {
                outln!();
            }
            render(view, palette, emoji);
        }
    })?;
    Ok(i32::from(views.iter().any(|view| view.details.is_none())))
}

/// Open the selected items in the browser and print each URL, also when
/// the opener fails, for a terminal elsewhere; workspace processes cannot.
pub(super) async fn open(ctx: &Context, items: ItemArgs) -> Result<i32> {
    ensure!(
        !env::inherits_scope(&ctx.paths.state),
        "workspace processes cannot open a browser; ask the user"
    );
    let (context, selection) = links::select_in_context(ctx, items).await?;
    let selected = links::select_items(ctx, &context, selection).await?;
    let urls: Vec<_> = selected.items.into_iter().map(|item| item.url).collect();
    let failed = urls.iter().find_map(|url| browser::open(url).err());
    ctx.show(&urls, |urls| {
        for url in urls {
            outln!("{url}");
        }
    })?;
    failed.map_or(Ok(0), Err)
}

/// The log of the selected PR's `check`.
async fn show_log(
    ctx: &Context,
    workspace: String,
    selection: Selection,
    check: String,
) -> Result<i32> {
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
    Ok(0)
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

fn render(view: &ItemView, palette: Palette, emoji: bool) {
    let kind = match view.kind {
        ItemKind::Issue => "Issue",
        ItemKind::Pr => "PR",
    };
    match &view.details {
        Some(details) => {
            let heading = format!("{kind} #{}: {}", details.number, details.title);
            outln!("{}", palette.paint(Style::Heading, heading));
            outln!("URL:      {}", view.url);
            render_details(details, palette, emoji);
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

fn render_details(details: &Details, palette: Palette, emoji: bool) {
    match &details.pr {
        Some(pr) => render_pr(&details.state, pr, palette),
        None => outln!("State:    {}", details.state),
    }
    outln!("Author:   {}", details.author);
    outln!("Created:  {}", details.created_at);
    if !details.labels.is_empty() {
        outln!("Labels:   {}", details.labels.join(", "));
    }
    render_reactions(&details.reactions, 0, emoji);
    if !details.body.trim().is_empty() {
        outln!();
        print_indented(&details.body, 2);
    }
    if let Some(comments) = &details.comments {
        outln!();
        outln!("Comments: {}", comments.len());
        for comment in comments {
            outln!(
                "  {} · {} · comment {}",
                comment.author,
                comment.created_at,
                comment.id
            );
            print_indented(&comment.body, 4);
            render_reactions(&comment.reactions, 4, emoji);
        }
    }
    if let Some(reviews) = &details.reviews {
        outln!();
        outln!("Reviews:  {}", reviews.len());
        for review in reviews {
            render_review(review, emoji);
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

fn render_review(review: &Review, emoji: bool) {
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
        render_reactions(&comment.reactions, 6, emoji);
    }
}

/// One line with each reaction's count, marking those the viewer made.
fn render_reactions(reactions: &[Reaction], indent: usize, emoji: bool) {
    if reactions.is_empty() {
        return;
    }
    outln!(
        "{:indent$}Reactions: {}",
        "",
        reactions_text(reactions, emoji)
    );
}

/// Each reaction as its emoji where `emoji` allows and it has one, otherwise
/// by name, with its count.
fn reactions_text(reactions: &[Reaction], emoji: bool) -> String {
    let reactions: Vec<String> = reactions
        .iter()
        .map(|reaction| {
            let shown = emoji
                .then(|| reaction_emoji(&reaction.content))
                .flatten()
                .unwrap_or(&reaction.content);
            let own = if reaction.reacted { " (you)" } else { "" };
            format!("{shown} {}{own}", reaction.count)
        })
        .collect();
    reactions.join(", ")
}

/// The emoji of a reaction GitHub names `content`.
fn reaction_emoji(content: &str) -> Option<&'static str> {
    Some(match content {
        "+1" => "👍",
        "-1" => "👎",
        "laugh" => "😄",
        "hooray" => "🎉",
        "confused" => "😕",
        "heart" => "❤️",
        "rocket" => "🚀",
        "eyes" => "👀",
        _ => return None,
    })
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
