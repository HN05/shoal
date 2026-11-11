//! `shoal start --in` in a mode that starts in a workspace: the agent of an
//! existing workspace, started as a new workspace's is, with no space
//! created. The workspace keeps its place in the tree, its name and links.
use anyhow::{Result, ensure};

use super::{
    AddOptions, Planned,
    agent::{AddSettings, AgentLaunch, choose_add_agent, resolve_add_agent},
    mode::choose_add_mode,
    plan::start_as,
    queue::named_blockers,
    source::load_add_issue,
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

/// Start `agent`, else the configured or a picked one, as the worker of the
/// existing `workspace`, on `options.prompt`, else the workspace's issue.
/// The daemon refuses it while the workspace has an agent of its own.
pub(in crate::cli::commands) async fn start_worker(
    ctx: &Context,
    workspace: SpaceSelector,
    agent: Option<Agent>,
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
    let agent = choose_add_agent(&settings, AgentLaunch::IssueDefault(agent)).await?;
    let agent = resolve_add_agent(ctx, &settings, agent, options.model.clone()).await?;
    ensure!(
        agent.agent.is_some(),
        "a workspace's worker needs an agent; name one with --agent"
    );
    // A task replaces the issue as the agent's subject.
    let issue = match &options.prompt {
        Some(_) => None,
        None => load_add_issue(ctx, &target, workspace.links.issue.clone()).await?,
    };
    let mut plan = worker_plan(&workspace, agent, issue.as_ref());
    start_as(&mut plan, &options, mode);
    let planned = Planned {
        plan,
        target,
        issue,
        blockers,
    };
    super::start_planned(ctx, planned, &settings, options).await
}

/// The plan that starts `agent` in `workspace` on `issue`, opening its
/// branch under its name and creating nothing.
fn worker_plan(
    workspace: &Workspace,
    agent: ResolvedAddAgent,
    issue: Option<&issues::Issue>,
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
        args: Vec::new(),
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
