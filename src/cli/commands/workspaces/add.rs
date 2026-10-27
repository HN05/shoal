//! `shoal add`: plan a workspace, then open it here or hand it to a terminal host.
mod agent;
mod dependencies;
mod finish;
mod handoff;
mod plan;
mod source;
mod target;

pub(in crate::cli::commands) use agent::AgentLaunch;
pub(in crate::cli) use finish::execute_add;
pub(in crate::cli) use handoff::start_queued;
pub(in crate::cli) use plan::{AddPlan, Creation};
use std::ffi::OsString;

use anyhow::{Result, ensure};

use self::{
    agent::{AddSettings, AgentChoice, choose_add_agent, resolve_add_agent},
    dependencies::{after_dependencies, launch_dependencies},
    finish::execute_add_with_target,
    handoff::start_beside,
    plan::{check_add_creation, planned_workspace_name},
    source::{pick_add_source, select_add_creation},
    target::resolve_add_target,
};
use crate::{
    cli::{
        commands::{agent_queue, issues, repositories},
        context::Context,
        terminal::PlaceName,
    },
    env,
};

/// How `shoal add` runs once its workspace is planned.
#[derive(Debug, Clone, Default)]
pub(in crate::cli::commands) struct AddOptions {
    /// Run in the current pane instead of opening a Herdr tab.
    pub here: bool,
    /// Focus the new tab or session whatever the `focus` setting says.
    pub focus: bool,
    /// Start the agent even when `max_agents` is reached.
    pub now: bool,
    /// Workspaces, queued agents and issues the agent waits for.
    pub after: Vec<String>,
}

impl AddOptions {
    /// Whether the agent skips the queue: a focused launch is one the user
    /// waits for, so it starts at once as `--now` does.
    fn starts_now(&self) -> bool {
        self.now || self.focus
    }
}

pub(in crate::cli::commands) async fn add(
    ctx: &Context,
    repository: Option<String>,
    creation: Creation,
    issue: Option<String>,
    agent: AgentLaunch,
    args: Vec<OsString>,
    options: AddOptions,
) -> Result<i32> {
    let mut creation = creation;
    creation.path = creation
        .path
        .map(|path| repositories::absolute(ctx, path))
        .transpose()?;
    let target = resolve_add_target(ctx, repository, issue.as_deref(), &agent).await?;
    check_add_creation(ctx, &target, &creation).await?;
    let after = after_dependencies(ctx, &target, &options.after).await?;
    let settings = AddSettings {
        ctx,
        repository: &target.selector,
        effective: tokio::sync::OnceCell::new(),
    };
    let agent = choose_add_agent(&settings, agent).await?;
    let picked = match issue {
        Some(_) => None,
        None => pick_add_source(ctx, &target, &mut creation).await?,
    };
    // A picked issue defaults its agent as --issue does.
    let agent = match (agent, &picked) {
        (AgentChoice::PickerIfInteractive, Some(_)) => {
            choose_add_agent(&settings, AgentLaunch::IssueDefault(None)).await?
        }
        (agent, _) => agent,
    };
    // Look the issue up before any other picker or handoff, so its failures end here.
    let issue = match issue.or(picked) {
        Some(input) => Some(issues::load(ctx, target.repository(ctx).await?, &input).await?),
        None => None,
    };
    let creation = select_add_creation(ctx, &target, creation, issue.as_ref()).await?;
    // An issue or a typed name settles the branch; check it before the agent picker.
    check_add_creation(ctx, &target, &creation).await?;
    let agent = resolve_add_agent(ctx, &settings, agent).await?;
    let place_name = match &issue {
        Some(issue) => Some(PlaceName::new(
            None,
            target.repository(ctx).await?,
            Some(issue),
        )),
        None => None,
    };
    let mut plan = AddPlan {
        repository: target.selector.clone(),
        creation,
        place_name,
        issue: issue.as_ref().map(|issue| issue.url.clone()),
        agent,
        args,
        queue_entry: None,
        chat: env::chat(),
        workspace: None,
    };
    ensure!(
        options.after.is_empty() || plan.agent_name().is_some(),
        "--after queues an agent; name one with --agent"
    );
    if let Some(agent) = plan.agent_name() {
        let title = issue.as_ref().map(|issue| issue.title.clone());
        let name = planned_workspace_name(ctx, &target, &plan.creation).await?;
        let launch = agent_queue::Launch {
            agent,
            name,
            issue_title: title,
            now: options.starts_now(),
            after: launch_dependencies(ctx, &target, issue.as_ref(), &options, after).await?,
        };
        match agent_queue::admit(ctx, &plan, launch).await? {
            agent_queue::Admission::Start { entry, base } => {
                plan.queue_entry = entry;
                plan.creation.build_on(base);
            }
            agent_queue::Admission::Queued(entry, waiting) => {
                return agent_queue::report_queued(ctx, &entry, waiting);
            }
        }
    }
    let handoff = start_beside(ctx, &target, &mut plan, issue.as_ref(), options).await;
    if let Some(entry) = &plan.queue_entry
        && let Err(error) = &handoff
    {
        agent_queue::fail(ctx, entry, error).await;
    }
    if handoff? {
        return Ok(0);
    }
    execute_add_with_target(ctx, plan, target, issue).await
}
