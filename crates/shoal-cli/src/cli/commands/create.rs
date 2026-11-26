//! `shoal create`: a workspace without an agent.
use anyhow::Result;
use shoal_ids::repository_ids::RepositoryId;
use shoal_workers::hosting;

use crate::client::{
    output::create::Child,
    preparation::{self, NewChild, NewCreation, Preparing},
};
use crate::{
    cli::{
        args::create::CreateArgs,
        context::Context,
        output::{Palette, Style},
        shell,
        ui::{self, Fallback},
    },
    client::connection::CommandPaths,
    model::{SpaceRef, Workspace},
    protocol::{
        CommitStart, CreationOptions,
        operations::{Prepared, QueuedLaunch, StartedWorkspace},
    },
    state::WorkspaceState,
};
use shoal_protocol::space_names::SpaceSelector;

pub(super) async fn run(ctx: &Context, args: CreateArgs) -> Result<i32> {
    super::start::ensure_scope_token(ctx)?;
    let parent = parent_space(ctx, args.parent.clone()).await?;
    if args.no_branch {
        return at_commit(ctx, parent, &args).await;
    }
    let creation = picked_creation(ctx, &args, parent).await?;
    let launch = creation.launch.clone();
    let env = crate::env::without_secrets();
    let started = preparation::start(&ctx.paths, creation, env).await?;
    // From the user's terminal it enters the workspace as a start without
    // an agent does; an agent's creation follows it and reports it.
    if let Some(code) = super::preparation::enter_created(ctx, &launch, &started).await? {
        return Ok(code);
    }
    let preparing = Preparing::follow(&ctx.paths, started).await?;
    let Some(created) = set_up(ctx, preparing).await? else {
        return Ok(1);
    };
    report(ctx, &created)?;
    Ok(0)
}

/// Made in this process's directory, with a relative path resolved there. Only
/// a parent named with `--parent` keeps the creation out of the existing
/// workspace of the branch it names.
async fn picked_creation(
    ctx: &Context,
    args: &CreateArgs,
    parent: Option<SpaceSelector>,
) -> Result<NewCreation> {
    let mut launch = args.launch(args.parent.clone());
    // What the arguments refuse fails before a picker opens.
    super::start::check_start_arguments(&launch)?;
    check_registered(ctx, launch_repository(&launch)?).await?;
    if let Some(copied) = super::copies::source(ctx, args.copy.clone()).await? {
        copied.copy_into(&mut launch)?;
    }
    launch.directory = ui::current_directory()?;
    launch.path = (launch.path.take())
        .map(|path| super::repositories::absolute(ctx, path))
        .transpose()?;
    super::start::pick_start_source(ctx, &mut launch).await?;
    let below = parent.filter(|_| args.parent.is_none());
    Ok(NewCreation { launch, below })
}

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

async fn at_commit(ctx: &Context, parent: Option<SpaceSelector>, args: &CreateArgs) -> Result<i32> {
    let named = commit_named_repository(args)?;
    check_registered(ctx, named.clone()).await?;
    let copy = super::copies::source(ctx, args.copy.clone()).await?;
    let copied = copy.as_ref().map(|copy| copy.repository.clone());
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
    // From the user's terminal it enters the workspace as a creation on a
    // branch does, once its setup is done.
    let started = StartedWorkspace {
        operation: None,
        workspace: Box::new(child.clone()),
        reused: false,
        entry: None,
        warnings: Vec::new(),
    };
    let launch = shoal_protocol::launches::Launch::default();
    if let Some(code) = super::preparation::enter_created(ctx, &launch, &started).await? {
        return Ok(code);
    }
    report(ctx, &child)?;
    Ok(0)
}

fn launch_repository(
    launch: &shoal_protocol::launches::Launch,
) -> Result<Option<crate::repository::Selector>> {
    use shoal_protocol::launches::StartTarget;
    let named = match crate::launches::decided_target(launch)? {
        Some(StartTarget::Repository(named)) => Some(named),
        _ => launch.repository.clone(),
    };
    named.map(crate::repository::named).transpose()
}

