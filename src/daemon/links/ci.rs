//! Rerunning failed and cancelling running CI jobs on a PR's current head,
//! as the agent account of the workspace or registered checkout.
use anyhow::{Context, Result};

use super::actions::named_or_linked;
use crate::{
    daemon::{events, workspace::Manager},
    forge::{
        ci::{CiActed, CiAction, checks::validate_check_name},
        items::link::ItemKind,
    },
    protocol::ItemContext,
};

impl Manager {
    /// Rerun the failed, or cancel the running, `checks` of a PR in the
    /// repository of `context`, or every such CI job when none is named.
    pub async fn ci_action(
        &self,
        context: &ItemContext,
        item: Option<String>,
        action: CiAction,
        checks: Vec<String>,
    ) -> Result<CiActed> {
        for check in &checks {
            validate_check_name(check)?;
        }
        let place = self.place(context).await?;
        let (forge, account) = self.place_forge_account(&place).await?;
        let input = named_or_linked(item, place.workspace(), ItemKind::Pr)?;
        let (number, url) = forge.pull(&input)?;
        let plan = forge
            .plan_ci(&account, number, action, &checks)
            .await
            .with_context(|| format!("could not {action} checks of {url}"))?;
        let (sent, result) = forge.post_ci(&account, &plan.endpoints).await;
        if sent > 0
            && let Some(workspace) = place.workspace()
        {
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
