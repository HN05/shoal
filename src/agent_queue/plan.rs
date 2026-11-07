//! The plan of a queued agent start, which the queue keeps as JSON in
//! [`QueueRequest::plan`] until the `queue-start` worker carries it out.
//! Entries queued by an earlier version still start, so the format only
//! gains fields with defaults.
use std::{ffi::OsString, path::PathBuf};

use anyhow::Result;

use crate::client::space_names::SpaceSelector;
use crate::{
    agent::{Agent, ModelChoice},
    model::{QueueRequest, Workspace},
    protocol::{CreationOptions, PullHead, operations::QueuedLaunch},
    terminal::PlaceName,
};

#[derive(Default, serde::Serialize, serde::Deserialize)]
pub(crate) struct Creation {
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
    /// The workspace the new one copies, on `branch` or a branch named
    /// after the copied one's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub copy: Option<CopyCreation>,
}

/// The workspace a new one copies, and the name the copy takes without a
/// branch of its own, which the daemon suffixes when it is taken.
#[derive(serde::Serialize, serde::Deserialize)]
pub(crate) struct CopyCreation {
    pub source: crate::protocol::CopySource,
    pub name: String,
}

impl Creation {
    /// Build a new branch on `base`, the branch of a done dependency, unless
    /// the addition names its own base, opens an existing branch or copies a
    /// workspace, which starts from that workspace instead.
    pub fn build_on(&mut self, base: Option<String>) {
        if self.base.is_none() && self.existing.is_none() && self.copy.is_none() {
            self.base = base;
        }
    }
}

/// The agent of a plan, which queued entries store; entries queued before
/// every launch ran tracked also carry `tracked` or `codex_mode`, which are
/// ignored.
#[derive(serde::Serialize, serde::Deserialize)]
pub(crate) struct ResolvedAddAgent {
    pub agent: Option<Agent>,
    /// The model and effort requested over the configured ones.
    #[serde(default)]
    pub model: ModelChoice,
}

/// The queue request of `launch` in `workspace`, which the agent queue
/// starts detached, beside the caller's terminal when the launch names it
/// or else in the default host.
pub(crate) fn queued_agent_request(
    workspace: &Workspace,
    mut launch: QueuedLaunch,
) -> Result<QueueRequest> {
    let env =
        (launch.env.take()).unwrap_or_else(|| crate::terminal::detached_start_environment(None));
    let issue_title = launch.issue.as_ref().map(|issue| issue.title.clone());
    let (agent, now) = (launch.agent.clone(), launch.now);
    let plan = AddPlan::agent_in(workspace, launch)?;
    Ok(QueueRequest {
        repository: workspace.repository_id.as_str().into(),
        name: workspace.name.clone(),
        agent,
        mode: plan.mode.clone(),
        issue_title,
        plan: serde_json::to_string(&plan)?,
        env,
        now,
        workspace: Some(SpaceSelector::id(&workspace.id)),
        after: Vec::new(),
    })
}

/// What an addition creates and the agent it starts there, which a terminal
/// host or the agent queue's worker carries out.
#[derive(serde::Serialize, serde::Deserialize)]
pub(crate) struct AddPlan {
    pub repository: crate::repository::Selector,
    pub creation: Creation,
    /// The name of the place a host opens for the addition.
    #[serde(default)]
    pub place_name: Option<PlaceName>,
    /// The found issue's URL; its body can exceed what a Herdr tab's
    /// environment carries, so the worker looks it up again.
    pub issue: Option<String>,
    pub agent: ResolvedAddAgent,
    pub args: Vec<OsString>,
    /// The agent's subject when no issue gives it one, which a launcher
    /// with `{prompt}` receives there.
    #[serde(default)]
    pub prompt: Option<String>,
    /// The mode the agent starts in; the default mode when `None`.
    #[serde(default)]
    pub mode: Option<String>,
    /// The session the agent starts from a fork of.
    #[serde(default)]
    pub fork: Option<String>,
    /// The agent queue entry whose slot this launch uses.
    #[serde(default)]
    pub queue_entry: Option<String>,
    /// The chat that planned the addition, which the workspace belongs to
    /// unless `parent` names another space.
    #[serde(default)]
    pub chat: Option<String>,
    /// The space the workspace goes below, which `--parent` names.
    #[serde(default)]
    pub parent: Option<String>,
    /// The existing workspace a launch in it reopens; it never creates one.
    #[serde(default)]
    pub workspace: Option<String>,
}

impl AddPlan {
    /// `launch` in the existing `workspace`. It runs detached, so it stays
    /// tracked.
    pub fn agent_in(workspace: &Workspace, launch: QueuedLaunch) -> Result<Self> {
        let QueuedLaunch {
            agent,
            prompt,
            mode,
            model,
            args,
            fork,
            issue,
            env: _,
            now: _,
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

    /// Whether the plan opens `workspace`'s branch in its repository without
    /// creating anything.
    pub fn opens(&self, workspace: &Workspace) -> bool {
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

#[cfg(test)]
mod tests;
