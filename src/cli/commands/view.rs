//! Show issues, PRs and linked CI runs as the forge reports them, without gh
//! or fj.
use anyhow::{Context as _, Result, bail, ensure};

use serde::Serialize;

use super::{ci, links};
use crate::{
    cli::{
        args::items::{ItemArgs, ViewArgs},
        client,
        context::Context,
        desktop,
        forge_hosts::ForgeHosts,
        output::{self, Palette, Style},
    },
    env,
    forge::{
        ForgeRepo,
        account::Account,
        capability::Capability,
        ci::{
            checks::CheckLog,
            run_view::{JobRef, RunView},
        },
        items::{
            link::{ItemKind, LinkKind, Selection},
            reaction::Reaction,
        },
        review::view::{AutoMerge, Details, ItemView, PrDetails, Review},
    },
    model::Selected,
    protocol::{CiMethod, ItemContext},
    repository,
};

/// Show or open what `args` selects: one CI run as `ci` reads it, otherwise
/// issues, PRs and linked runs.
pub(super) async fn show(ctx: &Context, args: ViewArgs) -> Result<i32> {
    let job = args.job.map(JobRef::Name).or(args.job_id.map(JobRef::Id));
    if args.items.selects_ci() && (args.items.item.is_some() || job.is_some()) {
        ensure!(
            args.check.is_none(),
            "--check reads a PR's check; use --job for a run's"
        );
        ensure!(
            args.repo.name.is_none(),
            "a CI run is read through a workspace; pass --workspace instead of --repo"
        );
        return super::ci::view(ctx, args.items, job, args.web).await;
    }
    ensure!(
        job.is_none(),
        "--job and --job-id read a CI run's job: view ci RUN --job NAME"
    );
    let repo = args.repo.name;
    if args.web {
        open(ctx, args.items, repo).await
    } else {
        view(ctx, args.items, repo, args.no_comments, args.check).await
    }
}

/// Exits 1 when an item could not be read; its error is in the output.
async fn view(
    ctx: &Context,
    items: ItemArgs,
    repo: Option<String>,
    no_comments: bool,
    check: Option<String>,
) -> Result<i32> {
    let (context, selection) = links::select_in_context(ctx, items, repo).await?;
    if let Some(check) = check {
        let ItemContext::Workspace(workspace) = context else {
            bail!("--check reads a log through a workspace; run this in one or pass --workspace");
        };
        return show_log(ctx, workspace, selection, check).await;
    }
    let selected = links::select_items(ctx, &context, selection, true).await?;
    let views = read_views(ctx, &context, &selected, !no_comments).await?;
    ctx.show(&views, |views| render_views(views, ctx.json))?;
    Ok(i32::from(views.iter().any(Shown::failed)))
}

/// What `shoal view` shows of one selected item.
#[derive(Serialize)]
#[serde(untagged)]
enum Shown {
    Item(Box<ItemView>),
    Run(ShownRun),
}

/// A linked CI run, as `view ci` shows it, or why it could not be read.
#[derive(Serialize)]
struct ShownRun {
    kind: LinkKind,
    #[serde(flatten)]
    read: RunRead,
}

#[derive(Serialize)]
#[serde(untagged)]
enum RunRead {
    Read(RunView),
    Failed { url: String, errors: Vec<String> },
}

impl Shown {
    fn failed(&self) -> bool {
        match self {
            Self::Item(view) => view.details.is_none(),
            Self::Run(run) => matches!(run.read, RunRead::Failed { .. }),
        }
    }
}

/// Each selected item as the forge reports it now, in selection order, with
/// failed job logs where a workspace reads them.
async fn read_views(
    ctx: &Context,
    context: &ItemContext,
    selected: &Selected,
    comments: bool,
) -> Result<Vec<Shown>> {
    ctx.progress("Reading items", async {
        let hosts = ctx.forge_hosts()?;
        let own = own_forge(selected, &hosts).await?;
        let account = ctx.forge_account()?;
        let mut items = read(selected, &own, &hosts, &account, comments).await;
        if let ItemContext::Workspace(workspace) = context {
            read_failed_logs(ctx, &own, workspace, &mut items).await;
        }
        let runs = read_runs(ctx, context, selected).await;
        Ok(in_selection_order(selected, items, runs))
    })
    .await
}

