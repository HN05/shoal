//! What an addition creates: its branch options and the plan a terminal host carries out.
use std::{ffi::OsString, path::PathBuf};

use anyhow::Result;

use super::{agent::ResolvedAddAgent, target::AddTarget};
use crate::{
    agent::Agent,
    cli::{
        agents,
        commands::{issues, repositories},
        context::Context,
        terminal::PlaceName,
    },
    config::Effective,
    env, git,
    model::{QueueRequest, Repository, Workspace},
    protocol::{CreationOptions, ItemContext, PullHead, operations::QueuedLaunch},
};

#[derive(Default, serde::Serialize, serde::Deserialize)]
pub(in crate::cli) struct Creation {
    pub path: Option<PathBuf>,
    pub branch: Option<String>,
    pub existing: Option<String>,
    pub base: Option<String>,
    pub git_profile: Option<String>,
    /// The PR whose head branch is opened; it is linked once the workspace is ready.
    #[serde(default)]
    pub pr: Option<String>,
    #[serde(default)]
    pub options: CreationOptions,
    /// The head of a PR from a fork, opened in a workspace at a commit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pull_head: Option<PullHead>,
}

impl Creation {
    /// The creation with its path resolved against the current directory.
    pub(super) fn absolute(mut self, ctx: &Context) -> Result<Self> {
        self.path = self
            .path
            .map(|path| repositories::absolute(ctx, path))
            .transpose()?;
        Ok(self)
    }

    /// Build a new branch on `base`, the branch of a done dependency, unless
    /// the addition names its own base or opens an existing branch.
    pub(in crate::cli::commands) fn build_on(&mut self, base: Option<String>) {
        if self.base.is_none() && self.existing.is_none() {
            self.base = base;
        }
    }
}

/// The plan to add `creation` in the chat this runs in, if any, starting
/// `agent` with the found `issue` or with `args`.
pub(super) async fn plan_addition(
    ctx: &Context,
    target: &AddTarget,
    creation: Creation,
    issue: Option<&issues::Issue>,
    agent: ResolvedAddAgent,
    args: Vec<OsString>,
) -> Result<AddPlan> {
    let place_name = match issue {
        Some(issue) => Some(PlaceName::new(
            None,
            target.repository(ctx).await?,
            Some(issue),
        )),
        None => None,
    };
    Ok(AddPlan {
        repository: target.selector.clone(),
        creation,
        place_name,
        issue: issue.map(|issue| issue.url.clone()),
        agent,
        args,
        prompt: None,
        mode: None,
        fork: None,
        queue_entry: None,
        chat: env::in_chat(),
        parent: None,
        workspace: None,
    })
}

/// The workspace name `creation` will derive. An existing branch opens under
/// its local name, so a remote's prefix is left out, as opening it does.
pub(super) async fn planned_workspace_name(
    ctx: &Context,
    target: &AddTarget,
    creation: &Creation,
) -> Result<String> {
    let branch = match (&creation.branch, &creation.existing) {
        (Some(branch), _) => branch.clone(),
        (None, Some(selector)) => match git::strip_local(selector) {
            Some(local) => local.to_owned(),
            None => {
                let repo = target.repository(ctx).await?;
                git::remote_branch(&repo.path, selector)
                    .await?
                    .map_or_else(|| selector.clone(), |(_, branch)| branch)
            }
        },
        (None, None) => String::new(),
    };
    Ok(crate::validate::derive_workspace_name(&branch))
}

/// Refuse `launch` when it cannot run under `settings`: an agent that is
/// off or missing, a fork it cannot start from, or a model its launch
/// cannot pass on.
pub(crate) fn check_queued_launch(launch: &QueuedLaunch, settings: &Effective) -> Result<()> {
    let agent: Agent = launch.agent.parse()?;
    agents::ensure_available(agent.name(), settings)?;
    if launch.fork.is_some() {
        agents::ensure_forks(settings, agent.name())?;
    }
    let model = agents::resolve_model(settings, agent.name(), launch.model.clone());
    agents::check_model(settings, agent.name(), &model)
}

/// Agent arguments as a queued launch carries them, refusing one that is
/// not UTF-8.
pub(super) fn launch_args(args: Vec<OsString>) -> Result<Vec<String>> {
    (args.into_iter())
        .map(|arg| {
            arg.into_string()
                .map_err(|arg| anyhow::anyhow!("agent argument {arg:?} is not UTF-8"))
        })
        .collect()
}

/// The queue request of `launch` in `workspace`, which the agent queue
/// starts detached, beside the caller's terminal when the launch names it
/// or else in the default host.
pub(crate) fn queued_agent_request(
    workspace: &Workspace,
    mut launch: QueuedLaunch,
) -> Result<QueueRequest> {
    let env = (launch.env.take())
        .unwrap_or_else(|| crate::cli::terminal::detached_start_environment(None));
    let issue_title = launch.issue.as_ref().map(|issue| issue.title.clone());
    let agent = launch.agent.clone();
    let plan = AddPlan::agent_in(workspace, launch)?;
    Ok(QueueRequest {
        repository: workspace.repository_id.as_str().into(),
        name: workspace.name.clone(),
        agent,
        mode: plan.mode.clone(),
        issue_title,
        plan: serde_json::to_string(&plan)?,
        env,
        now: false,
        workspace: Some(workspace.id.clone()),
        after: Vec::new(),
    })
}

