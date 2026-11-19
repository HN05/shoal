//! The one [`Launch`] every start sends, whatever its space: this process
//! reads only what needs it, such as where the start is made and the
//! session `--fork` forks, and on a terminal what a picker picks, then
//! places the agent where it runs in this caller.
use anyhow::{Context as _, Result};
use shoal_protocol::launches::{IssueChoice, Launch, SpaceChoice, StartTarget};
use shoal_protocol::space_names::SpaceSelector;

use super::{fork_from, mode_repository};
use crate::{
    cli::{
        agents,
        args::create::StartArgs,
        commands::{copies, issues, repositories, workspaces},
        context::Context,
        ui::{self, ModeChoice},
    },
    config::Effective,
    forge::items::{link::ItemKind, reference::ItemInput},
    launches::LaunchReads,
    model::{Repository, SpaceKind},
    protocol::ConfigTarget,
};

/// The settings of the registered repository `args` name without asking,
/// else the global config's.
async fn launch_settings(ctx: &Context, args: &StartArgs) -> Result<Effective> {
    let target = match mode_repository(ctx, args).await? {
        Some(repo) => ConfigTarget::Repository(repo.id.into()),
        None => ConfigTarget::Global,
    };
    workspaces::DaemonReads(ctx).settings(target).await
}

/// `launch` in the `checked` repository, such as one the start registered,
/// in place of the positional argument that named it, beside a positional
/// issue or link.
pub(in crate::cli::commands::start) fn checked_launch(
    mut launch: Launch,
    checked: Option<&crate::repository::Selector>,
) -> Launch {
    if let Some(checked) = checked {
        if matches!(launch.target, Some(StartTarget::Repository(_))) {
            launch.target = None;
        }
        launch.repository = Some(checked.value().to_owned());
    }
    launch
}

/// The launch `args` ask for, made from this process: the copy and the
/// session `--fork` forks it reads, the agent's task, below the chat it
/// runs in, in its directory and with its terminal's environment.
async fn located_launch(ctx: &Context, args: &StartArgs) -> Result<Launch> {
    let mut launch = checked_launch(args.launch(), args.checked_repository.as_ref());
    let copied = copies::source(ctx, args.copy.clone()).await?;
    if let Some(copied) = &copied {
        copied.copy_into(&mut launch)?;
    }
    if args.fork {
        let copied = copied.map(|copy| copy.source.workspace);
        let (parent, agent, fork) =
            fork_from(ctx, copied, args.parent.clone(), args.agent.clone()).await?;
        (launch.parent, launch.agent, launch.fork) = (parent, agent, fork);
    }
    launch.task = workspaces::launch_args(args.args.clone())?;
    launch.path = (launch.path.take())
        .map(|path| repositories::absolute(ctx, path))
        .transpose()?;
    // A start in a chat puts its space below the chat.
    launch.parent = (launch.parent.take()).or(crate::env::own_chat().map(SpaceSelector::Id));
    launch.directory = ui::current_directory()?;
    launch.env = Some(crate::terminal::start_environment());
    launch.setup_env = Some(crate::env::without_secrets());
    Ok(launch)
}

/// How `args` run their agent once the launch records its space.
fn options(args: &StartArgs) -> workspaces::AddOptions {
    workspaces::AddOptions {
        here: args.here,
        focus: args.focus,
        wait: args.wait,
        ..Default::default()
    }
}

/// Have the daemon start the new workspace `args` ask for in `mode`, which
/// a terminal-free start checks under the repository's settings.
pub(in crate::cli::commands::start) async fn workspace(
    ctx: &Context,
    args: StartArgs,
    mode: ModeChoice,
) -> Result<i32> {
    let settings = launch_settings(ctx, &args).await?;
    let launch = located_launch(ctx, &args).await?;
    workspaces::launch_workspace(ctx, launch, mode, (&options(&args), &settings)).await
}

