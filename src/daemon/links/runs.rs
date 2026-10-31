//! Starting, reading, rerunning and cancelling CI workflow runs of the
//! workspace's repository as its agent account, or of another repository as
//! the user.
mod place;

use std::collections::BTreeMap;

use anyhow::{Context, Result, ensure};

use crate::{
    daemon::workspace::Manager,
    forge::{
        ForgeRepo,
        account::Account,
        capability::Capability,
        ci::{
            CiAction,
            checks::CheckLog,
            run::{NewRun, StartedRun},
            run_action::{Run, RunActed, validate_run_reference},
            run_view::{JobRef, RunFilter, RunJob, validate_filter},
        },
        items::link::LinkKind,
    },
    model::{LinkedItem, Relation, Workspace},
    protocol::ItemContext,
};

use self::place::workspace_context;

impl Manager {
    /// Start `workflow`, named by its file or ID or by its web URL, on
    /// `branch`, by default the one the URL names, and link the run as
    /// opened in the workspace where the forge reports it, so watches follow
    /// it. Workspace processes start runs only on their workspace's branch.
    pub async fn start_ci_run(
        &self,
        selector: Option<&str>,
        workflow: Option<String>,
        branch: Option<String>,
        inputs: BTreeMap<String, String>,
        scoped: bool,
    ) -> Result<StartedRun> {
        let (place, workflow, linked) = self
            .workflow_place(workspace_context(selector).as_ref(), workflow, scoped)
            .await?;
        let branch = place.run_branch(branch.or(linked)).await?;
        if scoped {
            on_own_branch_name(place.own_workspace(), &branch)?;
        }
        let run = NewRun {
            workflow,
            branch,
            inputs,
        };
        if let Some(workspace) = &place.workspace {
            // Refuse before the run starts what would refuse its link after.
            self.ensure_links_accepted(&workspace.id).await?;
        }
        let mut started = (place.forge.start_run(&place.account, &run).await)
            .with_context(|| format!("could not start a CI run on {}", run.branch))?;
        if let Some(workspace) = &place.workspace {
            (self.link_started(&workspace.id, &place.forge, &mut started)).await;
        }
        Ok(started)
    }

    /// Refuse a workspace that cannot record links, as one being removed.
    async fn ensure_links_accepted(&self, id: &str) -> Result<()> {
        let id = id.to_owned();
        self.store
            .run(move |db| super::ensure_accepts_links(db, &id))
            .await
    }

