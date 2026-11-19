//! `shoal create`: a workspace without an agent, made as `shoal start`
//! makes one, below the current space or the one `--parent` names, whose
//! agent works on it and receives its activity, or outside a space at the
//! top; or a workspace at a commit, with no branch of its own. `shoal
//! start` starts an agent of its own there.
use anyhow::Result;
use shoal_ids::repository_ids::RepositoryId;

use crate::client::{
    output::create::Child,
    preparation::{self, NewChild, NewCreation, Preparing},
};
use crate::{
    cli::{
        args::create::CreateArgs,
        context::Context,
        output::{Palette, Style},
        shell, terminal,
        ui::{self, Fallback},
    },
    client::connection::CommandPaths,
    model::{SpaceRef, Workspace},
    protocol::{
        CommitStart, CreationOptions,
        operations::{Prepared, QueuedLaunch},
    },
    refusal::{Reason, Refusal},
    state::WorkspaceState,
};
use shoal_protocol::space_names::SpaceSelector;

/// Create the workspace `args` describe, below the space they or the
/// current directory name, or at the top outside one, follow its
/// preparation and report it.
pub(super) async fn run(ctx: &Context, args: CreateArgs) -> Result<i32> {
    let parent = parent_space(ctx, args.parent.clone()).await?;
    if args.no_branch {
        return at_commit(ctx, parent, &args).await;
    }
    let mut launch = args.launch(args.parent.clone());
    launch.directory = ui::current_directory()?;
    launch.path = (launch.path.take())
        .map(|path| super::repositories::absolute(ctx, path))
        .transpose()?;
    super::workspaces::pick_start_source(ctx, &mut launch).await?;
    // Only a parent named with --parent keeps the start out of the
    // existing workspace of the branch it names.
    let below = parent.filter(|_| args.parent.is_none());
    let creation = NewCreation { launch, below };
    let env = crate::env::without_secrets();
    let preparing = preparation::create(&ctx.paths, creation, env).await?;
    let Some(created) = set_up(ctx, preparing).await? else {
        return Ok(1);
    };
    report(ctx, &created)?;
    Ok(0)
}

/// The space a new workspace goes below: the one `named`, else the
/// current one, else none.
async fn parent_space(
    ctx: &Context,
    named: Option<SpaceSelector>,
) -> Result<Option<SpaceSelector>> {
    match ui::select_space(ctx, named, Fallback::CurrentDirectoryOnly).await {
        Ok(parent) => Ok(Some(parent)),
        Err(error) if error.is::<ui::NoCurrentWorkspace>() => Ok(None),
        Err(error) => Err(error),
    }
}

/// Create the workspace without a branch `args` describe, at their base,
/// or at the default branch, or a copy of the workspace they copy: a child
/// of `parent`, or without one a top-level workspace in the repository
/// they name, else the copied one's or the current one.
async fn at_commit(ctx: &Context, parent: Option<SpaceSelector>, args: &CreateArgs) -> Result<i32> {
    let copy = super::copies::source(ctx, args.copy.clone()).await?;
    let copied = copy.as_ref().map(|copy| copy.repository.clone());
    let named = (args.repository_override.clone())
        .map(crate::repository::named)
        .transpose()?;
    let repository = match named {
        Some(named) => named,
        None => commit_repository(ctx, parent.as_ref(), copied).await?,
    };
    let start = match copy {
        Some(copy) => CommitStart::Copy(copy.source),
        None => CommitStart::Revision(args.base.clone()),
    };
    let options = CreationOptions {
        name: args.name.clone(),
        priority: args.priority,
        ..args.creation.clone().into()
    };
    let created =
        crate::client::workspaces::create_at_commit(&ctx.paths, repository, parent, start, options)
            .await?;
    let Some(child) = prepare(ctx, created).await? else {
        return Ok(1);
    };
    report(ctx, &child)?;
    Ok(0)
}

