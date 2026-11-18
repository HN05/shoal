//! `shoal split`: a child workspace on a new branch, which the agent of the
//! parent space, a workspace or a chat, works on and receives the activity
//! of, or a workspace at a commit, with no branch of its own. Neither gets an
//! agent of its own; `shoal start` starts one.
use anyhow::Result;
use shoal_ids::repository_ids::RepositoryId;

use crate::client::{
    output::split::Child,
    preparation::{self, NewSplit, Preparing},
};
use crate::{
    cli::{
        args::create::CopyArgs,
        context::Context,
        output::{Palette, Style},
        shell, terminal,
        ui::{self, Fallback},
    },
    client::connection::CommandPaths,
    forge::items::reference::ItemInput,
    model::{SpaceRef, Workspace},
    protocol::{
        CommitStart, CreationOptions, ItemContext,
        operations::{Prepared, QueuedLaunch},
    },
    refusal::{Reason, Refusal},
    state::WorkspaceState,
};
use shoal_protocol::space_names::{SpaceName, SpaceSelector};

use super::issues::Issue;

/// What `shoal split` asks for besides the parent.
pub(super) struct Split {
    pub branch: Option<String>,
    pub issue: Option<ItemInput>,
    pub base: Option<String>,
    pub copy: CopyArgs,
    pub options: CreationOptions,
}

pub(super) async fn run(ctx: &Context, space: Option<SpaceSelector>, split: Split) -> Result<i32> {
    // A supplied branch fails before the parent picker opens.
    if let Some(branch) = &split.branch {
        shoal_core::git::check_branch_name(branch)?;
    }
    // A named copy fails before the parent picker too.
    let copy = super::copies::source(ctx, split.copy).await?;
    let parent = ui::select_space(ctx, space, Fallback::CurrentDirectory).await?;
    let issue = match &split.issue {
        Some(input) => Some(load_issue(ctx, &parent, input).await?),
        None => None,
    };
    let name = match (split.branch, &issue) {
        (Some(branch), _) => Some(branch),
        (None, Some(issue)) => Some(issue.branch_name()),
        (None, None) if copy.is_some() => None,
        (None, None) => anyhow::bail!("name the child's branch or pass --issue"),
    };
    // The child takes the issue's title as its name.
    let mut options = split.options;
    if let Some(issue) = &issue {
        options.name = Some(SpaceName::derive(&issue.title, "workspace"));
    }
    let start = NewSplit {
        copy: copy.map(|copy| copy.source),
        base: split.base,
        options,
        issue: issue.map(|issue| issue.url.into()),
        ..NewSplit::new(parent.clone(), name)
    };
    let preparing = start_child(ctx, start).await?;
    let Some(child) = set_up(ctx, preparing).await? else {
        return Ok(1);
    };
    report(ctx, &child)?;
    Ok(0)
}

/// Create a workspace without a branch at `base`, or at the default branch,
/// or a copy of the workspace `copy` names: a child of the named or current
/// space, or outside one a top-level workspace in the current repository, or
/// the copied one's.
pub(super) async fn at_commit(
    ctx: &Context,
    space: Option<SpaceSelector>,
    base: Option<String>,
    copy: CopyArgs,
    options: CreationOptions,
) -> Result<i32> {
    let copy = super::copies::source(ctx, copy).await?;
    let copied = copy.as_ref().map(|copy| copy.repository.clone());
    let (repository, parent) = commit_owner(ctx, space, copied).await?;
    let start = match copy {
        Some(copy) => CommitStart::Copy(copy.source),
        None => CommitStart::Revision(base),
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

/// The repository and parent of a workspace at a commit: the named or
/// current space and its repository, or the `copied` repository or else the
/// current one for a parent without one, or outside every space none.
async fn commit_owner(
    ctx: &Context,
    space: Option<SpaceSelector>,
    copied: Option<RepositoryId>,
) -> Result<(crate::repository::Selector, Option<SpaceSelector>)> {
    let parent = match ui::select_space(ctx, space, Fallback::CurrentDirectoryOnly).await {
        Ok(parent) => Some(parent),
        Err(error) if error.is::<ui::NoCurrentWorkspace>() => None,
        Err(error) => return Err(error),
    };
    let own = match &parent {
        Some(parent) => space_repository(ctx, parent).await?,
        None => None,
    };
    let repository = match own.or(copied) {
        Some(repository) => repository,
        None => current_repository(ctx).await?,
    };
    Ok((repository.into(), parent))
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
    let hint = "pass --space or run it in a registered checkout";
    super::issues::current_repository(ctx, repositories, hint).await
}

/// Split the child `split` describes and follow its preparation.
pub(super) async fn start_child(
    ctx: &Context,
    split: NewSplit,
) -> Result<Preparing<'_, CommandPaths>> {
    let env = crate::env::without_secrets();
    preparation::create(&ctx.paths, split, env).await
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

/// The issue `input` names in the parent workspace's repository.
async fn load_issue(ctx: &Context, parent: &SpaceSelector, input: &ItemInput) -> Result<Issue> {
    let context = ItemContext::space(parent.clone());
    super::issues::load(ctx, context, input.clone()).await
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