/// Refuse a repository a creation names that is not registered, before a
/// copy's picker opens, offering an unregistered checkout for registration
/// first as a start does from the user's own terminal.
async fn check_registered(
    ctx: &Context,
    repository: Option<crate::repository::Selector>,
) -> Result<()> {
    let Some(mut repository) = repository else {
        return Ok(());
    };
    let offer = !crate::env::is_scoped() && crate::env::own_chat().is_none();
    if offer
        && let Some(registered) = super::repositories::offer_unregistered(ctx, &repository).await?
    {
        repository = registered.id.into();
    }
    let repos = crate::client::repositories::list(&ctx.paths).await?;
    crate::repository::select(&repos, repository).await?;
    Ok(())
}

fn commit_named_repository(args: &CreateArgs) -> Result<Option<crate::repository::Selector>> {
    use shoal_protocol::launches::StartTarget;
    let positional = match &args.repository {
        None => None,
        Some(StartTarget::Repository(named) | StartTarget::Number(named)) => Some(named.clone()),
        Some(_) => anyhow::bail!(
            "--no-branch starts at a commit of a repository, not an issue, PR or branch link"
        ),
    };
    anyhow::ensure!(
        positional.is_none() || args.repository_override.is_none(),
        "a positional repository cannot be combined with --repo"
    );
    (positional.or(args.repository_override.clone()))
        .map(crate::repository::named)
        .transpose()
}

async fn commit_repository(
    ctx: &Context,
    parent: Option<&SpaceSelector>,
    copied: Option<RepositoryId>,
) -> Result<crate::repository::Selector> {
    let own = match parent {
        Some(parent) => crate::cli::commands::start::space_repository(ctx, parent)
            .await?
            .here()?,
        None => None,
    };
    let repository = match own.or(copied) {
        Some(repository) => repository,
        None => current_repository(ctx).await?,
    };
    Ok(repository.into())
}

async fn current_repository(ctx: &Context) -> Result<RepositoryId> {
    let repositories = crate::client::repositories::list(&ctx.paths).await?;
    let hint = "pass --parent or run it in a registered checkout";
    super::issues::current_repository(ctx, repositories, hint).await
}

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
        shoal_workers::preparation::run_post_setup(&ctx.paths, ctx, &child).await?;
        return Ok(Some(child));
    }
    match shoal_workers::preparation::setup_workspace(&ctx.paths, ctx, &child, true).await? {
        Some(set_up) => set_up.finish(&ctx.paths, ctx).await.map(Some),
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
    match shoal_workers::preparation::follow_setup(&ctx.paths, ctx, preparing).await? {
        Some(set_up) => set_up
            .finish_queuing(&ctx.paths, ctx, agent)
            .await
            .map(Some),
        None => Ok(None),
    }
}

fn report(ctx: &Context, child: &Workspace) -> Result<()> {
    let palette = Palette::stdout(ctx.json);
    let text = format!(
        "Created {} {}",
        palette.paint(Style::Heading, &child.name),
        super::preparation::location_phrase(child),
    );
    ctx.emit(
        &text,
        Child {
            workspace: child.clone(),
        },
    )?;
    if hosting::follows_caller(ctx.caller()) && !crate::env::is_scoped() {
        shell::navigate(ctx, &child.path)?;
    }
    Ok(())
}

pub(super) fn render(parent: &Option<SpaceRef>, children: &[SpaceRef]) {
    if let Some(parent) = parent {
        outln!("Parent space:  {}", super::base::describe_space(parent));
    }
    if !children.is_empty() {
        let children: Vec<_> = (children.iter()).map(super::base::describe_space).collect();
        outln!("Child spaces:  {}", children.join(", "));
    }
}