/// The issues and PRs, and the runs, each read in selection order, merged
/// back into that order.
fn in_selection_order(
    selected: &Selected,
    items: Vec<ItemView>,
    runs: Vec<ShownRun>,
) -> Vec<Shown> {
    let (mut items, mut runs) = (items.into_iter(), runs.into_iter());
    selected
        .items
        .iter()
        .filter_map(|item| match item.kind.item() {
            Some(_) => items.next().map(|item| Shown::Item(Box::new(item))),
            None => runs.next().map(Shown::Run),
        })
        .collect()
}

/// Each selected run with its jobs and failed jobs' logs, read through the
/// workspace.
async fn read_runs(ctx: &Context, context: &ItemContext, selected: &Selected) -> Vec<ShownRun> {
    let mut runs = Vec::new();
    for item in &selected.items {
        if item.kind != LinkKind::Ci {
            continue;
        }
        let read = match context {
            ItemContext::Workspace(workspace) => ci::read_view(ctx, workspace, &item.url)
                .await
                .map(RunRead::Read),
            ItemContext::Checkout(_) => Err(anyhow::anyhow!(
                "a CI run is read through a workspace; run this in one or pass --workspace"
            )),
        };
        let read = read.unwrap_or_else(|error| RunRead::Failed {
            url: item.url.clone(),
            errors: vec![format!("{error:#}")],
        });
        runs.push(ShownRun {
            kind: LinkKind::Ci,
            read,
        });
    }
    runs
}

/// The items one after another, separated by a blank line.
fn render_views(views: &[Shown], json: bool) {
    let palette = Palette::stdout(json);
    let emoji = output::stdout_shows_emoji(json);
    for (index, view) in views.iter().enumerate() {
        if index > 0 {
            outln!();
        }
        match view {
            Shown::Item(view) => render(view, palette, emoji),
            Shown::Run(ShownRun {
                read: RunRead::Read(view),
                ..
            }) => ci::render_run(view),
            Shown::Run(ShownRun {
                read: RunRead::Failed { url, errors },
                ..
            }) => {
                outln!("Run:      {url}");
                for error in errors {
                    outln!("{}", palette.paint(Style::Warning, error));
                }
            }
        }
    }
}

/// Open the selected items in the browser and print each URL, also when
/// the opener fails, for a terminal elsewhere; workspace processes cannot.
async fn open(ctx: &Context, items: ItemArgs, repo: Option<String>) -> Result<i32> {
    may_open_browser(ctx)?;
    let (context, selection) = links::select_in_context(ctx, items, repo).await?;
    let selected = links::select_items(ctx, &context, selection, true).await?;
    open_urls(
        ctx,
        selected.items.into_iter().map(|item| item.url).collect(),
    )
}

/// Open `urls` in the browser and print each, also when the opener fails.
pub(super) fn open_urls(ctx: &Context, urls: Vec<String>) -> Result<i32> {
    let failed = urls.iter().find_map(|url| desktop::open(url).err());
    ctx.show(&urls, |urls| {
        for url in urls {
            outln!("{url}");
        }
    })?;
    failed.map_or(Ok(0), Err)
}

/// Refuse workspace processes, whose user may be elsewhere.
pub(super) fn may_open_browser(ctx: &Context) -> Result<()> {
    ensure!(
        !env::inherits_scope(&ctx.paths.state),
        "workspace processes cannot open a browser; ask the user"
    );
    Ok(())
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
    own: &ForgeRepo,
    hosts: &ForgeHosts<'_>,
    account: &Account,
    comments: bool,
) -> Vec<ItemView> {
    futures_util::future::join_all(selected.items.iter().filter_map(|item| {
        let kind = item.kind.item()?;
        Some(async move {
            match item_forge(own, kind, &item.url, hosts).await {
                Ok(forge) => forge.view(account, kind, &item.url, comments).await,
                Err(error) => ItemView {
                    url: item.url.clone(),
                    kind,
                    details: None,
                    check_logs: Vec::new(),
                    errors: vec![format!("{error:#}")],
                },
            }
        })
    }))
    .await
}

