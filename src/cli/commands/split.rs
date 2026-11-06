//! `shoal split`: a child workspace on a new branch, which the parent
//! workspace's agent works on and receives the activity of, or a workspace
//! at a commit, with no branch of its own. Neither gets an agent of its own;
//! `shoal start` starts one.
use anyhow::Result;

use crate::client::{operations::NewSplit, output::split::Child};
use crate::{
    cli::{
        args::create::CopyArgs,
        context::Context,
        output::{Palette, Style},
        shell, terminal,
        ui::{self, Fallback},
    },
    model::{SpaceRef, Workspace},
    protocol::{
        CommitStart, CreationOptions, ItemContext,
        operations::{Prepared, QueuedLaunch, StartedWorkspace},
    },
    refusal::{Reason, Refusal},
    state::WorkspaceState,
};

use super::issues::Issue;

/// What `shoal split` asks for besides the parent.
pub(super) struct Split {
    pub branch: Option<String>,
    pub issue: Option<String>,
    pub base: Option<String>,
    pub copy: CopyArgs,
    pub options: CreationOptions,
}

pub(super) async fn run(ctx: &Context, workspace: Option<String>, split: Split) -> Result<i32> {
    // A supplied branch fails before the parent picker opens.
    if let Some(branch) = &split.branch {
        crate::git::check_branch_name(None, branch).await?;
    }
    // A named copy fails before the parent picker too.
    let copy = super::copies::source(ctx, split.copy).await?;
    let parent = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
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
    let start = NewSplit {
        copy: copy.map(|copy| copy.source),
        base: split.base,
        options: split.options,
        issue: issue.as_ref().map(|issue| issue.url.clone()),
        ..NewSplit::new(parent, name)
    };
    let child = start_child(ctx, start).await?;
    let Some(child) = follow_preparation(ctx, child).await? else {
        return Ok(1);
    };
    report(ctx, &child)?;
    Ok(0)
}

/// Create a workspace without a branch at `base`, or at the default branch,
/// or a copy of the workspace `copy` names: a child of the named or current
/// workspace, or outside one a top-level workspace in the current
/// repository, or the copied one's.
pub(super) async fn at_commit(
    ctx: &Context,
    workspace: Option<String>,
    base: Option<String>,
    copy: CopyArgs,
    options: CreationOptions,
) -> Result<i32> {
    let copy = super::copies::source(ctx, copy).await?;
    let copied = copy.as_ref().map(|copy| copy.repository.clone());
    let (repository, parent) = commit_owner(ctx, workspace, copied).await?;
    let parent = parent.or_else(|| ui::current_chat(&None));
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
/// current workspace's, or without one the `copied` repository or else the
/// current one, and none.
async fn commit_owner(
    ctx: &Context,
    workspace: Option<String>,
    copied: Option<String>,
) -> Result<(crate::repository::Selector, Option<String>)> {
    match ui::choose_workspace(ctx, workspace, Fallback::CurrentDirectoryOnly).await {
        Ok(parent) => {
            let repository = crate::client::workspaces::list(&ctx.paths)
                .await?
                .into_iter()
                .find(|workspace| workspace.id == parent || workspace.name == parent)
                .map(|workspace| workspace.repository_id)
                .ok_or_else(|| {
                    Refusal::new(Reason::NotFound, format!("unknown workspace: {parent}"))
                })?;
            Ok((repository.into(), Some(parent)))
        }
        Err(error) if error.is::<ui::NoCurrentWorkspace>() => {
            if let Some(repository) = copied {
                return Ok((repository.into(), None));
            }
            let repositories = crate::client::repositories::list(&ctx.paths).await?;
            let hint = "pass --workspace or run it in a registered checkout";
            let repository = super::issues::current_repository(ctx, repositories, hint).await?;
            Ok((repository.into(), None))
        }
        Err(error) => Err(error),
    }
}

/// Split the child `split` describes and start preparing it.
pub(super) async fn start_child(ctx: &Context, split: NewSplit) -> Result<StartedWorkspace> {
    let env = crate::env::without_secrets();
    crate::client::operations::start_split(&ctx.paths, split, env).await
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

/// Follow the preparation the daemon started for a workspace a request
/// opened to its end; `None` when setup failed.
pub(super) async fn follow_preparation(
    ctx: &Context,
    started: StartedWorkspace,
) -> Result<Option<Workspace>> {
    let prepared = follow_queuing(ctx, started, None).await?;
    Ok(prepared.map(|prepared| *prepared.workspace))
}

/// Follow the preparation as [`follow_preparation`] does, with the agent it
/// queued; `agent`, the one the request asked for, is queued after a setup
/// failure the user ignored, which ends preparation before it.
async fn follow_queuing(
    ctx: &Context,
    started: StartedWorkspace,
    agent: Option<Box<QueuedLaunch>>,
) -> Result<Option<Prepared>> {
    match super::workspaces::follow_setup(ctx, started).await? {
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
        shell::navigate(&child.path, ctx.json)?;
    }
    Ok(())
}

/// The issue `input` names in the parent workspace's repository.
async fn load_issue(ctx: &Context, parent: &str, input: &str) -> Result<Issue> {
    let context = ItemContext::Workspace(parent.to_owned());
    super::issues::load(ctx, context, input).await
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
