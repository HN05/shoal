//! Link relations: selecting a space's issues, PRs and CI runs, linking,
//! unlinking and watching them, and marking them ready.
use anyhow::{Result, bail, ensure};

use super::workspaces;
use crate::client::assignments;
use crate::client::output::workspaces::Registered;
use crate::client::space_names::SpaceSelector;
use crate::{
    cli::{
        args::items::{ItemArgs, RelationArgs},
        client,
        context::Context,
        ui,
    },
    forge::items::{
        link::{ItemKind, LinkKind, Selection},
        list::Search,
    },
    model::{ReadyMarks, Relation, ReviewMark},
    protocol::ItemContext,
};

/// The space and the items selected in it, with the kind of a bare number
/// resolved.
pub(super) async fn select(ctx: &Context, items: ItemArgs) -> Result<(SpaceSelector, Selection)> {
    let selection = Selection::parse(items.kind_or_url, items.item)?;
    let space = ui::select_space(ctx, items.space, ui::Fallback::CurrentDirectory).await?;
    let context = super::items::space_context(ctx, space.clone()).await?;
    let selection = resolve_kind(ctx, context, selection).await?;
    Ok((space, selection))
}

/// The items selected in the named or current space, or in the checkout of
/// the registered repository `repo` names or the current directory or a URL
/// is in, where only a number or URL selects one. A bare number keeps no
/// kind, which the daemon gives it as it reads.
pub(super) async fn in_context(
    ctx: &Context,
    items: ItemArgs,
    repo: Option<String>,
) -> Result<(ItemContext, Selection)> {
    let selection = Selection::parse(items.kind_or_url, items.item)?;
    let input = selection.input.as_deref();
    let context = super::items::select_space_item_context(ctx, items.space, repo, input).await?;
    Ok((context, selection))
}

/// [`in_context`], with the kind of a bare number selected in a space
/// resolved.
pub(super) async fn select_in_context(
    ctx: &Context,
    items: ItemArgs,
    repo: Option<String>,
) -> Result<(ItemContext, Selection)> {
    let (context, selection) = in_context(ctx, items, repo).await?;
    let selection = match &context {
        ItemContext::Workspace(_) | ItemContext::Chat(_) => {
            resolve_kind(ctx, context.clone(), selection).await?
        }
        ItemContext::Checkout(_) | ItemContext::ChatIn { .. } => selection,
    };
    Ok((context, selection))
}

/// Give a number selected without a kind the kind of the item with that
/// number in `context`: in a space, that of the linked one, otherwise the
/// kind the forge reports for it.
async fn resolve_kind(
    ctx: &Context,
    context: ItemContext,
    selection: Selection,
) -> Result<Selection> {
    let (None, Some(input)) = (selection.kind, &selection.input) else {
        return Ok(selection);
    };
    let number = input.parse::<u64>()?;
    let kind = client::number_kind(&ctx.paths, context, number).await?;
    Ok(Selection {
        kind: Some(kind),
        ..selection
    })
}

pub(super) async fn link(ctx: &Context, items: RelationArgs) -> Result<i32> {
    let (items, relation) = items.split()?;
    let selected = Selection::parse(items.kind_or_url, items.item)?;
    ensure!(
        selected.input.is_some() || ctx.interactive(),
        "link needs an issue or PR number, or an issue, PR or CI run URL"
    );
    let space = ui::select_space(ctx, items.space, ui::Fallback::CurrentDirectory).await?;
    let context = super::items::space_context(ctx, space.clone()).await?;
    let selected = resolve_kind(ctx, context.clone(), selected).await?;
    let (kind, input) = match selected {
        Selection {
            kind: Some(kind),
            input: Some(input),
        } => (kind, input),
        Selection { kind, .. } => pick_open_item(ctx, context.clone(), kind).await?,
    };
    let title = match kind {
        LinkKind::Issue => issue_title(ctx, context, &input).await,
        LinkKind::Pr | LinkKind::Ci => None,
    };
    let item = crate::model::LinkedItem {
        kind,
        url: input,
        title,
    };
    crate::client::items::link(&ctx.paths, &space, relation, &item).await?;
    ctx.emit(
        &link_outcome(kind, relation),
        Registered {
            registered: true,
            relation: Some(relation),
        },
    )?;
    Ok(0)
}

fn link_outcome(kind: LinkKind, relation: Relation) -> String {
    match (relation, kind) {
        (_, LinkKind::Ci) => {
            format!("CI run linked as {relation}; shoal watch reports when it finishes.")
        }
        (Relation::Assigned, LinkKind::Pr) => {
            "PR linked as assigned; run shoal watch pr for updates and shoal done once no work is left.".into()
        }
        (Relation::Assigned, LinkKind::Issue) => {
            "Issue linked as assigned; closure can complete the assignment.".into()
        }
        (Relation::Opened, LinkKind::Pr) => "PR linked as opened.".into(),
        (Relation::Opened, LinkKind::Issue) => "Issue linked as opened.".into(),
        (Relation::Related, LinkKind::Pr) => "PR linked as related.".into(),
        (Relation::Related, LinkKind::Issue) => "Issue linked as related.".into(),
    }
}

