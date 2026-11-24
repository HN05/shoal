//! Starting, reading, rerunning and cancelling CI workflow runs of the
//! workspace's repository as its agent account, or of another repository as
//! the user.
pub mod place;
pub mod reach;

use std::{collections::BTreeMap, sync::Arc};

use anyhow::{Context, Result, bail, ensure};
use shoal_daemon_base::callers::Caller;
use shoal_forge::{
    capability::CapabilityRequests,
    ci::{
        checks::CheckRequests,
        run::{NewRun, RunRequests},
        run_action::RunActionRequests,
        run_view::{RunViewRequests, validate_filter},
    },
};
use shoal_forge_types::{
    ForgeRepo,
    capability::Capability,
    ci::{
        CiAction,
        checks::CheckLog,
        run::StartedRun,
        run_action::{Run, RunActed},
        run_view::{JobRef, RunFilter, RunJob},
    },
    items::{
        link::{ItemKind, LinkKind},
        reference::{ItemInput, ItemRef},
    },
};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::{
    model::{LinkedItem, Relation},
    protocol::RepoTarget,
};

use crate::{
    Links,
    daemon::Daemon,
    forges,
    place::{Access, Place},
    space::LinkSpace,
};

impl Links {
    /// Start `workflow`, named by its file or ID or by its web URL, on
    /// `branch`, by default the one the URL names, and link the run as
    /// opened in the workspace where the forge reports it, so watches follow
    /// it. Scoped processes start runs only on the branches their scope
    /// reaches.
    pub async fn start_ci_run(
        self: &Arc<Self>,
        daemon: &dyn Daemon,
        target: &RepoTarget,
        workflow: Option<String>,
        branch: Option<String>,
        inputs: BTreeMap<String, String>,
        caller: Option<&Caller>,
    ) -> Result<StartedRun> {
        let placed = self.workflow_place(daemon, target, workflow, caller);
        let (place, workflow, linked) = placed.await?;
        let branch = place.run_branch(branch.or(linked)).await?;
        if let Some(caller) = caller {
            (self.ensure_run_branch_in_reach(daemon, caller, &place, Some(&branch))).await?;
        }
        let run = NewRun {
            workflow,
            branch,
            inputs,
        };
        if let Some(id) = place.space_id() {
            // Refuse before the run starts what would refuse its link after.
            self.ensure_links_accepted(id).await?;
        }
        let mut started = (place.forge().start_run(&place.account, &run).await)
            .with_context(|| format!("could not start a CI run on {}", run.branch))?;
        if let Some(id) = place.space_id() {
            (self.link_started(id, place.forge(), &mut started)).await;
        }
        Ok(started)
    }

    /// Refuse a workspace that cannot record links, as one being removed.
    async fn ensure_links_accepted(&self, id: &SpaceId) -> Result<()> {
        let (id, records) = (id.to_owned(), self.records.clone());
        self.store
            .read(move |db| records.ensure_accepts_links(db, &id))
            .await
    }

    /// Link the started run where Shoal reads it. The run has started
    /// whatever happens here, so a failure to link it is a warning, which
    /// keeps a retry from starting another run.
    async fn link_started(
        self: &Arc<Self>,
        id: &SpaceId,
        forge: &ForgeRepo,
        started: &mut StartedRun,
    ) {
        let Some(item) = started_link(forge, started) else {
            return;
        };
        let url = item.url.clone();
        match self.record_items(id, vec![(Relation::Opened, item)]).await {
            Ok(()) => started.linked = true,
            Err(error) => started.warnings.push(format!(
                "could not link {url}: {error:#}; link it with shoal link ci {url} opened"
            )),
        }
    }

    /// Rerun or cancel the run `input` names in the `target` repository, or
    /// that of its URL; scoped processes act only on the runs of the
    /// branches their scope reaches and of PRs from them.
    pub async fn ci_run_action(
        &self,
        daemon: &dyn Daemon,
        target: &RepoTarget,
        input: ItemInput,
        action: CiAction,
        failed: bool,
        caller: Option<&Caller>,
    ) -> Result<RunActed> {
        let named = self.named_run(daemon, target, &input, Access::Change, caller);
        let (place, run) = named.await?;
        let (forge, account) = (place.forge(), &place.account);
        let run = forge
            .find_run(account, &run, action)
            .await
            .with_context(|| format!("could not {action} CI run {run}"))?;
        if let Some(caller) = caller {
            let branch = forge.run_branch(account, &run).await?;
            let reach = self.ensure_run_branch_in_reach(daemon, caller, &place, branch.as_deref());
            reach.await?;
        }
        forge
            .act_on_run(account, &run, action, failed)
            .await
            .with_context(|| format!("could not {action} {}", run.url))
    }

    /// The run `input` names.
    pub async fn view_ci_run(
        &self,
        daemon: &dyn Daemon,
        target: &RepoTarget,
        input: ItemInput,
        caller: Option<&Caller>,
    ) -> Result<Run> {
        (self.read_run(daemon, target, &input, Access::Read, caller)).await
    }

    /// The run `run` that a launch from `target` starts on, read as
    /// `caller` may link it, so a scoped caller's run of another
    /// repository is refused as its link would be.
    pub async fn launch_run(
        &self,
        daemon: &dyn Daemon,
        target: &RepoTarget,
        run: &ItemRef,
        caller: Option<&Caller>,
    ) -> Result<Run> {
        let input = ItemInput::Url(run.clone());
        (self.read_run(daemon, target, &input, Access::Change, caller)).await
    }