/// Have the daemon start the new chat `args` ask for in `mode`, in a
/// repository's checkout, or with `--in` a workspace's worktree, unless
/// `in_repository` is false, which starts it without one. Its agent and
/// model and what the daemon would refuse of it are checked before a
/// terminal picks what the launch leaves to a picker.
pub(in crate::cli::commands::start) async fn chat(
    ctx: &Context,
    args: StartArgs,
    (mode, in_repository): (ModeChoice, bool),
) -> Result<i32> {
    let mut launch = Launch {
        chat: true,
        ..located_launch(ctx, &args).await?
    };
    if ctx.interactive() && in_repository {
        name_repository(ctx, &mut launch).await?;
    }
    // A chat without a repository reads the global config alone.
    let repository = match in_repository {
        true => chat_repository(ctx, &launch).await?,
        false => None,
    };
    let settings = chat_settings(ctx, repository.as_ref()).await?;
    // Another machine checks and picks its own agents.
    let here = launch.machine.is_none();
    if here {
        launch.agent = crate::agents::checked_agent(&settings, launch.agent.take(), &launch.model)?;
    }
    check_chat(ctx, &launch, repository.as_ref()).await?;
    launch.mode = match ctx.interactive() {
        true => Some(mode.chosen(
            ctx,
            &settings.modes,
            ui::ModeFit::in_repository(SpaceKind::Chat),
        )?),
        false => mode.named(),
    };
    pick_plan_issue(ctx, &mut launch, (repository.as_ref(), &settings)).await?;
    if here {
        launch.agent = agents::start_agent(ctx, &settings, launch.agent.take())?;
    }
    super::place(ctx, launch, &settings, &options(&args)).await
}

/// On a terminal, name the repository of the chat `launch`, which runs in
/// no workspace it names: the one of the issue it starts on, which a
/// terminal offers to register, else, when nothing names one, the current
/// or a picked one.
async fn name_repository(ctx: &Context, launch: &mut Launch) -> Result<()> {
    if launch.in_workspace.is_some() {
        return Ok(());
    }
    let (named, issue) = crate::launches::chat_target(
        launch.target.clone(),
        launch.repository.clone(),
        launch.issue.clone(),
        false,
    )?;
    let named = named_or_parents(ctx, named, issue.as_ref(), launch.parent.as_ref()).await?;
    let retry = "shoal start --mode chat --issue <url> --repo <repository>";
    let repo = match &issue {
        Some(issue) => {
            let input = issue.named();
            issues::item_repository(ctx, ItemKind::Issue, input, named, retry).await?
        }
        // A repository the launch names is its own, refused when unknown,
        // never one a picker replaces.
        None if let Some(named) = named => {
            let repos = crate::client::repositories::list(&ctx.paths).await?;
            let selector = crate::repository::named_in(named, launch.directory.as_deref())?;
            crate::repository::select(&repos, selector).await?;
            return Ok(());
        }
        None => {
            let reads = workspaces::DaemonReads(ctx);
            if crate::launches::shape::launch_repository(&reads, launch)
                .await?
                .is_some()
            {
                return Ok(());
            }
            let repos = crate::client::repositories::list(&ctx.paths).await?;
            let current = issues::current_repository(ctx, repos, repositories::REPOSITORY);
            launch.repository = Some(current.await?.to_string());
            return Ok(());
        }
    };
    launch.repository = Some(repo.id.to_string());
    Ok(())
}

/// The repository `named`, else for a chat on no issue URL that of the
/// space `parent` names, which a child shares, by ID.
async fn named_or_parents(
    ctx: &Context,
    named: Option<String>,
    issue: Option<&IssueChoice>,
    parent: Option<&shoal_protocol::space_names::SpaceSelector>,
) -> Result<Option<String>> {
    let url = matches!(issue, Some(IssueChoice::Named(ItemInput::Url(_))));
    let (None, false, Some(parent)) = (&named, url, parent) else {
        return Ok(named);
    };
    let id = workspaces::DaemonReads(ctx)
        .space_repository(parent)
        .await?;
    Ok(id.map(|id| id.to_string()))
}

/// The registered repository of the chat `launch` starts: that of the
/// workspace whose worktree it runs in, else the one it names or its
/// parent's or directory's; none for a chat without one.
async fn chat_repository(ctx: &Context, launch: &Launch) -> Result<Option<Repository>> {
    let reads = workspaces::DaemonReads(ctx);
    let Some(workspace) = (launch.in_workspace.as_ref()).and_then(SpaceChoice::named) else {
        return crate::launches::shape::launch_repository(&reads, launch).await;
    };
    let inspection = crate::client::workspaces::inspect(&ctx.paths, workspace.clone()).await?;
    let id = inspection.workspace.repository().clone();
    let repos = crate::client::repositories::list(&ctx.paths).await?;
    Ok(repos.into_iter().find(|repo| repo.id == id))
}

