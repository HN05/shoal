//! Rerunning failed and cancelling running CI jobs on a PR's current head,
//! as the agent account of the workspace or registered checkout.
use anyhow::{Context, Result};
use shoal_ids::space_ids::SpaceId;

use crate::{
    daemon::{
        events,
        links::place::{Access, Place},
        manager::Manager,
        scope::Caller,
    },
    forge::{
        ci::{CiActed, CiAction},
        items::{link::ItemKind, reference::ItemInput},
    },
    protocol::RepoTarget,
};
use shoal_forge::ci::{CiRequests, checks::validate_check_name};

impl Manager {
    /// Rerun the failed, or cancel the running, `checks` of a PR in the
    /// `target` repository, or every such CI job when none is named.
    pub async fn ci_action(
        &self,
        target: &RepoTarget,
        item: Option<ItemInput>,
        action: CiAction,
        checks: Vec<String>,
        caller: Option<&Caller>,
    ) -> Result<CiActed> {
        for check in &checks {
            validate_check_name(check)?;
        }
        let acted = self.target_item(target, item, ItemKind::Pr, Access::Change, caller);
        let (target, pull) = acted.await?;
        let account = self.target_account(&target, Access::Change, caller);
        let (forge, number, url) = (target.forge()?, pull.pull()?, pull.to_string());
        let plan = forge
            .plan_ci(&account, number, action, &checks)
            .await
            .with_context(|| format!("could not {action} checks of {url}"))?;
        let (sent, result) = forge.post_ci(&account, &plan.endpoints).await;
        if sent > 0
            && let Some(workspace) = target.own_place().and_then(Place::workspace)
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

    async fn record_ci_action(&self, id: &SpaceId, url: &str, action: CiAction) -> Result<()> {
        let (id, url) = (id.to_owned(), url.to_owned());
        self.store
            .run(move |db| events::record_item(db, &id, ItemKind::Pr, &url, action.as_str()))
            .await
    }
}
