//! What the places hosts open for workspaces are called: the host's name
//! template with the repository, branch, issue and title filled in,
//! otherwise the title the workspace's agent gave it, otherwise
//! `<repo>#<number>` for an issue, otherwise the branch. Hosts that show
//! names apply the label they are given.
use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};

use super::{Configured, Host};
use crate::{
    cli::{
        client,
        commands::{
            issues::{self, Issue},
            workspaces::AddPlan,
        },
        context::Context,
    },
    model::{OwnedPlace, Repository, Workspace},
    protocol::{ConfigTarget, WorkspaceMethod},
    sessions::Origin,
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

    /// The label of a place hosting the workspace on `branch` with `title`.
    pub fn render(&self, branch: &str, title: Option<&str>) -> String {
        match &self.template {
            Some(template) => self.fill(template, branch, title),
            None => title.map_or_else(|| self.untitled(branch), str::to_owned),
        }
    }

    /// The label without a title or template: the issue's, else the branch.
    fn untitled(&self, branch: &str) -> String {
        match self.issue_number {
            Some(number) => issue_label(&self.repo, number),
            None => branch.to_owned(),
        }
    }

    /// `template` filled in; `{title}` falls back to the untitled label.
    fn fill(&self, template: &str, branch: &str, title: Option<&str>) -> String {
        let title = title.map_or_else(|| self.untitled(branch), str::to_owned);
        crate::config::templates::render(
            template,
            &[
                ("{repo}", &self.repo),
                ("{branch}", branch),
                ("{title}", &title),
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

/// Give `plan`'s place the host's name `template`, if any. Without `issue`,
/// a plan that names one only by URL looks it up.
pub(super) async fn name_plan(
    ctx: &Context,
    template: Option<String>,
    plan: &mut AddPlan,
    issue: Option<&Issue>,
) -> Result<()> {
    let Some(template) = template else {
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

/// The label of the place `host` opens for `workspace`, from its name
/// `template`, or the workspace's name when the host shows no names and
/// names its places after it. A failed lookup falls back to the branch.
pub(super) async fn workspace_label(
    ctx: &Context,
    template: Option<String>,
    host: &dyn Host,
    workspace: &Workspace,
) -> String {
    if !super::names_places(host) {
        return workspace.name.clone();
    }
    if let (None, Some(title)) = (&template, &workspace.title) {
        return title.clone();
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
        let name = PlaceName::new(template, repo, issue.as_ref());
        anyhow::Ok(name.render(&workspace.branch, workspace.title.as_deref()))
    };
    labelled.await.unwrap_or_else(|error| {
        eprintln!("warning: cannot name the {} place: {error:#}", host.name());
        workspace.branch.clone()
    })
}

/// Show `workspace`'s label on the places Shoal created for it, as after its
/// title changed. Only a host this process runs under reaches its places,
/// and only those on its own server; a place that is gone is skipped.
pub(in crate::cli) async fn relabel_places(ctx: &Context, workspace: &Workspace) -> Result<()> {
    let places: Vec<OwnedPlace> = client::request(
        &ctx.paths,
        WorkspaceMethod::WorkspacePlaces {
            workspace: workspace.id.clone(),
        },
    )
    .await?;
    let settings =
        client::settings(&ctx.paths, ConfigTarget::Workspace(workspace.id.clone())).await?;
    for configured in Configured::all(&settings).filter(|host| host.options.enabled) {
        let Some(host_places) = configured.host.places().filter(|p| p.names_places()) else {
            continue;
        };
        let Some(here) = host_places.origin() else {
            continue;
        };
        let reachable = reachable_from(&here, &places);
        if reachable.is_empty() {
            continue;
        }
        let template = configured.options.name_template.clone();
        let label = workspace_label(ctx, template, configured.host, workspace).await;
        for place in reachable {
            host_places.rename_place(place.id(), &label).await;
        }
    }
    Ok(())
}

/// The places of `places` on the host and server of `here`.
fn reachable_from<'a>(here: &Origin, places: &'a [OwnedPlace]) -> Vec<&'a Origin> {
    places
        .iter()
        .map(|owned| &owned.origin)
        .filter(|place| place.host == here.host && place.server() == here.server())
        .collect()
}

#[cfg(test)]
mod tests;
