//! Starting, reading, rerunning and cancelling CI workflow runs of the
//! workspace's repository, as its agent account.
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
};

impl Manager {
    /// Start `workflow` on `branch`, by default the workspace's, and link the
    /// run as opened where the forge reports it, so watches follow it.
    pub async fn start_ci_run(
        &self,
        selector: &str,
        workflow: Option<String>,
        branch: Option<String>,
        inputs: BTreeMap<String, String>,
    ) -> Result<StartedRun> {
        let (workspace, forge, account) = self.run_forge(selector).await?;
        let run = NewRun {
            workflow,
            branch: branch.unwrap_or_else(|| workspace.branch.clone()),
            inputs,
        };
        let started = forge
            .start_run(&account, &run)
            .await
            .with_context(|| format!("could not start a CI run on {}", run.branch))?;
        if let Some(url) = &started.url {
            let item = LinkedItem {
                kind: LinkKind::Ci,
                url: forge.run(url)?.1,
                title: started.workflow.clone(),
            };
            self.record_items(&workspace.id, vec![(Relation::Opened, item)])
                .await?;
        }
        Ok(started)
    }

    /// Rerun or cancel the run `input` names; workspace processes act only
    /// on runs of their workspace's branch.
    pub async fn ci_run_action(
        &self,
        selector: &str,
        input: String,
        action: CiAction,
        failed: bool,
        scoped: bool,
    ) -> Result<RunActed> {
        validate_run_reference(&input)?;
        let (workspace, forge, account) = self.run_forge(selector).await?;
        let run = forge
            .find_run(&account, &input, action)
            .await
            .with_context(|| format!("could not {action} CI run {input}"))?;
        if scoped {
            on_own_branch(&workspace, &run)?;
        }
        forge
            .act_on_run(&account, &run, action, failed)
            .await
            .with_context(|| format!("could not {action} {}", run.url))
    }

    /// The run `input` names.
    pub async fn view_ci_run(&self, selector: &str, input: String) -> Result<Run> {
        validate_run_reference(&input)?;
        let (_, forge, account) = self.run_forge(selector).await?;
        forge
            .view_run(&account, &input)
            .await
            .with_context(|| format!("could not read CI run {input}"))
    }

    /// Every job of the run `input` names.
    pub async fn ci_run_jobs(&self, selector: &str, input: String) -> Result<Vec<RunJob>> {
        validate_run_reference(&input)?;
        let (_, forge, account) = self.run_forge(selector).await?;
        forge
            .run_jobs(&account, &input)
            .await
            .with_context(|| format!("could not read the jobs of CI run {input}"))
    }

    /// The log of the job `job` of the run `input` names.
    pub async fn ci_run_job_log(
        &self,
        selector: &str,
        input: String,
        job: JobRef,
    ) -> Result<CheckLog> {
        validate_run_reference(&input)?;
        let (_, forge, account) = self.run_forge(selector).await?;
        forge
            .run_job_log(&account, &input, &job)
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

/// Refuse a run on any branch but the workspace's.
fn on_own_branch(workspace: &Workspace, run: &Run) -> Result<()> {
    ensure!(
        run.branch.as_deref() == Some(workspace.branch.as_str()),
        "workspace processes act only on CI runs of their workspace's branch; \
         use --pr for a PR's checks"
    );
    Ok(())
}