/// The repository of a workspace at a commit: that of its `parent`, or the
/// `copied` repository or else the current one for a parent without one or
/// none.
async fn commit_repository(
    ctx: &Context,
    parent: Option<&SpaceSelector>,
    copied: Option<RepositoryId>,
) -> Result<crate::repository::Selector> {
    let own = match parent {
        Some(parent) => space_repository(ctx, parent).await?,
        None => None,
    };
    let repository = match own.or(copied) {
        Some(repository) => repository,
        None => current_repository(ctx).await?,
    };
    Ok(repository.into())
}

/// The repository of the space `selector` names; none for a chat without
/// one.
async fn space_repository(ctx: &Context, selector: &SpaceSelector) -> Result<Option<RepositoryId>> {
    let spaces = crate::client::spaces::list(&ctx.paths).await?;
    let space = spaces
        .find(selector)
        .ok_or_else(|| Refusal::new(Reason::NotFound, format!("unknown space: {selector}")))?;
    Ok(space.repository_id)
}

/// The registered repository of the current directory.
async fn current_repository(ctx: &Context) -> Result<RepositoryId> {
    let repositories = crate::client::repositories::list(&ctx.paths).await?;
    let hint = "pass --parent or run it in a registered checkout";
    super::issues::current_repository(ctx, repositories, hint).await
}

/// Create the child `child` describes and follow its preparation.
pub(super) async fn start_child(
    ctx: &Context,
    child: NewChild,
) -> Result<Preparing<'_, CommandPaths>> {
    let env = crate::env::without_secrets();
    preparation::create(&ctx.paths, child, env).await
}

/// Run the child's setup and post-setup hook; `None` when setup failed.
pub(super) async fn prepare(ctx: &Context, child: Workspace) -> Result<Option<Workspace>> {
    if child.state != WorkspaceState::Preparing {
        super::workspaces::run_post_setup(ctx, &child).await?;
        return Ok(Some(child));
    }
    match super::workspaces::setup_workspace(ctx, &child, true).await? {
        Some(set_up) => set_up.finish(ctx).await.map(Some),
        None => Ok(None),
    }
}

/// The workspace a request opened once the preparation the daemon started
/// for it, followed to its end, set it up; `None` when setup failed.
pub(super) async fn set_up(
    ctx: &Context,
    preparing: Preparing<'_, CommandPaths>,
) -> Result<Option<Workspace>> {
    let prepared = follow_queuing(ctx, preparing, None).await?;
    Ok(prepared.map(|prepared| *prepared.workspace))
}

/// Follow the preparation as [`set_up`] does, with the agent it queued;
/// `agent`, the one the request asked for, is queued after a setup failure
/// the user ignored, which ends preparation before it.
async fn follow_queuing(
    ctx: &Context,
    preparing: Preparing<'_, CommandPaths>,
    agent: Option<Box<QueuedLaunch>>,
) -> Result<Option<Prepared>> {
    match super::workspaces::follow_setup(ctx, preparing).await? {
        Some(set_up) => set_up.finish_queuing(ctx, agent).await.map(Some),
        None => Ok(None),
    }
}

/// Report the child, then enter it when this terminal follows new
/// workspaces.
fn report(ctx: &Context, child: &Workspace) -> Result<()> {
    let palette = Palette::stdout(ctx.json);
    let text = format!(
        "Created {} {}",
        palette.paint(Style::Heading, &child.name),
        super::workspaces::location_phrase(child),
    );
    ctx.emit(
        &text,
        Child {
            workspace: child.clone(),
        },
    )?;
    if terminal::follows_caller(ctx) && !crate::env::is_scoped() {
        shell::navigate(ctx, &child.path)?;
    }
    Ok(())
}

/// The parent and child spaces of a space, when it has any.
pub(super) fn render(parent: &Option<SpaceRef>, children: &[SpaceRef]) {
    if let Some(parent) = parent {
        outln!("Parent space:  {}", super::base::describe_space(parent));
    }
    if !children.is_empty() {
        let children: Vec<_> = (children.iter()).map(super::base::describe_space).collect();
        outln!("Child spaces:  {}", children.join(", "));
    }
}