/// The settings a chat in `repository` starts under, as the daemon reads
/// them for it: its repository's, or the global config's without one.
async fn chat_settings(ctx: &Context, repository: Option<&Repository>) -> Result<Effective> {
    let target = match repository {
        Some(repo) => ConfigTarget::Repository(repo.id.clone().into()),
        None => ConfigTarget::Global,
    };
    workspaces::DaemonReads(ctx).settings(target).await
}

/// Refuse what the daemon would refuse of the chat `launch` in
/// `repository` before a picker asks for the rest: a workspace it runs in
/// that is not ready there, permissions it gives that this process does
/// not hold, the parent it names, and an issue it names that cannot be
/// read.
async fn check_chat(ctx: &Context, launch: &Launch, repository: Option<&Repository>) -> Result<()> {
    ensure_hosting_workspace(ctx, launch).await?;
    let selector = repository.map(|repo| crate::repository::Selector::from(repo.id.as_str()));
    super::super::permissions::ensure_held(ctx, selector, &launch.permissions).await?;
    check_parent(ctx, launch, repository).await?;
    check_issue(ctx, launch, repository).await
}

/// Refuse the workspace whose worktree the chat `launch` runs in unless it
/// is ready with its worktree there.
async fn ensure_hosting_workspace(ctx: &Context, launch: &Launch) -> Result<()> {
    let Some(workspace) = (launch.in_workspace.as_ref()).and_then(SpaceChoice::named) else {
        return Ok(());
    };
    let workspace = crate::client::workspaces::inspect(&ctx.paths, workspace.clone())
        .await?
        .workspace;
    anyhow::ensure!(
        workspace.state == crate::state::WorkspaceState::Ready,
        "workspace {} is {}, and a chat runs only in a ready workspace",
        workspace.name,
        workspace.state
    );
    anyhow::ensure!(
        workspace.path.is_dir(),
        "the worktree of workspace {} is missing at {}",
        workspace.name,
        workspace.path.display()
    );
    Ok(())
}

/// Refuse the issue the chat `launch` in `repository` names when it cannot
/// be read, before the daemon records a chat on it.
async fn check_issue(
    ctx: &Context,
    launch: &Launch,
    repository: Option<&Repository>,
) -> Result<()> {
    let in_workspace = launch.in_workspace.is_some();
    let (_, issue) = crate::launches::chat_target(
        launch.target.clone(),
        launch.repository.clone(),
        launch.issue.clone(),
        in_workspace,
    )?;
    let (Some(input), Some(repo)) = (issue.as_ref().and_then(IssueChoice::named), repository)
    else {
        return Ok(());
    };
    let context = issues::read_context(ctx, repo).await?;
    issues::load(ctx, context, input.clone()).await.map(drop)
}

/// Refuse the parent the chat `launch` in `repository` names, where the
/// daemon would refuse it, before a picker asks for the rest.
async fn check_parent(
    ctx: &Context,
    launch: &Launch,
    repository: Option<&Repository>,
) -> Result<()> {
    let Some(parent) = &launch.parent else {
        return Ok(());
    };
    let repository = repository.map(|repo| repo.id.clone().into());
    let workspace = (launch.in_workspace.as_ref())
        .and_then(SpaceChoice::named)
        .cloned();
    crate::client::chats::check_parent(&ctx.paths, repository, workspace, parent.clone()).await
}

/// The issue a start in plan mode that asks for one without naming it
/// plans, picked on a terminal among the open issues labelled `[issue]
/// plan_label` of its `repository`, under its `settings`; a launch that
/// names one keeps it.
async fn pick_plan_issue(
    ctx: &Context,
    launch: &mut Launch,
    (repository, settings): (Option<&Repository>, &Effective),
) -> Result<()> {
    if !matches!(launch.issue, Some(IssueChoice::Pick)) || !ctx.interactive() {
        return Ok(());
    }
    let repo = repository.context("name the repository whose issue the plan settles")?;
    let mode = (launch.mode.clone()).unwrap_or(crate::config::modes::ModeName::DEFAULT);
    let issue = super::issue::picked(ctx, repo, &settings.issue, &mode).await?;
    launch.issue = Some(IssueChoice::Named(issue));
    Ok(())
}
