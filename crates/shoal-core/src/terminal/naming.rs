//! What a place hosting a workspace is called: its name, or the host's name
//! template filled with it, the repository, the branch and the issue.
use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};

use super::{Host, names_places};
use crate::client::space_names::SpaceSelector;
use crate::{
    model::{Repository, Workspace},
    paths::CommandPaths,
    protocol::ItemContext,
    start_plan::AddPlan,
};

/// The name of the place hosting a workspace, with the repository and issue
/// filled in, rendered once the branch is known.
#[derive(Clone, Serialize, Deserialize)]
pub struct PlaceName {
    template: Option<String>,
    repo: String,
    issue_number: Option<u64>,
    issue_title: Option<String>,
}

impl PlaceName {
    /// The name of a place for a workspace of `repo`, for the `issue` given
    /// by its number and title, if any.
    pub fn new(template: Option<String>, repo: &Repository, issue: Option<(u64, &str)>) -> Self {
        Self {
            template,
            repo: crate::repository::name(repo).to_owned(),
            issue_number: issue.map(|(number, _)| number),
            issue_title: issue.map(|(_, title)| title.to_owned()),
        }
    }

    /// This name under the host's name `template`.
    pub fn templated(&self, template: Option<String>) -> Self {
        Self {
            template,
            ..self.clone()
        }
    }

    /// The label of a place hosting the workspace `name` on `branch`: the
    /// name, or the host's template filled in.
    pub fn render(&self, branch: &str, name: &str) -> String {
        match &self.template {
            Some(template) => self.fill(template, branch, name),
            None => name.to_owned(),
        }
    }

    /// `template` filled in.
    fn fill(&self, template: &str, branch: &str, name: &str) -> String {
        crate::config::templates::render(
            template,
            &[
                ("{repo}", &self.repo),
                ("{branch}", branch),
                ("{name}", name),
                // As templates written before names replaced titles say it.
                ("{title}", name),
                (
                    "{issue_number}",
                    &self.issue_number.map(|n| n.to_string()).unwrap_or_default(),
                ),
                (
                    "{issue_title}",
                    self.issue_title.as_deref().unwrap_or_default(),
                ),
            ],
        )
    }
}

/// Give `plan`'s place the host's name `template`, if any, with `issue`,
/// its issue's number and title. Without `issue`, a plan that names one
/// only by URL, as a queued start's does, looks it up.
pub async fn name_plan(
    paths: &CommandPaths,
    template: Option<String>,
    plan: &mut AddPlan,
    issue: Option<(u64, &str)>,
) -> Result<()> {
    let Some(template) = template else {
        return Ok(());
    };
    if let Some(name) = &mut plan.place_name {
        *name = name.templated(Some(template));
        return Ok(());
    }
    let repos = crate::client::repositories::list(paths).await?;
    let repo = crate::repository::select(&repos, &plan.repository).await?;
    let found = match (issue, plan.issue()) {
        (None, Some(url)) => {
            let context = plan_context(plan, repo);
            Some(
                crate::client::forge_reads::issue_details(paths, context, url.clone().into())
                    .await?,
            )
        }
        _ => None,
    };
    let issue = issue.or(found
        .as_ref()
        .map(|issue| (issue.number.get(), &*issue.title)));
    plan.place_name = Some(PlaceName::new(Some(template), repo, issue));
    Ok(())
}

/// Where `plan`'s issue in `repo` is read: from the workspace it opens or
/// the chat it starts from, else from the checkout.
fn plan_context(plan: &AddPlan, repo: &Repository) -> ItemContext {
    match (&plan.workspace, &plan.chat) {
        (Some(workspace), _) => ItemContext::Workspace(SpaceSelector::id(workspace)),
        (None, Some(chat)) => ItemContext::Chat(SpaceSelector::id(chat)),
        (None, None) => ItemContext::Checkout(repo.id.clone()),
    }
}

/// The label of the place `host` opens for `workspace`: its name, or its
/// name `template` filled in, or the workspace's directory name when the
/// host shows no names and names its places after it. A failed lookup
/// falls back to the name.
pub(super) async fn workspace_label(
    paths: &CommandPaths,
    template: Option<String>,
    host: &dyn Host,
    workspace: &Workspace,
) -> String {
    if !names_places(host) {
        return workspace.directory_name().to_owned();
    }
    if template.is_none() {
        return workspace.name.to_string();
    }
    match place_name(paths, workspace).await {
        Ok(name) => name
            .templated(template)
            .render(workspace.branch_or_name(), &workspace.name),
        Err(error) => {
            eprintln!("warning: cannot name the {} place: {error:#}", host.name());
            workspace.name.to_string()
        }
    }
}

/// The name of the places hosting `workspace`, without a host's template:
/// its repository and issue looked up.
pub async fn place_name(paths: &CommandPaths, workspace: &Workspace) -> Result<PlaceName> {
    let repositories = crate::client::repositories::list(paths).await?;
    let repo = repositories
        .iter()
        .find(|repo| repo.id == workspace.repository_id)
        .context("repository is no longer registered")?;
    let issue = match &workspace.links.issue {
        Some(url) => {
            let context = ItemContext::Workspace(SpaceSelector::id(&workspace.id));
            Some(
                crate::client::forge_reads::issue_details(paths, context, url.clone().into())
                    .await?,
            )
        }
        None => None,
    };
    let issue = issue
        .as_ref()
        .map(|issue| (issue.number.get(), &*issue.title));
    Ok(PlaceName::new(None, repo, issue))
}

#[cfg(test)]
mod tests;
