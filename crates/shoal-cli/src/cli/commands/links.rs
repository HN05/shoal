//! Link relations: selecting a space's issues, PRs and CI runs, linking,
//! unlinking and watching them, and marking them ready.
use anyhow::{Result, bail, ensure};

use super::spaces;
use crate::client::assignments;
use crate::client::output::items::Registered;
use crate::{
    cli::{
        args::items::{ItemArgs, RelationArgs},
        client,
        context::Context,
        ui::{self, Acting},
    },
    forge::items::{
        link::{ItemKind, LinkKind, Selection},
        list::Search,
        reference::ItemInput,
    },
    model::{ReadyMarks, Relation, ReviewMark},
    protocol::{ItemContext, RepoTarget},
};
use shoal_ids::repository_ids::RepositoryId;
use shoal_protocol::space_names::SpaceSelector;

/// The space and the items selected in it, with the kind of a bare number
/// resolved.
pub(super) async fn select(
    ctx: &Context,
    items: ItemArgs,
    acting: &Acting,
) -> Result<(SpaceSelector, Selection)> {
    let selection = items.selection()?;
    let space = ui::select_space(ctx, items.space, ui::Fallback::CurrentDirectory(acting));
    let space = space.await?;
    let target = ItemContext::space(space.clone()).into();
    let selection = resolve_kind(ctx, target, selection).await?;
    Ok((space, selection))
}

/// The items selected in the named or current space, or in the repository
/// `repo` names, of the checkout the current directory or a URL is in, or
/// of a URL, where only a number or URL selects one. A bare number keeps no
/// kind, which the daemon gives it as it reads.
pub(super) async fn in_context(
    ctx: &Context,
    items: ItemArgs,
    repo: Option<String>,
) -> Result<(RepoTarget, Selection)> {
    let selection = items.selection()?;
    let input = selection.input.as_ref();
    let target = super::items::select_item_target(ctx, items.space, repo, input).await?;
    Ok((target, selection))
}

/// [`in_context`], with the kind of a bare number selected in a space
/// resolved.
pub(super) async fn select_in_context(
    ctx: &Context,
    items: ItemArgs,
    repo: Option<String>,
) -> Result<(RepoTarget, Selection)> {
    let (target, selection) = in_context(ctx, items, repo).await?;
    let selection = match in_own_space(&target) {
        true => resolve_kind(ctx, target.clone(), selection).await?,
        false => selection,
    };
    Ok((target, selection))
}

/// Whether `target` is the own repository of the space it comes from, whose
/// linked items a number may name.
pub(super) fn in_own_space(target: &RepoTarget) -> bool {
    let own = matches!(
        &target.context,
        Some(ItemContext::Space {
            repository: None,
            ..
        })
    );
    own && target.repository.is_none()
}

/// Give a number selected without a kind the kind of the item with that
/// number in the `target` repository: in a space's own, that of the linked
/// one, otherwise the kind the forge reports for it.
async fn resolve_kind(
    ctx: &Context,
    target: RepoTarget,
    selection: Selection,
) -> Result<Selection> {
    let (None, Some(ItemInput::Number(number))) = (selection.kind, &selection.input) else {
        return Ok(selection);
    };
    let kind = client::number_kind(&ctx.paths, target, *number).await?;
    Ok(Selection {
        kind: Some(kind),
        ..selection
    })
}

