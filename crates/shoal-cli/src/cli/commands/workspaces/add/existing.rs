//! `shoal start --in` in a mode that starts in a workspace, or with a
//! `--branch` a workspace already has: the agent of an existing workspace,
//! started as a new workspace's is, with no space created. The workspace
//! keeps its place in the tree, its name and links.
use std::ffi::OsString;

use anyhow::{Result, ensure};

use super::{
    AddOptions, Planned,
    agent::{AddSettings, AgentLaunch, choose_add_agent, resolve_add_agent},
    mode::choose_add_mode,
    plan::start_as,
    queue::named_blockers,
    source::load_linked_issue,
    target::AddTarget,
};
use crate::client::space_names::SpaceSelector;
use crate::{
    agent::Agent,
    cli::{commands::issues, context::Context},
    model::Workspace,
    protocol::CreationOptions,
    start_plan::{AddPlan, Creation, ResolvedAddAgent},
};

/// Start `agent`, else on a terminal a picked one, as the worker of the
/// existing `workspace`, with `args` forwarded as any start forwards them,
/// else on the workspace's issue.
/// The daemon refuses it while the workspace has an agent of its own.
/// Without an agent it reopens the workspace, as a new workspace's start
/// without one opens it.
pub(in crate::cli::commands) async fn start_worker(
    ctx: &Context,
    workspace: SpaceSelector,
    agent: Option<Agent>,
    args: Vec<OsString>,
    mut options: AddOptions,
) -> Result<i32> {
    // Without its token, the daemon would take a workspace process's start
    // for the user's.
    ensure!(
        crate::env::is_scoped() || !crate::env::inherits_scope(&ctx.paths.state),
        "workspace processes start agents only with their scope token"
    );
    let workspace = crate::client::workspaces::inspect(&ctx.paths, workspace)
        .await?
        .workspace;
    let target = AddTarget {
        selector: (&workspace.repository_id).into(),
        repositories: tokio::sync::OnceCell::new(),
    };
    let blockers = named_blockers(ctx, &target, &options.blockers).await?;
    let settings = AddSettings::workspace(ctx, &workspace.id);
    let repository = Some(target.selector.clone());
    crate::cli::commands::permissions::ensure_held(ctx, repository, &options.narrowing).await?;
    let mode = choose_add_mode(ctx, &settings, options.mode.take()).await?;
    let agent = choose_add_agent(&settings, AgentLaunch::Add(agent)).await?;
    let agent = resolve_add_agent(ctx, &settings, agent, options.model.clone()).await?;
    // A task after `--` replaces the issue as the agent's subject.
    let issue = match agent.agent.is_some() && args.is_empty() {
        true => load_linked_issue(ctx, &target, workspace.links.issue.clone()).await?,
        false => None,
    };
    let mut plan = worker_plan(&workspace, agent, issue.as_ref(), args);
    start_as(&mut plan, &options, mode);
    let planned = Planned {
        plan,
        target,
        issue,
        blockers,
    };
    super::start_planned(ctx, planned, &settings, options).await
}

/// Start the worker of `workspace`, which already has the branch a start
/// named, as `--in` starts it, with `args` forwarded, refusing what only
/// the new workspace of `creation`, on `issue` below `options.parent`, takes.
pub(super) async fn start_on_branch(
    ctx: &Context,
    workspace: Workspace,
    (creation, issue): (&Creation, Option<&str>),
    agent: AgentLaunch,
    args: Vec<OsString>,
    options: AddOptions,
) -> Result<i32> {
    let new_space = [
        ("--issue", issue.is_some()),
        ("--name", creation.options.name.is_some()),
        ("--parent", options.parent.is_some()),
        ("--base", creation.base.is_some()),
        ("--path", creation.path.is_some()),
        ("--git-profile", creation.git_profile.is_some()),
        ("--copy", creation.copy.is_some()),
        ("--skip-setup", creation.options.skip_setup),
        ("--disposable", creation.options.disposable),
    ];
    if let Some((flag, _)) = new_space.iter().find(|(_, set)| *set) {
        anyhow::bail!(
            "{flag} applies to a new workspace, and workspace {} already has branch {}; the start goes into it as --in does",
            workspace.number,
            workspace.branch.as_deref().unwrap_or_default()
        );
    }
    let selector = SpaceSelector::id(&workspace.id);
    start_worker(ctx, selector, agent.named(), args, options).await
}

/// The plan that starts `agent` in `workspace` on `issue` with `args`,
/// opening its branch under its name and creating nothing.
fn worker_plan(
    workspace: &Workspace,
    agent: ResolvedAddAgent,
    issue: Option<&issues::Issue>,
    args: Vec<OsString>,
) -> AddPlan {
    AddPlan {
        repository: (&workspace.repository_id).into(),
        creation: Creation {
            existing: workspace.branch.clone(),
            options: CreationOptions {
                name: Some(workspace.name.clone()),
                ..Default::default()
            },
            ..Default::default()
        },
        place_name: None,
        issue: issue.map(|issue| issue.url.clone()),
        agent,
        args,
        prompt: None,
        mode: None,
        narrowing: Default::default(),
        fork: None,
        queue_entry: None,
        chat: None,
        parent: None,
        workspace: Some(workspace.id.clone()),
    }
}