    /// Link the started run where Shoal reads it. The run has started
    /// whatever happens here, so a failure to link it is a warning, which
    /// keeps a retry from starting another run.
    async fn link_started(&self, id: &str, forge: &ForgeRepo, started: &mut StartedRun) {
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

    /// Rerun or cancel the run `input` names in the repository of `context`,
    /// or of its URL; workspace processes act only on runs of their
    /// workspace's branch.
    pub async fn ci_run_action(
        &self,
        context: Option<&ItemContext>,
        input: String,
        action: CiAction,
        failed: bool,
        scoped: bool,
    ) -> Result<RunActed> {
        validate_run_reference(&input)?;
        let place = self.run_input_place(context, &input, scoped).await?;
        let (forge, account) = (&place.forge, &place.account);
        let run = forge
            .find_run(account, &input, action)
            .await
            .with_context(|| format!("could not {action} CI run {input}"))?;
        // Scope keeps workspace processes in their workspace.
        if scoped {
            on_own_branch(place.own_workspace(), &run)?;
        }
        forge
            .act_on_run(account, &run, action, failed)
            .await
            .with_context(|| format!("could not {action} {}", run.url))
    }

    /// The run `input` names.
    pub async fn view_ci_run(
        &self,
        selector: Option<&str>,
        input: String,
        scoped: bool,
    ) -> Result<Run> {
        validate_run_reference(&input)?;
        let place = self
            .run_input_place(workspace_context(selector).as_ref(), &input, scoped)
            .await?;
        place
            .forge
            .view_run(&place.account, &input)
            .await
            .with_context(|| format!("could not read CI run {input}"))
    }

    /// Every job of the run `input` names.
    pub async fn ci_run_jobs(
        &self,
        selector: Option<&str>,
        input: String,
        scoped: bool,
    ) -> Result<Vec<RunJob>> {
        validate_run_reference(&input)?;
        let place = self
            .run_input_place(workspace_context(selector).as_ref(), &input, scoped)
            .await?;
        place
            .forge
            .run_jobs(&place.account, &input)
            .await
            .with_context(|| format!("could not read the jobs of CI run {input}"))
    }

    /// The log of the job `job` of the run `input` names.
    pub async fn ci_run_job_log(
        &self,
        selector: Option<&str>,
        input: String,
        job: JobRef,
        scoped: bool,
    ) -> Result<CheckLog> {
        validate_run_reference(&input)?;
        let place = self
            .run_input_place(workspace_context(selector).as_ref(), &input, scoped)
            .await?;
        place
            .forge
            .run_job_log(&place.account, &input, &job)
            .await
            .with_context(|| format!("could not read a job log of CI run {input}"))
    }

    /// The newest runs `filter` selects on the head commit of the PR `pr`
    /// names, otherwise on the workspace's branch unless it names another.
    pub async fn list_ci_runs(
        &self,
        selector: &str,
        pr: Option<String>,
        mut filter: RunFilter,
    ) -> Result<Vec<Run>> {
        validate_filter(&filter)?;
        ensure!(
            pr.is_none() || filter.branch.is_none(),
            "list the runs of a branch or of a PR, not both"
        );
        let (workspace, forge, account) = self.run_forge(selector).await?;
        // Refuse a forge without runs before reading the PR.
        forge.require(Capability::ReadRuns)?;
        match pr {
            Some(input) => {
                let (_, url) = forge.pull(&input)?;
                let head = forge
                    .pull_head(&account, &url)
                    .await
                    .with_context(|| format!("could not read the head of {url}"))?;
                filter.commit = Some(head);
            }
            None => {
                filter.branch.get_or_insert(workspace.branch);
            }
        }
        forge
            .list_runs(&account, &filter)
            .await
            .context("could not list CI runs")
    }

    /// The verified workspace `selector` names, its forge and its agent
    /// account.
    pub(super) async fn run_forge(
        &self,
        selector: &str,
    ) -> Result<(Workspace, ForgeRepo, Account)> {
        let workspace = self.workspace(selector).await?;
        self.verify_worktree(&workspace).await?;
        let (forge, account) = self.workspace_forge_account(&workspace).await?;
        Ok((workspace, forge, account))
    }
}

/// The link a started run gets, where the forge reports its URL and Shoal
/// reads its runs, so a watch can follow it; a run Shoal cannot read, such
/// as a Bitbucket pipeline, started all the same.
fn started_link(forge: &ForgeRepo, started: &StartedRun) -> Option<LinkedItem> {
    if !forge.supports(Capability::ReadRuns) {
        return None;
    }
    let (_, url) = forge.run(started.url.as_deref()?).ok()?;
    Some(LinkedItem {
        kind: LinkKind::Ci,
        url,
        title: started.workflow.clone(),
    })
}

/// Refuse a run started on any branch but the workspace's, or in another
/// repository.
fn on_own_branch_name(workspace: Option<&Workspace>, branch: &str) -> Result<()> {
    ensure!(
        workspace.is_some_and(|workspace| workspace.branch == branch),
        "workspace processes start CI runs only on their workspace's branch"
    );
    Ok(())
}

/// Refuse a run on any branch but the workspace's, or of another repository.
fn on_own_branch(workspace: Option<&Workspace>, run: &Run) -> Result<()> {
    ensure!(
        workspace.is_some_and(|workspace| run.branch.as_deref() == Some(&workspace.branch)),
        "workspace processes act only on CI runs of their workspace's branch; \
         use --pr for a PR's checks"
    );
    Ok(())
}

#[cfg(test)]
mod tests;
