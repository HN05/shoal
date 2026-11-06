//! What the places hosts open for spaces are called. A workspace's take the
//! host's name template with the repository, branch, issue and title filled
//! in, otherwise the title the workspace's agent gave it, otherwise
//! `<repo>#<number>` for an issue, otherwise the branch; a chat's take its
//! name. Hosts that show names apply the label they are given, and a place
//! takes a space's new label only while it still shows the one Shoal gave
//! it, since another is the user's.
use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};
use tokio::sync::OnceCell;

use crate::{
    cli::{
        commands::{
            issues::{self, Issue},
            workspaces::AddPlan,
        },
        context::Context,
    },
    config::Effective,
    model::{Repository, Workspace},
    protocol::ItemContext,
    sessions::{HostName, Origin},
    terminal::{Configured, Host, Options, Places, names_places},
};

/// The name of the place hosting a workspace, with the repository and issue
/// filled in, rendered once the branch is known.
#[derive(Clone, Serialize, Deserialize)]
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

    /// This name under the host's name `template`.
    fn templated(&self, template: Option<String>) -> Self {
        Self {
            template,
            ..self.clone()
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
    let repos = crate::client::repositories::list(&ctx.paths).await?;
    let repo = crate::repository::select(&repos, &plan.repository).await?;
    // A queued start has only the issue's URL; the template may name it.
    let found = match (issue, plan.issue_url()) {
        (None, Some(url)) => {
            Some(issues::load(ctx, plan.item_context(ctx, repo).await?, url).await?)
        }
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
    if !names_places(host) {
        return workspace.name.clone();
    }
    if let (None, Some(title)) = (&template, &workspace.title) {
        return title.clone();
    }
    match place_name(ctx, workspace).await {
        Ok(name) => name
            .templated(template)
            .render(workspace.branch_or_name(), workspace.title.as_deref()),
        Err(error) => {
            eprintln!("warning: cannot name the {} place: {error:#}", host.name());
            workspace.branch_or_name().to_owned()
        }
    }
}

/// The name of the places hosting `workspace`, without a host's template:
/// its repository and issue looked up.
async fn place_name(ctx: &Context, workspace: &Workspace) -> Result<PlaceName> {
    let repositories = crate::client::repositories::list(&ctx.paths).await?;
    let repo = repositories
        .iter()
        .find(|repo| repo.id == workspace.repository_id)
        .context("repository is no longer registered")?;
    let issue = match &workspace.links.issue {
        Some(url) => {
            let context = ItemContext::Workspace(workspace.id.clone());
            Some(issues::load(ctx, context, url).await?)
        }
        None => None,
    };
    Ok(PlaceName::new(None, repo, issue.as_ref()))
}

/// What a space's places on one host show: `new` from now on, in place of
/// `expected`, the label Shoal gave them before.
#[derive(Clone)]
pub(in crate::cli) struct Labels {
    pub new: String,
    pub expected: String,
}

/// The labels of the places hosting `workspace` after its title changed
/// from `previous`, from one lookup of its repository and issue, made when
/// a host first needs it.
pub(in crate::cli) struct Retitled<'a> {
    ctx: &'a Context,
    workspace: &'a Workspace,
    previous: Option<&'a str>,
    name: OnceCell<Option<PlaceName>>,
}

impl<'a> Retitled<'a> {
    pub(in crate::cli) fn new(
        ctx: &'a Context,
        workspace: &'a Workspace,
        previous: Option<&'a str>,
    ) -> Self {
        Self {
            ctx,
            workspace,
            previous,
            name: OnceCell::new(),
        }
    }

    /// The labels of the workspace's places on a host with `options`,
    /// rendered for its title and its previous one. Without the lookup both
    /// are the branch, so the places keep their labels.
    pub(in crate::cli) async fn labels(&self, options: &Options) -> Labels {
        let branch = self.workspace.branch_or_name();
        let Some(name) = self.name().await else {
            return Labels {
                new: branch.to_owned(),
                expected: branch.to_owned(),
            };
        };
        let name = name.templated(options.name_template.clone());
        Labels {
            new: name.render(branch, self.workspace.title.as_deref()),
            expected: name.render(branch, self.previous),
        }
    }

    /// The workspace's place name, looked up once; a failed lookup warns.
    async fn name(&self) -> Option<&PlaceName> {
        let looked_up = self.name.get_or_init(async || {
            place_name(self.ctx, self.workspace)
                .await
                .inspect_err(|error| {
                    eprintln!(
                        "warning: cannot name the places of {}: {error:#}",
                        self.workspace.name
                    );
                })
                .ok()
        });
        looked_up.await.as_ref()
    }
}

/// Show a space's new label on `places`, the places Shoal created for it,
/// through the hosts `settings` enable that show names, on the server each
/// place records. `labels` gives each such host's labels; a place takes the
/// new one only while it still shows the expected one, and a host whose
/// labels match is skipped. A place that fails warns, naming its host.
pub(in crate::cli) async fn relabel_space(
    settings: &Effective,
    places: &[Origin],
    labels: impl AsyncFn(&Options) -> Labels,
) {
    for configured in Configured::all(settings).filter(|host| host.options.enabled) {
        let Some(host_places) = configured.host.places().filter(|p| p.names_places()) else {
            continue;
        };
        let name = configured.host.name();
        let hosted: Vec<&Origin> = places.iter().filter(|place| place.host == name).collect();
        if hosted.is_empty() {
            continue;
        }
        let labels = labels(&configured.options).await;
        if labels.new != labels.expected {
            relabel_each(name, host_places, &hosted, &labels).await;
        }
    }
}

/// Show `labels`' new label on each of `hosted`, places of `host`, while
/// it shows the expected one.
async fn relabel_each(host: HostName, places: &dyn Places, hosted: &[&Origin], labels: &Labels) {
    for place in hosted {
        let renamed = places
            .rename_place(place, Some(&labels.expected), &labels.new)
            .await;
        if let Err(error) = renamed {
            eprintln!(
                "warning: cannot relabel the {host} place {}: {error:#}",
                place.id()
            );
        }
    }
}

#[cfg(test)]
mod tests;
