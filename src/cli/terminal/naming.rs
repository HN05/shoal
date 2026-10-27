//! What the places hosts open for workspaces are called.
use anyhow::Context as _;
use serde::{Deserialize, Serialize};

use crate::{
    cli::{
        client,
        commands::issues::{self, Issue},
        context::Context,
    },
    config::Effective,
    model::{Repository, Workspace},
};

/// The name a host gives the place hosting a new workspace: a template with
/// the repository and issue filled in, rendered once the branch is known.
#[derive(Serialize, Deserialize)]
pub(in crate::cli) struct PlaceName {
    template: String,
    repo: String,
    issue_number: Option<u64>,
    issue_title: Option<String>,
}

impl PlaceName {
    pub(super) fn new(template: String, repo: &Repository, issue: Option<&Issue>) -> Self {
        Self {
            template,
            repo: crate::repository::name(repo).to_owned(),
            issue_number: issue.map(|issue| issue.number),
            issue_title: issue.map(|issue| issue.title.clone()),
        }
    }

    pub fn render(&self, branch: &str) -> String {
        crate::config::templates::render(
            &self.template,
            &[
                ("{repo}", &self.repo),
                ("{branch}", branch),
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

/// The label `shoal add` gives a workspace's tab: the `tab_name` template,
/// `<repo>#<number>` for an issue, or the branch.
pub(super) fn tab_label(
    template: Option<String>,
    repo: &Repository,
    issue: Option<&Issue>,
    branch: &str,
) -> String {
    match template {
        Some(template) => PlaceName::new(template, repo, issue).render(branch),
        None => issue.map_or_else(|| branch.to_owned(), |issue| issue.tab_label(repo)),
    }
}

/// The tab label `shoal add` would give `workspace`. A failed issue lookup
/// falls back to the branch.
pub(super) async fn workspace_label(
    ctx: &Context,
    settings: &Effective,
    workspace: &Workspace,
) -> String {
    let labelled = async {
        let repositories = client::repositories(&ctx.paths).await?;
        let repo = repositories
            .iter()
            .find(|repo| repo.id == workspace.repository_id)
            .context("repository is no longer registered")?;
        let issue = match &workspace.links.issue {
            Some(url) => Some(issues::load(ctx, repo, url).await?),
            None => None,
        };
        anyhow::Ok(tab_label(
            settings.herdr.tab_name.clone(),
            repo,
            issue.as_ref(),
            &workspace.branch,
        ))
    };
    labelled.await.unwrap_or_else(|error| {
        eprintln!("warning: cannot label the Herdr tab: {error:#}");
        workspace.branch.clone()
    })
}

#[cfg(test)]
mod tests;
