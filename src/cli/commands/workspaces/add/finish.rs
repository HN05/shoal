//! Opening a planned workspace, setting it up and launching its agent.
use anyhow::{Context as _, Result};

use super::{
    agent::ResolvedAddAgent,
    plan::{AddPlan, Creation},
    queue::failing_entry,
    source::load_add_issue,
    target::AddTarget,
};
use crate::{
    cli::{
        agents,
        client::{self, request},
        commands::{
            agent_queue, issues,
            workspaces::{link_pull, run_post_setup, setup_workspace},
        },
        context::Context,
        output::{Palette, Style},
        terminal,
    },
    model::{OpenedWorkspace, Workspace},
    protocol::{ConfigTarget, ItemMethod, WorkspaceMethod},
    shell,
    state::WorkspaceState,
};

pub(in crate::cli) async fn execute_add(ctx: &Context, plan: AddPlan) -> Result<i32> {
    let target = AddTarget {
        selector: plan.repository.clone(),
        repositories: tokio::sync::OnceCell::new(),
    };
    let issue = load_add_issue(ctx, &target, plan.issue.clone()).await;
    // A queued start reports a failed lookup as it reports any other failure.
    let issue = failing_entry(ctx, plan.queue_entry.as_deref(), issue).await?;
    execute_add_with_target(ctx, plan, target, issue).await
}

pub(super) async fn execute_add_with_target(
    ctx: &Context,
    plan: AddPlan,
    target: AddTarget,
    issue: Option<issues::Issue>,
) -> Result<i32> {
    let Some(entry) = plan.queue_entry.clone() else {
        return add_planned(ctx, plan, target, issue).await;
    };
    let _hold = agent_queue::Hold::start(&ctx.paths, &entry).await;
    let result = add_planned(ctx, plan, target, issue).await;
    failing_entry(ctx, Some(&entry), result).await
}

async fn add_planned(
    ctx: &Context,
    plan: AddPlan,
    target: AddTarget,
    issue: Option<issues::Issue>,
) -> Result<i32> {
    let AddPlan {
        repository: _,
        creation,
        place_name,
        issue: _,
        agent,
        args,
        queue_entry,
        chat,
        workspace,
    } = plan;
    let pr = creation.pr.clone();
    let opened = match workspace {
        Some(workspace) => {
            request::<OpenedWorkspace>(&ctx.paths, WorkspaceMethod::ReopenWorkspace { workspace })
                .await?
        }
        None => open_add_workspace(ctx, &target, creation, chat).await?,
    };
    if let Some(entry) = &queue_entry {
        agent_queue::attach(ctx, entry, &opened.workspace).await?;
    }
    let label = match (place_name, &issue) {
        (Some(name), _) => name.render(&opened.workspace.branch),
        (None, Some(issue)) => issue.tab_label(target.repository(ctx).await?),
        (None, None) => opened.workspace.branch.clone(),
    };
    let agent_name = agent.agent.clone().map(String::from);
    terminal::host_workspace(ctx, &opened.workspace, agent_name.as_deref(), &label).await?;
    if let Some(issue) = &issue {
        request::<()>(
            &ctx.paths,
            ItemMethod::SetIssue {
                workspace: opened.workspace.id.clone(),
                url: issue.url.clone(),
                title: Some(issue.title.clone()),
            },
        )
        .await?;
    }
    let name = opened.workspace.name.clone();
    let Some(workspace) = finish_add_workspace(ctx, opened, pr, &agent, issue.is_some()).await?
    else {
        if let Some(entry) = &queue_entry {
            let error = anyhow::anyhow!(
                "setup of {name} did not complete; retry it with `shoal work setup {name}` or skip it with `shoal work setup {name} --skip`"
            );
            agent_queue::fail(ctx, entry, &error).await;
        }
        return Ok(1);
    };
    if let Some(entry) = &queue_entry {
        crate::env::launch_queue_entry(entry);
    }
    agent.launch(ctx, workspace, issue, args).await
}

/// Whether the caller's shell enters the added `workspace`, where `agent`
/// starts unless `None`. An agent that runs in a session this terminal
/// attaches to, given an issue prompt when `prompted`, leaves it where it
/// was, as detaching returns to that shell rather than to the workspace.
async fn enters_added(
    ctx: &Context,
    workspace: &Workspace,
    agent: Option<&ResolvedAddAgent>,
    prompted: bool,
) -> Result<bool> {
    if !terminal::follows_caller(ctx) {
        return Ok(false);
    }
    let Some((agent, chosen)) = agent.and_then(|agent| Some((agent, agent.agent.as_ref()?))) else {
        return Ok(true);
    };
    let settings =
        client::settings(&ctx.paths, ConfigTarget::Workspace(workspace.id.clone())).await?;
    Ok(!agents::runs_in_session(
        chosen,
        &settings,
        prompted,
        agent.tracked,
    ))
}

/// Open the planned workspace for `chat`, or for the user without one.
async fn open_add_workspace(
    ctx: &Context,
    target: &AddTarget,
    creation: Creation,
    chat: Option<String>,
) -> Result<OpenedWorkspace> {
    let Creation {
        path,
        branch,
        existing,
        base,
        git_profile,
        pr: _,
    } = creation;
    let repository = target.selector.clone();
    if let Some(branch) = existing {
        request::<OpenedWorkspace>(
            &ctx.paths,
            WorkspaceMethod::OpenBranch {
                path,
                repository,
                branch,
                git_profile,
                base,
                chat,
            },
        )
        .await
    } else {
        let name = branch.context("a branch name is required")?;
        let workspace = request::<Workspace>(
            &ctx.paths,
            WorkspaceMethod::CreateWorkspace {
                path,
                repository,
                name,
                base,
                git_profile,
                chat,
            },
        )
        .await?;
        Ok(OpenedWorkspace {
            workspace,
            reused: false,
        })
    }
}

/// Set up an opened workspace and link its PR, which needs a ready workspace,
/// before reporting it and entering it unless `agent`, given an issue prompt
/// when `prompted`, takes the terminal.
async fn finish_add_workspace(
    ctx: &Context,
    opened: OpenedWorkspace,
    pr: Option<String>,
    agent: &ResolvedAddAgent,
    prompted: bool,
) -> Result<Option<Workspace>> {
    let OpenedWorkspace {
        mut workspace,
        reused,
    } = opened;
    if workspace.state == WorkspaceState::Preparing {
        let Some(ready) = setup_workspace(ctx, &workspace).await? else {
            return Ok(None);
        };
        workspace = ready;
    }
    if let Some(url) = pr {
        workspace = link_pull(ctx, workspace, url).await?;
    }
    ctx.emit(
        &format!(
            "{} {} on branch {} at {}",
            if reused { "Opened" } else { "Created" },
            Palette::stdout(ctx.json).paint(Style::Heading, &workspace.name),
            workspace.branch,
            workspace.path.display()
        ),
        &workspace,
    )?;
    // The post-setup hook may change how the agent launches, so the shell's
    // destination waits for it; a failed hook starts no agent.
    let post_setup = if reused {
        Ok(())
    } else {
        run_post_setup(ctx, &workspace).await
    };
    let starts = post_setup.is_ok().then_some(agent);
    if enters_added(ctx, &workspace, starts, prompted).await? {
        shell::navigate(&workspace.path, ctx.json)?;
    }
    post_setup?;
    Ok(Some(workspace))
}
