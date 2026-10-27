//! What an addition creates: its branch options and the plan a terminal host carries out.
use std::{ffi::OsString, path::PathBuf};

use anyhow::Result;

use super::{agent::ResolvedAddAgent, target::AddTarget};
use crate::{
    agent::Agent,
    cli::{client::request, commands::issues, context::Context, terminal::PlaceName},
    git,
    model::Workspace,
    protocol::WorkspaceMethod,
};

#[derive(serde::Serialize, serde::Deserialize)]
pub(in crate::cli) struct Creation {
    pub path: Option<PathBuf>,
    pub branch: Option<String>,
    pub existing: Option<String>,
    pub base: Option<String>,
    pub git_profile: Option<String>,
    /// The PR whose head branch is opened; it is linked once the workspace is ready.
    #[serde(default)]
    pub pr: Option<String>,
}

impl Creation {
    /// Build a new branch on `base`, the branch of a done dependency, unless
    /// the addition names its own base or opens an existing branch.
    pub(in crate::cli::commands) fn build_on(&mut self, base: Option<String>) {
        if self.base.is_none() && self.existing.is_none() {
            self.base = base;
        }
    }
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
    Ok(crate::daemon::workspace::derive_workspace_name(&branch))
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
    /// The agent queue entry whose slot this launch uses.
    #[serde(default)]
    pub queue_entry: Option<String>,
    /// The chat that planned the addition, which the workspace belongs to.
    #[serde(default)]
    pub(super) chat: Option<String>,
    /// The existing workspace a launch in it reopens; it never creates one.
    #[serde(default)]
    pub(super) workspace: Option<String>,
}

impl AddPlan {
    /// A launch of `agent` in the existing `workspace`, prompted with the
    /// issue at `issue` or with `args`. It runs detached, so it stays tracked.
    pub(in crate::cli::commands) fn agent_in(
        workspace: &Workspace,
        agent: Agent,
        issue: Option<&issues::Issue>,
        args: Vec<OsString>,
    ) -> Self {
        Self {
            repository: workspace.repository_id.as_str().into(),
            creation: Creation {
                path: None,
                branch: None,
                existing: Some(workspace.branch.clone()),
                base: None,
                git_profile: None,
                pr: None,
            },
            place_name: None,
            issue: issue.map(|issue| issue.url.clone()),
            agent: ResolvedAddAgent {
                agent: Some(agent),
                tracked: true,
            },
            args,
            queue_entry: None,
            chat: None,
            workspace: Some(workspace.id.clone()),
        }
    }

    /// Whether the plan opens `workspace`'s branch in its repository without
    /// creating anything.
    pub(in crate::cli::commands) fn opens(&self, workspace: &Workspace) -> bool {
        self.workspace.as_deref() == Some(workspace.id.as_str())
            && self.repository.value() == workspace.repository_id
            && self.creation.branch.is_none()
            && self.creation.path.is_none()
            && self.creation.existing.as_deref() == Some(workspace.branch.as_str())
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
        self.agent.agent.is_some() && (self.issue.is_some() || !self.args.is_empty())
    }

    pub fn label(&self) -> String {
        let branch = self
            .creation
            .branch
            .as_deref()
            .or(self.creation.existing.as_deref())
            .unwrap_or("shoal add");
        self.place_name
            .as_ref()
            .map_or_else(|| branch.to_owned(), |name| name.render(branch))
    }
}

/// Refuse what creation would refuse in the given repository, branch and
/// options before a prompt asks for the rest of the addition. Only a terminal
/// prompts; without one, creation refuses the arguments itself.
pub(super) async fn check_add_creation(
    ctx: &Context,
    target: &AddTarget,
    creation: &Creation,
) -> Result<()> {
    if !ctx.interactive() {
        return Ok(());
    }
    request::<()>(
        &ctx.paths,
        WorkspaceMethod::CheckCreation {
            repository: target.selector.clone(),
            branch: creation.branch.clone(),
            existing: creation.existing.clone(),
            base: creation.base.clone(),
            git_profile: creation.git_profile.clone(),
            path: creation.path.clone(),
        },
    )
    .await
}

#[cfg(test)]
mod tests;