pub(super) async fn link(ctx: &Context, items: RelationArgs) -> Result<i32> {
    let (items, relation) = items.split()?;
    let selected = items.selection()?;
    ensure!(
        selected.input.is_some() || ctx.interactive(),
        "link needs an issue or PR number, or an issue, PR or CI run URL"
    );
    // The item, which the space's context classifies once it is picked,
    // decides nothing of which spaces the link reaches.
    let linking = Acting::request(move |space| {
        let item = ItemInput::Number(1);
        crate::client::items::link(space, relation, LinkKind::Issue, item, None)
    });
    let space = ui::select_space(ctx, items.space, ui::Fallback::CurrentDirectory(&linking));
    let space = space.await?;
    let context = ItemContext::space(space.clone());
    let selected = resolve_kind(ctx, context.clone().into(), selected).await?;
    let (kind, input) = match selected {
        Selection {
            kind: Some(kind),
            input: Some(input),
        } => (kind, input),
        Selection { kind, .. } => pick_open_item(ctx, context.clone(), kind).await?,
    };
    let linked = (crate::client::items::link(space.clone(), relation, kind, input, None))
        .send(&ctx.paths)
        .await?;
    if let Some(error) = linked.title_error {
        eprintln!("Title unavailable: {error}");
    }
    print_warnings(&linked.warnings);
    ctx.emit(
        &link_outcome(kind, relation),
        Registered {
            registered: true,
            relation: Some(relation),
            warnings: linked.warnings,
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

/// Choose an open issue or PR of the space's repository, asking for the
/// kind when it is not given; runs have no list to choose from.
async fn pick_open_item(
    ctx: &Context,
    context: ItemContext,
    kind: Option<LinkKind>,
) -> Result<(LinkKind, ItemInput)> {
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
    let items = client::list_items(&ctx.paths, context.into(), kind, Search::picker()).await?;
    let number = match kind {
        ItemKind::Issue => ui::pick_item(ctx, "Issue> ", "issues", items)?,
        ItemKind::Pr => ui::pick_item(ctx, "Pull request> ", "pull requests", items)?,
    };
    Ok((kind.into(), ItemInput::Number(number)))
}

pub(super) async fn unlink(ctx: &Context, items: RelationArgs) -> Result<i32> {
    let (items, relation) = items.split()?;
    let unlinking =
        Acting::request(move |space| crate::client::items::unlink(space, relation, None, None));
    let (space, selected) = select(ctx, items, &unlinking).await?;
    let unlinked =
        crate::client::items::unlink(space.clone(), relation, selected.kind, selected.input);
    let unlinked = unlinked.send(&ctx.paths).await?;
    print_warnings(&unlinked.warnings);
    let unlinked = Registered {
        registered: false,
        relation: None,
        warnings: unlinked.warnings,
    };
    ctx.emit("Items unlinked", unlinked)?;
    Ok(0)
}

/// Print what changing an issue's assignees on the forge warned of.
pub(super) fn print_warnings(warnings: &[String]) {
    for warning in warnings {
        eprintln!("warning: {warning}");
    }
}

/// Wait for the activity of the items selected in a space, an item of the
/// registered repository `repo` names for a chat without a repository.
pub(super) async fn watch(
    ctx: &Context,
    items: ItemArgs,
    repo: Option<String>,
    timeout: Option<u64>,
    resumed: bool,
) -> Result<i32> {
    let selection = items.selection()?;
    let space = ui::select_space(
        ctx,
        items.space,
        ui::Fallback::CurrentDirectory(&crate::cli::ui::Acting::Unconverted),
    )
    .await?;
    let (context, repository) = watch_context(ctx, &space, repo.as_deref()).await?;
    let selection = resolve_kind(ctx, context.into(), selection).await?;
    spaces::watch_items(ctx, space, selection, repository, timeout, resumed).await
}

/// Where a watch in `space` reads its items, with the registered repository
/// `repo` names: the space, or a chat acting in that repository.
async fn watch_context(
    ctx: &Context,
    space: &SpaceSelector,
    repo: Option<&str>,
) -> Result<(ItemContext, Option<RepositoryId>)> {
    let repository = match repo {
        Some(repo) => Some(super::items::named_repository(ctx, repo).await?.id),
        None => None,
    };
    let context = ItemContext::Space {
        space: space.clone(),
        repository: repository.clone(),
    };
    Ok((context, repository))
}

pub(super) async fn ready(ctx: &Context, items: ItemArgs) -> Result<i32> {
    let marking = Acting::request(|space| assignments::mark_ready(space, Selection::default()));
    let (space, selection) = select(ctx, items, &marking).await?;
    let itself = space_kind(ctx, &space).await?;
    let outcomes = (assignments::mark_ready(space.clone(), selection))
        .send(&ctx.paths)
        .await?;
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
    let clearing =
        Acting::request(move |space| assignments::clear_ready(space, Selection::default(), draft));
    let (space, selection) = select(ctx, items, &clearing).await?;
    let itself = space_kind(ctx, &space).await?;
    let marks = (assignments::clear_ready(space.clone(), selection, draft))
        .send(&ctx.paths)
        .await?;
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
        (crate::client::spaces::present_space(&ctx.paths, space).await?)
            .map_or("workspace", |space| space.kind().as_str()),
    )
}

/// One indented line per mark: its item, or the space itself, called
/// `itself`.
pub(super) fn review_lines(marks: &[ReviewMark], itself: &str) -> String {
    marks
        .iter()
        .map(|mark| match (mark.kind, &mark.url) {
            (Some(kind), Some(url)) => format!("  {kind} {url}"),
            _ => format!("  {itself}"),
        })
        .collect::<Vec<_>>()
        .join("\n")
}