    /// The run `input` names, read where a request with `access` acts on
    /// it.
    async fn read_run(
        &self,
        daemon: &dyn Daemon,
        target: &RepoTarget,
        input: &ItemInput,
        access: Access,
        caller: Option<&Caller>,
    ) -> Result<Run> {
        let named = self.named_run(daemon, target, input, access, caller);
        let (place, run) = named.await?;
        place
            .forge()
            .view_run(&place.account, &run)
            .await
            .with_context(|| format!("could not read CI run {run}"))
    }

    /// Every job of the run `input` names.
    pub async fn ci_run_jobs(
        &self,
        daemon: &dyn Daemon,
        target: &RepoTarget,
        input: ItemInput,
        caller: Option<&Caller>,
    ) -> Result<Vec<RunJob>> {
        let named = self.named_run(daemon, target, &input, Access::Read, caller);
        let (place, run) = named.await?;
        place
            .forge()
            .run_jobs(&place.account, &run)
            .await
            .with_context(|| format!("could not read the jobs of CI run {run}"))
    }

    /// The log of the job `job` of the run `input` names.
    pub async fn ci_run_job_log(
        &self,
        daemon: &dyn Daemon,
        target: &RepoTarget,
        input: ItemInput,
        job: JobRef,
        caller: Option<&Caller>,
    ) -> Result<CheckLog> {
        let named = self.named_run(daemon, target, &input, Access::Read, caller);
        let (place, run) = named.await?;
        place
            .forge()
            .run_job_log(&place.account, &run, &job)
            .await
            .with_context(|| format!("could not read a job log of CI run {run}"))
    }

    /// The newest runs `filter` selects on the head commit of the PR `pr`
    /// names, otherwise on the branch it names, by default the workspace's,
    /// a chat's linked PR's head, or else the repository's default branch.
    pub async fn list_ci_runs(
        &self,
        daemon: &dyn Daemon,
        target: &RepoTarget,
        pr: Option<ItemInput>,
        mut filter: RunFilter,
        caller: Option<&Caller>,
    ) -> Result<Vec<Run>> {
        validate_filter(&filter)?;
        ensure!(
            pr.is_none() || filter.branch.is_none(),
            "list the runs of a branch or of a PR, not both"
        );
        let named = match &pr {
            Some(pr) => forges::of_input(&self.forges, pr).await?,
            None => None,
        };
        let place = (self.run_place(daemon, target, named, Access::Read, caller)).await?;
        let (forge, account) = (place.forge(), &place.account);
        // Refuse a forge without runs before reading the PR.
        forge.require(Capability::ReadRuns)?;
        // A chat's checkout is not on its PR's branch, so its runs are
        // those of its linked PR.
        let chat = (place.target.own_place())
            .and_then(Place::space)
            .filter(|space| space.worktree().is_none());
        let pr = match (pr, &filter.branch, chat) {
            (None, None, Some(chat)) => self.linked_pr(&chat.id).await?,
            (Some(pr), ..) => Some(ItemRef::in_repository(forge, LinkKind::Pr, &pr)?),
            (None, ..) => None,
        };
        match pr {
            Some(pull) => {
                let head = forge
                    .pull_head(account, &pull)
                    .await
                    .with_context(|| format!("could not read the head of {pull}"))?;
                filter.commit = Some(head);
            }
            None => filter.branch = Some(place.run_branch(filter.branch.take()).await?),
        }
        forge
            .list_runs(account, &filter)
            .await
            .context("could not list CI runs")
    }

    /// The one PR linked to the space `id`, if any.
    async fn linked_pr(&self, id: &SpaceId) -> Result<Option<ItemRef>> {
        let id = id.to_owned();
        let prs = (self.store)
            .read(move |db| crate::linked::assigned_items(db, &id, Some(ItemKind::Pr)))
            .await?;
        match prs.as_slice() {
            [] => Ok(None),
            [pull] => Ok(Some(pull.clone())),
            _ => bail!("several PRs are linked; pass --pr or --branch"),
        }
    }

    /// The CI run `input` names, by number in the repository `space` acts
    /// in or by URL, as `caller` may link it, with its workflow as its
    /// title. A watch reads the run as the user where it is not the
    /// space's own, so linking it is a change there.
    pub(crate) async fn linked_run(
        &self,
        daemon: &dyn Daemon,
        space: &LinkSpace,
        input: &ItemInput,
        caller: Option<&Caller>,
    ) -> Result<LinkedItem> {
        let target = space.item_context().into();
        let linked = self.named_run(daemon, &target, input, Access::Change, caller);
        let (place, url) = linked.await?;
        let run = (place.forge().view_run(&place.account, &url).await)
            .with_context(|| format!("could not read CI run {url}"))?;
        Ok(LinkedItem {
            url,
            title: run.workflow,
        })
    }
}

/// The link a started run gets, where the forge reports its URL and Shoal
/// reads its runs, so a watch can follow it; a run Shoal cannot read, such
/// as a Bitbucket pipeline, started all the same.
fn started_link(forge: &ForgeRepo, started: &StartedRun) -> Option<LinkedItem> {
    if !forge.supports(Capability::ReadRuns) {
        return None;
    }
    let url = ItemRef::parse_kind(LinkKind::Ci, started.url.as_deref()?).ok()?;
    Some(LinkedItem {
        url,
        title: started.workflow.clone(),
    })
}

#[cfg(test)]
mod tests;
