//! Opening a planned workspace, setting it up and launching its agent.
use std::ffi::OsString;

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
    model::{OpenedWorkspace, QueueEntry, Workspace},
    protocol::{ConfigTarget, ItemMethod, WorkspaceMethod},
    shell,
    state::{QueueState, WorkspaceState},
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
        return add_planned(ctx, plan, target, issue, false).await;
    };
    let _hold = agent_queue::Hold::start(&ctx.paths, &entry).await;
    let result = add_planned(ctx, plan, target, issue, false).await;
    failing_entry(ctx, Some(&entry), result).await
}

/// Open the planned workspace and launch its agent here, or with `queued`
/// queue the agent there.
pub(super) async fn add_planned(
    ctx: &Context,
    plan: AddPlan,
    target: AddTarget,
    issue: Option<issues::Issue>,
    queued: bool,
) -> Result<i32> {
    let AddPlan {
        repository: _,
        creation,
        place_name,
        issue: _,
        agent,
        args,
        prompt,
        fork,
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
    let (branch, title) = (opened.workspace.branch_or_name(), &opened.workspace.title);
    let label = match (place_name, &issue, title) {
        (Some(name), _, title) => name.render(branch, title.as_deref()),
        (None, _, Some(title)) => title.clone(),
        (None, Some(issue), None) => issue.tab_label(target.repository(ctx).await?),
        (None, None, None) => branch.to_owned(),
    };
    let agent_name = agent.agent.clone().map(String::from);
    // A queued agent starts detached, so the caller's terminal, such as a
    // chat's session, stays its own.
    let queued = queued && agent_name.is_some();
    if !queued {
        terminal::host_workspace(ctx, &opened.workspace, agent_name.is_some(), &label).await?;
    }
    if let Some(issue) = &issue {
        request::<()>(
            &ctx.paths,
            ItemMethod::SetIssue {
                space: opened.workspace.id.clone(),
                url: issue.url.clone(),
                title: Some(issue.title.clone()),
            },
        )
        .await?;
    }
    let name = opened.workspace.name.clone();
    let own_prompt = prompt.clone();
    let prompt = issue.as_ref().map(issues::Issue::prompt).or(prompt);
    let Some(workspace) = finish_add_workspace(ctx, opened, pr, &agent, prompt.is_some()).await?
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
    if queued {
        let input = (own_prompt, args, fork);
        return queue_added_agent(ctx, &workspace, agent, issue, input).await;
    }
    agent.launch(ctx, workspace, prompt, args, fork).await
}

/// Queue `agent` in the added `workspace`, prompted with `issue` or else the plan's own prompt, with its arguments
/// and fork. The daemon starts it outside this process, so it gets the
/// workspace's own scope, as `shoal split --agent` does.
async fn queue_added_agent(
    ctx: &Context,
    workspace: &Workspace,
    agent: ResolvedAddAgent,
    issue: Option<issues::Issue>,
    (prompt, args, fork): (Option<String>, Vec<OsString>, Option<String>),
) -> Result<i32> {
    let chosen = agent.agent.context("the queued launch has no agent")?;
    let name = String::from(chosen.clone());
    let mut plan = AddPlan::agent_in(workspace, chosen, agent.model, issue.as_ref(), args, fork);
    plan.prompt = prompt.filter(|_| plan.issue.is_none());
    let title = issue.map(|issue| issue.title);
    let entry = agent_queue::queue_in(ctx, &plan, workspace, name, title).await?;
    report_queued_in(ctx, workspace, &entry)
}

/// Report the agent queued in the added `workspace`.
fn report_queued_in(ctx: &Context, workspace: &Workspace, entry: &QueueEntry) -> Result<i32> {
    let text = match entry.state {
        QueueState::Queued => format!(
            "Queued {} in {} (position {}); it starts {}",
            entry.agent,
            workspace.name,
            entry.position.unwrap_or(1),
            agent_queue::starts_when(entry)
        ),
        _ => format!("Starting {} in {}", entry.agent, workspace.name),
    };
    ctx.emit(&text, entry)?;
    Ok(0)
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
        pr,
        options,
        pull_head,
    } = creation;
    let repository = target.selector.clone();
    if let Some(head) = pull_head {
        let url = pr.context("a PR head needs its PR")?;
        let method = WorkspaceMethod::OpenPullHead {
            repository,
            url,
            head,
            parent: chat,
            options,
        };
        return request(&ctx.paths, method).await;
    }
    if let Some(branch) = existing {
        request::<OpenedWorkspace>(
            &ctx.paths,
            WorkspaceMethod::OpenBranch {
                path,
                repository,
                branch,
                git_profile,
                base,
                parent: chat,
                options,
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
                parent: chat,
                options,
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
    // The hook runs with setup, so it goes on when this process goes.
    let mut post_setup = None;
    if workspace.state == WorkspaceState::Preparing {
        let Some(set_up) = setup_workspace(ctx, &workspace, !reused).await? else {
            return Ok(None);
        };
        (workspace, post_setup) = (set_up.workspace, set_up.post_setup);
    }
    if let Some(url) = pr {
        workspace = link_pull(ctx, workspace, url).await?;
    }
    ctx.emit(
        &format!(
            "{} {} {}",
            if reused { "Opened" } else { "Created" },
            Palette::stdout(ctx.json).paint(Style::Heading, &workspace.name),
            super::super::location_phrase(&workspace),
        ),
        &workspace,
    )?;
    // The post-setup hook may change how the agent launches, so the shell's
    // destination waits for it; a failed hook starts no agent.
    let post_setup = match post_setup {
        Some(result) => result,
        None if reused => Ok(()),
        None => run_post_setup(ctx, &workspace).await,
    };
    let starts = post_setup.is_ok().then_some(agent);
    if enters_added(ctx, &workspace, starts, prompted).await? {
        shell::navigate(&workspace.path, ctx.json)?;
    }
    post_setup?;
    Ok(Some(workspace))
}
