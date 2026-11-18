//! The names of the places hosting a workspace or a planned one, with the
//! repository and issue looked up from the daemon.
use anyhow::{Context as _, Result};

use crate::{
    client::connection::CommandPaths,
    model::{Repository, Workspace},
    protocol::ItemContext,
    start_plan::AddPlan,
    terminal::{Host, PlaceName, names_places},
};
use shoal_protocol::space_names::SpaceSelector;

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
        // A name reads only the issue's number and title, so a workspace
        // whose issue has closed keeps them.
        (None, Some(url)) => {
            let context = plan_context(plan, repo);
            let item = url.clone().into();
            Some(crate::client::forge_reads::linked_issue_details(paths, context, item).await?)
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
    match plan.workspace.as_ref().or(plan.chat.as_ref()) {
        Some(space) => ItemContext::space(SpaceSelector::id(space)),
        None => ItemContext::Checkout(repo.id.clone()),
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
/// its repository and first issue looked up.
pub async fn place_name(paths: &CommandPaths, workspace: &Workspace) -> Result<PlaceName> {
    let repositories = crate::client::repositories::list(paths).await?;
    let repo = repositories
        .iter()
        .find(|repo| repo.id == *workspace.repository())
        .context("repository is no longer registered")?;
    let issue = match workspace.links.issue() {
        Some(linked) => {
            let context = ItemContext::space(SpaceSelector::id(&workspace.id));
            let input = linked.url.clone().into();
            Some(crate::client::forge_reads::issue_details(paths, context, input).await?)
        }
        None => None,
    };
    let issue = issue
        .as_ref()
        .map(|issue| (issue.number.get(), &*issue.title));
    Ok(PlaceName::new(None, repo, issue))
}
