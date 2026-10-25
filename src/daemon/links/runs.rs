//! Starting, reading, rerunning and cancelling CI workflow runs of the
//! workspace's repository, as its agent account.
use std::collections::BTreeMap;

use anyhow::{Context, Result, ensure};

use crate::{
    daemon::workspace::Manager,
    forge::{
        ForgeRepo,
        account::Account,
        ci::{
            CiAction,
            checks::CheckLog,
            run::{NewRun, StartedRun},
            run_action::{Run, RunActed, validate_run_reference},
            run_view::{RunFilter, RunView, validate_filter},
        },
    },
    model::Workspace,
};

impl Manager {
    /// Start `workflow` on `branch`, by default the workspace's.
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
        forge
            .start_run(&account, &run)
            .await
            .with_context(|| format!("could not start a CI run on {}", run.branch))
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

    /// The run `input` names, with its jobs.
    pub async fn view_ci_run(&self, selector: &str, input: String) -> Result<RunView> {
        validate_run_reference(&input)?;
        let (_, forge, account) = self.run_forge(selector).await?;
        forge
            .view_run(&account, &input)
            .await
            .with_context(|| format!("could not read CI run {input}"))
    }

    /// The log of the job `job` of the run `input` names.
    pub async fn ci_run_job_log(
        &self,
        selector: &str,
        input: String,
        job: String,
    ) -> Result<CheckLog> {
        validate_run_reference(&input)?;
        let (_, forge, account) = self.run_forge(selector).await?;
        forge
            .run_job_log(&account, &input, &job)
            .await
            .with_context(|| format!("could not read the log of {job} in CI run {input}"))
    }

    /// The newest runs `filter` selects, on the workspace's branch unless it
    /// names another.
    pub async fn list_ci_runs(&self, selector: &str, mut filter: RunFilter) -> Result<Vec<Run>> {
        validate_filter(&filter)?;
        let (workspace, forge, account) = self.run_forge(selector).await?;
        filter.branch.get_or_insert(workspace.branch);
        forge
            .list_runs(&account, &filter)
            .await
            .context("could not list CI runs")
    }

    /// The verified workspace `selector` names, its forge and its agent
    /// account.
    async fn run_forge(&self, selector: &str) -> Result<(Workspace, ForgeRepo, Account)> {
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
         use shoal pr ci for a PR's checks"
    );
    Ok(())
}