/// The title lists show for the space; linking proceeds without one when
/// the forge cannot be read.
async fn issue_title(ctx: &Context, context: ItemContext, input: &str) -> Option<String> {
    match super::issues::load(ctx, context, input).await {
        Ok(issue) => Some(issue.title),
        Err(error) => {
            eprintln!("Issue title unavailable: {error:#}");
            None
        }
    }
}

/// Choose an open issue or PR of the space's repository, asking for the
/// kind when it is not given; runs have no list to choose from.
async fn pick_open_item(
    ctx: &Context,
    context: ItemContext,
    kind: Option<LinkKind>,
) -> Result<(LinkKind, String)> {
    let kind = match kind {
        Some(LinkKind::Ci) => bail!("link ci needs a run's number or URL"),
        kind => kind.and_then(LinkKind::item),
    };
    let kind = match kind {
        Some(kind) => kind,
        None => ui::pick_choice(
            ctx,
            "Link> ",
            &[(ItemKind::Issue, "Issue"), (ItemKind::Pr, "Pull request")],
        )?,
    };
    let items = client::list_items(&ctx.paths, context, kind, Search::picker(), None).await?;
    let number = match kind {
        ItemKind::Issue => ui::pick_item(ctx, "Issue> ", "issues", items)?,
        ItemKind::Pr => ui::pick_item(ctx, "Pull request> ", "pull requests", items)?,
    };
    Ok((kind.into(), number))
}

pub(super) async fn unlink(ctx: &Context, items: RelationArgs) -> Result<i32> {
    let (items, relation) = items.split()?;
    let (space, selected) = select(ctx, items).await?;
    crate::client::items::unlink(
        &ctx.paths,
        space.clone(),
        relation,
        selected.kind,
        selected.input,
    )
    .await?;
    let unlinked = Registered {
        registered: false,
        relation: None,
    };
    ctx.emit("Items unlinked", unlinked)?;
    Ok(0)
}

pub(super) async fn watch(
    ctx: &Context,
    items: ItemArgs,
    timeout: Option<u64>,
    resumed: bool,
) -> Result<i32> {
    let (space, selected) = select(ctx, items).await?;
    workspaces::watch_items(ctx, space, selected, timeout, resumed).await
}

pub(super) async fn ready(ctx: &Context, items: ItemArgs) -> Result<i32> {
    let (space, selection) = select(ctx, items).await?;
    let itself = space_kind(ctx, &space).await?;
    let outcomes = assignments::mark_ready(&ctx.paths, space.clone(), selection).await?;
    let ReadyMarks { marks, skipped } = outcomes.into();
    for skipped in &skipped {
        eprintln!("Skipped PR {}: {}", skipped.url, skipped.reason);
    }
    for warning in marks.iter().flat_map(|mark| &mark.warnings) {
        eprintln!("{warning}");
    }
    ctx.emit(
        &format!("Ready for review:\n{}", review_lines(&marks, itself)),
        &marks,
    )?;
    Ok(0)
}

/// `draft` overrides `[pr] draft_on_unready` when given.
pub(super) async fn unready(ctx: &Context, items: ItemArgs, draft: Option<bool>) -> Result<i32> {
    let (space, selection) = select(ctx, items).await?;
    let itself = space_kind(ctx, &space).await?;
    let marks = assignments::clear_ready(&ctx.paths, space.clone(), selection, draft).await?;
    let message = format!(
        "Ready-for-review marks cleared:\n{}",
        review_lines(&marks, itself)
    );
    ctx.emit(&message, &marks)?;
    Ok(0)
}

/// What a mark on the space `space` itself is called: `workspace` or
/// `chat`.
async fn space_kind(ctx: &Context, space: &SpaceSelector) -> Result<&'static str> {
    Ok(
        (crate::client::workspaces::present_space(&ctx.paths, space).await?)
            .map_or("workspace", |space| space.kind()),
    )
}

/// One indented line per mark: its item, or the space itself, called
/// `itself`, and whether new commits arrived since.
pub(super) fn review_lines(marks: &[ReviewMark], itself: &str) -> String {
    marks
        .iter()
        .map(|mark| {
            let item = match (mark.kind, &mark.url) {
                (Some(kind), Some(url)) => format!("{kind} {url}"),
                _ => itself.to_owned(),
            };
            let outdated = if mark.stale == Some(true) {
                " (outdated)"
            } else {
                ""
            };
            format!("  {item}{outdated}")
        })
        .collect::<Vec<_>>()
        .join("\n")
}
