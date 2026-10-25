//! Rerunning failed and cancelling running CI jobs on a PR's current head,
//! and starting workflow runs on a branch, as the workspace's agent account.
use std::collections::BTreeMap;

use anyhow::{Context, Result};

use super::actions::linked;
use crate::{
    daemon::{events, workspace::Manager},
    forge::{
        ci::{
            CiActed, CiAction,
            checks::validate_check_name,
            run::{NewRun, StartedRun},
        },
        items::link::ItemKind,
    },
};

impl Manager {
    /// Rerun the failed, or cancel the running, `checks` of a PR in the
    /// workspace's repository, or every such CI job when none is named.
    pub async fn ci_action(
        &self,
        selector: &str,
        item: Option<String>,
        action: CiAction,
        checks: Vec<String>,
    ) -> Result<CiActed> {
        for check in &checks {
            validate_check_name(check)?;
        }
        let workspace = self.workspace(selector).await?;
        self.verify_worktree(&workspace).await?;
        let forge = self.workspace_forge(&workspace).await?;
        let input = match item {
            Some(input) => input,
            None => linked(&workspace, ItemKind::Pr)?,
        };
        let (number, url) = forge.pull(&input)?;
        let account = self.agent_account(&workspace).await?;
        let plan = forge
            .plan_ci(&account, number, action, &checks)
            .await
            .with_context(|| format!("could not {action} checks of {url}"))?;
        let (sent, result) = forge.post_ci(&account, &plan.endpoints).await;
        if sent > 0 {
            self.record_ci_action(&workspace.id, &url, action).await?;
        }
        result.with_context(|| {
            format!(
                "could not {action} every check of {url}; {sent} of {} requests succeeded",
                plan.endpoints.len()
            )
        })?;
        Ok(CiActed {
            url,
            action,
            checks: plan.checks,
        })
    }

    /// Start `workflow` on `branch`, by default the workspace's, in the
    /// workspace's repository.
    pub async fn start_ci_run(
        &self,
        selector: &str,
        workflow: Option<String>,
        branch: Option<String>,
        inputs: BTreeMap<String, String>,
    ) -> Result<StartedRun> {
        let workspace = self.workspace(selector).await?;
        self.verify_worktree(&workspace).await?;
        let forge = self.workspace_forge(&workspace).await?;
        let run = NewRun {
            workflow,
            branch: branch.unwrap_or_else(|| workspace.branch.clone()),
            inputs,
        };
        let account = self.agent_account(&workspace).await?;
        forge
            .start_run(&account, &run)
            .await
            .with_context(|| format!("could not start a CI run on {}", run.branch))
    }

    async fn record_ci_action(&self, id: &str, url: &str, action: CiAction) -> Result<()> {
        let (id, url) = (id.to_owned(), url.to_owned());
        self.store
            .run(move |db| events::record_item(db, &id, ItemKind::Pr, &url, action.as_str()))
            .await
    }
}
