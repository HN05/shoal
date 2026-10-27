//! What the places hosts open for workspaces are called: the host's name
//! template with the repository, branch and issue filled in, otherwise
//! `<repo>#<number>` for an issue, otherwise the branch. Hosts that show
//! names apply the label they are given.
use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};

use super::Host;
use crate::{
    cli::{
        client,
        commands::{
            issues::{self, Issue},
            workspaces::AddPlan,
        },
        context::Context,
    },
    config::Effective,
    model::{Repository, Workspace},
};

/// The name of the place hosting a workspace, with the repository and issue
/// filled in, rendered once the branch is known.
#[derive(Serialize, Deserialize)]
pub(in crate::cli) struct PlaceName {
    template: Option<String>,
    repo: String,
    issue_number: Option<u64>,
    issue_title: Option<String>,
}

impl PlaceName {
    pub(in crate::cli) fn new(
        template: Option<String>,
        repo: &Repository,
        issue: Option<&Issue>,
    ) -> Self {
        Self {
            template,
            repo: crate::repository::name(repo).to_owned(),
            issue_number: issue.map(|issue| issue.number),
            issue_title: issue.map(|issue| issue.title.clone()),
        }
    }

    pub fn render(&self, branch: &str) -> String {
        match (&self.template, self.issue_number) {
            (Some(template), _) => self.fill(template, branch),
            (None, Some(number)) => issue_label(&self.repo, number),
            (None, None) => branch.to_owned(),
        }
    }

    fn fill(&self, template: &str, branch: &str) -> String {
        crate::config::templates::render(
            template,
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

/// `<repo>#<number>`: short, and unique across repositories sharing a host.
pub(in crate::cli) fn issue_label(repo: &str, number: u64) -> String {
    format!("{repo}#{number}")
}

/// Give `plan`'s place the name template `host` takes from `settings`, if
/// any. Without `issue`, a plan that names one only by URL looks it up.
pub(super) async fn name_plan(
    ctx: &Context,
    settings: &Effective,
    host: &dyn Host,
    plan: &mut AddPlan,
    issue: Option<&Issue>,
) -> Result<()> {
    let Some(template) = host.name_template(settings) else {
        return Ok(());
    };
    if let Some(name) = &mut plan.place_name {
        name.template = Some(template);
        return Ok(());
    }
    let repos = client::repositories(&ctx.paths).await?;
    let repo = crate::repository::select(&repos, &plan.repository).await?;
    // A queued start has only the issue's URL; the template may name it.
    let found = match (issue, plan.issue_url()) {
        (None, Some(url)) => Some(issues::load(ctx, repo, url).await?),
        _ => None,
    };
    plan.place_name = Some(PlaceName::new(
        Some(template),
        repo,
        issue.or(found.as_ref()),
    ));
    Ok(())
}

/// The label of the place `host` opens for `workspace` under `settings`,
/// or its branch when the host shows no names. A failed lookup falls back
/// to the branch.
pub(super) async fn workspace_label(
    ctx: &Context,
    settings: &Effective,
    host: &dyn Host,
    workspace: &Workspace,
) -> String {
    if !host.names_places() {
        return workspace.branch.clone();
    }
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
        let name = PlaceName::new(host.name_template(settings), repo, issue.as_ref());
        anyhow::Ok(name.render(&workspace.branch))
    };
    labelled.await.unwrap_or_else(|error| {
        eprintln!("warning: cannot name the {} place: {error:#}", host.name());
        workspace.branch.clone()
    })
}

#[cfg(test)]
mod tests;