#[derive(serde::Serialize, serde::Deserialize)]
pub(in crate::cli) struct AddPlan {
    pub repository: crate::repository::Selector,
    pub creation: Creation,
    /// The name of the place a host opens for the addition.
    #[serde(default)]
    pub place_name: Option<PlaceName>,
    /// The found issue's URL; its body can exceed what a Herdr tab's
    /// environment carries, so the worker looks it up again.
    pub(super) issue: Option<String>,
    pub(super) agent: ResolvedAddAgent,
    pub(super) args: Vec<OsString>,
    /// The agent's subject when no issue gives it one, which a launcher
    /// with `{prompt}` receives there.
    #[serde(default)]
    pub(super) prompt: Option<String>,
    /// The mode the agent starts in; the default mode when `None`.
    #[serde(default)]
    pub(in crate::cli::commands) mode: Option<String>,
    /// The session the agent starts from a fork of.
    #[serde(default)]
    pub(super) fork: Option<String>,
    /// The agent queue entry whose slot this launch uses.
    #[serde(default)]
    pub queue_entry: Option<String>,
    /// The chat that planned the addition, which the workspace belongs to
    /// unless `parent` names another space.
    #[serde(default)]
    pub(super) chat: Option<String>,
    /// The space the workspace goes below, which `--parent` names.
    #[serde(default)]
    pub(super) parent: Option<String>,
    /// The existing workspace a launch in it reopens; it never creates one.
    #[serde(default)]
    pub(super) workspace: Option<String>,
}

impl AddPlan {
    /// `launch` in the existing `workspace`. It runs detached, so it stays
    /// tracked.
    pub(in crate::cli::commands) fn agent_in(
        workspace: &Workspace,
        launch: QueuedLaunch,
    ) -> Result<Self> {
        let QueuedLaunch {
            agent,
            prompt,
            mode,
            model,
            args,
            fork,
            issue,
            env: _,
        } = launch;
        Ok(Self {
            repository: workspace.repository_id.as_str().into(),
            creation: Creation {
                existing: workspace.branch.clone(),
                ..Default::default()
            },
            place_name: None,
            issue: issue.map(|issue| issue.url),
            agent: ResolvedAddAgent {
                agent: Some(agent.parse()?),
                tracked: true,
                model,
            },
            args: args.into_iter().map(OsString::from).collect(),
            prompt,
            mode,
            fork,
            queue_entry: None,
            chat: None,
            parent: None,
            workspace: Some(workspace.id.clone()),
        })
    }

    /// Start the agent in the mode `options` names, below the space it
    /// names, from a fork of the session it names.
    pub(super) fn start_as(&mut self, options: &super::AddOptions) {
        self.mode.clone_from(&options.mode);
        self.parent.clone_from(&options.parent);
        self.fork.clone_from(&options.fork);
    }

    /// Whether the plan opens `workspace`'s branch in its repository without
    /// creating anything.
    pub(in crate::cli::commands) fn opens(&self, workspace: &Workspace) -> bool {
        self.workspace.as_deref() == Some(workspace.id.as_str())
            && self.repository.value() == workspace.repository_id
            && self.creation.branch.is_none()
            && self.creation.path.is_none()
            && workspace.branch.is_some()
            && self.creation.existing == workspace.branch
    }

    /// The agent's name, when the addition starts one.
    pub fn agent_name(&self) -> Option<String> {
        self.agent.agent.clone().map(String::from)
    }

    /// The found issue's URL.
    pub fn issue_url(&self) -> Option<&str> {
        self.issue.as_deref()
    }

    /// Where the plan reads from the forge: the existing workspace it
    /// launches in, else the chat that planned it, else where a command
    /// reads from the forge of its repository `repo`.
    pub async fn item_context(&self, ctx: &Context, repo: &Repository) -> Result<ItemContext> {
        Ok(match (&self.workspace, &self.chat) {
            (Some(workspace), _) => ItemContext::Workspace(workspace.clone()),
            (None, Some(chat)) => ItemContext::Chat(chat.clone()),
            (None, None) => crate::cli::commands::issues::read_context(ctx, repo).await?,
        })
    }

    /// Whether the agent starts with input: an issue prompt or forwarded arguments.
    /// A workspace shell or an agent without input waits for the user.
    pub fn runs_unattended(&self) -> bool {
        self.agent.agent.is_some()
            && (self.issue.is_some() || self.prompt.is_some() || !self.args.is_empty())
    }

    pub fn label(&self) -> String {
        let branch = self
            .creation
            .branch
            .as_deref()
            .or(self.creation.existing.as_deref())
            .unwrap_or("shoal start");
        self.place_name
            .as_ref()
            .map_or_else(|| branch.to_owned(), |name| name.render(branch, None))
    }
}

/// Refuse what creation would refuse in the given repository, branch and
/// options before a prompt asks for the rest of the addition. Only a terminal
/// prompts; without one, creation refuses the arguments itself, except that
/// a repository other than the chat's is refused before a launch is queued.
pub(super) async fn check_add_creation(
    ctx: &Context,
    target: &AddTarget,
    creation: &Creation,
) -> Result<()> {
    let chat = env::in_chat();
    if !ctx.interactive() && chat.is_none() {
        return Ok(());
    }
    let planned = crate::client::workspaces::PlannedWorkspace {
        repository: target.selector.clone(),
        branch: creation.branch.clone(),
        existing: creation.existing.clone(),
        base: creation.base.clone(),
        git_profile: creation.git_profile.clone(),
        path: creation.path.clone(),
        parent: chat,
        options: creation.options,
    };
    crate::client::workspaces::check_creation(&ctx.paths, planned).await
}

#[cfg(test)]
mod tests;