/// The repository the items were selected in.
async fn own_forge(selected: &Selected, hosts: &ForgeHosts<'_>) -> Result<ForgeRepo> {
    let remote = repository::remote_url_from_path(&selected.path)
        .await?
        .context("item lookup needs an origin remote")?;
    ForgeRepo::resolve(&remote, hosts).await
}

/// The repository the item at `url` is in: `own`, or the other one its URL
/// names.
async fn item_forge(
    own: &ForgeRepo,
    kind: ItemKind,
    url: &str,
    hosts: &ForgeHosts<'_>,
) -> Result<ForgeRepo> {
    match kind {
        ItemKind::Issue if own.issue(url).is_err() => {
            ForgeRepo::resolve_issue_url(url, hosts).await
        }
        ItemKind::Pr if own.pull(url).is_err() => ForgeRepo::resolve_pull_url(url, hosts).await,
        _ => Ok(own.clone()),
    }
}

/// The failed checks' logs of the workspace repository's PRs among `views`.
async fn read_failed_logs(ctx: &Context, own: &ForgeRepo, workspace: &str, views: &mut [ItemView]) {
    if !own.supports(Capability::ReadJobLogs) {
        return;
    }
    for view in views {
        if own.pull(&view.url).is_err() {
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
                        kind: Some(LinkKind::Pr),
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
    let kind = view.kind.title();
    match &view.details {
        Some(details) => {
            let heading = format!("{kind} #{}: {}", details.number, details.title);
            outln!("{}", palette.paint(Style::Heading, heading));
            outln!("URL:      {}", view.url);
            render_details(&view.url, details, palette, emoji);
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

fn render_details(url: &str, details: &Details, palette: Palette, emoji: bool) {
    match &details.pr {
        Some(pr) => render_pr(url, &details.state, pr, palette),
        None => outln!("State:    {}", details.state),
    }
    outln!("Author:   {}", details.author);
    outln!("Created:  {}", details.created_at);
    if !details.labels.is_empty() {
        outln!("Labels:   {}", details.labels.join(", "));
    }
    if !details.assignees.is_empty() {
        outln!("Assignees: {}", details.assignees.join(", "));
    }
    if let Some(milestone) = &details.milestone {
        outln!("Milestone: {milestone}");
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

fn render_pr(url: &str, state: &str, pr: &PrDetails, palette: Palette) {
    let conflicts = match pr.merge_conflicts {
        Some(true) => format!(", {}", palette.paint(Style::Error, "merge conflicts")),
        Some(false) => ", no merge conflicts".into(),
        None => String::new(),
    };
    let draft = if pr.draft { ", draft" } else { "" };
    let auto_merge = match pr.auto_merge {
        Some(AutoMerge {
            method: Some(method),
        }) => format!(", auto-merge ({method})"),
        Some(AutoMerge { method: None }) => ", auto-merge".into(),
        None => String::new(),
    };
    outln!("State:    {state}{draft}{conflicts}{auto_merge}");
    let behind = match pr.behind {
        Some(0) => ", up to date".into(),
        Some(1) => ", 1 commit behind".into(),
        Some(behind) => format!(", {behind} commits behind"),
        None => String::new(),
    };
    outln!("Branch:   {} → {}{behind}", pr.head, pr.base);
    if let Some(review) = pr.review {
        outln!("Review:   {}", palette.review_state(review));
    }
    if !pr.reviewers.is_empty() {
        outln!("Reviewers: {}", pr.reviewers.join(", "));
    }
    outln!("Checks:   {}", pr.checks.len());
    for check in &pr.checks {
        outln!("  {}: {}", check.name, palette.check_result(&check.result));
    }
    if !pr.waiting_runs.is_empty() {
        let line = ci::waiting_runs_line(url, pr.waiting_runs.len());
        outln!("{}", palette.paint(Style::Warning, &line));
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
