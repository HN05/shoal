//! Rerunning failed and cancelling running CI jobs on a PR's current head,
//! as the workspace's agent account.
use anyhow::{Context, Result};

use super::actions::{linked, workspace_forge};
use crate::{
    daemon::{events, workspace::Manager},
    forge::{
        ci::{CiActed, CiAction, checks::validate_check_name},
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
        let forge = workspace_forge(&workspace).await?;
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

    async fn record_ci_action(&self, id: &str, url: &str, action: CiAction) -> Result<()> {
        let (id, url) = (id.to_owned(), url.to_owned());
        self.store
            .run(move |db| events::record_item(db, &id, ItemKind::Pr, &url, action.as_str()))
            .await
    }
}
