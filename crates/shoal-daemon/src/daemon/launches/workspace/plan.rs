//! What a launch of a new workspace plans once its target is known: the
//! agent it starts, the issue it starts on, and the branch that issue
//! settles, as a start without a terminal to pick them chooses.
use anyhow::{Result, ensure};
use shoal_protocol::launches::Launch;

use super::{ManagerReads, target::Target};
use crate::{
    agent::{Agent, ModelChoice},
    config::Effective,
    daemon::{scope::Caller, workspace::Manager},
    forge::items::{issue::IssueDetails, reference::ItemInput},
    launches::workspace,
    model::Repository,
    start_plan::Creation,
};

/// The agent a launch of a new workspace starts, if any, and the model it
/// runs with.
pub(super) struct PlannedAgent {
    pub agent: Option<Agent>,
    pub model: ModelChoice,
}

impl Manager {
    /// The agent `launch` starts on `target` with `creation`: the one it
    /// names, else for a start on a positional issue `agent.default`, else
    /// none, as no terminal picks one; never one for a PR from a fork,
    /// whose head opens in a workspace at a commit.
    pub(super) async fn planned_agent(
        &self,
        launch: &Launch,
        target: &Target,
        creation: &Creation,
    ) -> Result<PlannedAgent> {
        let settings = self
            .repository_settings_with_templates(&target.repo.id)
            .await?;
        let agent = match (&launch.agent, target.issue_default) {
            (Some(agent), _) => Some(agent.clone()),
            (None, true) => Some(default_agent(&settings)?),
            (None, false) => None,
        };
        if creation.pull_head.is_some() {
            ensure!(
                launch.agent.is_none(),
                "the PR comes from a fork, so its head opens in a workspace at a commit, which holds no agent of its own"
            );
            return Ok(no_agent(launch));
        }
        let Some(agent) = agent else {
            ensure!(
                launch.model.is_empty(),
                "--model and --effort need an agent; name one with --agent"
            );
            return Ok(no_agent(launch));
        };
        crate::agents::ensure_available(&agent, &settings)?;
        let model = crate::agents::resolve_model(&settings, &agent, launch.model.clone());
        crate::agents::check_model(&settings, &agent, &model)?;
        Ok(PlannedAgent {
            agent: Some(agent),
            model: launch.model.clone(),
        })
    }

    /// The open issue `target` starts on, if any.
    pub(super) async fn planned_issue(
        &self,
        target: &Target,
        caller: Option<&Caller>,
    ) -> Result<Option<IssueDetails>> {
        let Some(issue) = &target.issue else {
            return Ok(None);
        };
        let context = self.launch_read_context(&target.repo.id, caller).await?;
        Ok(Some(
            self.issue_details(&context, issue, false, caller).await?,
        ))
    }
}

/// `creation` on the branch `issue` settles in `repo` when nothing else
/// names one: the branch or workspace earlier work on it left, else a new
/// branch named after it.
pub(super) async fn issue_settled(
    reads: &ManagerReads<'_>,
    repo: &Repository,
    creation: Creation,
    issue: Option<&IssueDetails>,
) -> Result<Creation> {
    match issue {
        Some(issue) if creation.unsettled() => {
            let branch = workspace::issue_branch(issue.number.get(), &issue.title);
            workspace::issue_creation(reads, repo, creation, (branch, &issue.url)).await
        }
        _ => Ok(creation),
    }
}

/// The issue `--issue` or the positional argument names, by its URL.
pub(super) fn issue_url(issue: &IssueDetails) -> ItemInput {
    ItemInput::Url(issue.url.clone())
}

/// `agent.default`, without which a start on an issue, which picks an
/// agent on a terminal, is refused.
fn default_agent(settings: &Effective) -> Result<Agent> {
    (settings.agent.default.clone()).ok_or_else(|| {
        anyhow::anyhow!(
            "no agent selected; pass --agent or set agent.default in the repository or global config"
        )
    })
}

/// No agent, with the model `launch` asks for, which none takes.
fn no_agent(launch: &Launch) -> PlannedAgent {
    PlannedAgent {
        agent: None,
        model: launch.model.clone(),
    }
}
