//! The names of the places hosting a workspace or a planned one, with the
//! repository and issue looked up from the daemon.
use anyhow::{Context as _, Result};

use crate::{
    client::connection::CommandPaths,
    model::{Space, Workspace},
    protocol::ItemContext,
    start_plan::PlacePlan,
    terminal::{Host, PlaceName, names_places},
};
use shoal_protocol::space_names::SpaceSelector;

/// Give `plan`'s place in `space` the host's name `template`, if any, with
/// the repository and first issue of a workspace; a chat's place takes its
/// name.
pub async fn name_plan(
    paths: &CommandPaths,
    template: Option<String>,
    plan: &mut PlacePlan,
    space: &Space,
) -> Result<()> {
    let Some(template) = template else {
        return Ok(());
    };
    if let Some(name) = &mut plan.place_name {
        *name = name.templated(Some(template));
        return Ok(());
    }
    let Some(workspace) = space.to_workspace() else {
        return Ok(());
    };
    let name = place_name(paths, &workspace).await?;
    plan.place_name = Some(name.templated(Some(template)));
    Ok(())
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
    // A name reads only the issue's number and title, so a workspace whose
    // issue has closed keeps them.
    let issue = match workspace.links.issue() {
        Some(linked) => {
            let context = ItemContext::space(SpaceSelector::id(&workspace.id));
            let input = linked.url.clone().into();
            Some(crate::client::forge_reads::linked_issue_details(paths, context, input).await?)
        }
        None => None,
    };
    let issue = issue
        .as_ref()
        .map(|issue| (issue.number.get(), &*issue.title));
    Ok(PlaceName::new(None, repo, issue))
}
