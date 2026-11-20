//! Failed Actions logs resolved from a PR's current check, using the agent
//! account of the workspace or registered checkout.
use anyhow::{Context as _, Result, bail};
use shoal_links::place::{Access, Place};
use shoal_links::space::LinkSpace;
use shoal_links::target::NOTHING_LINKED;

use crate::{
    daemon::{manager::Manager, scope::Caller},
    forge::{
        ForgeRepo,
        capability::Capability,
        ci::checks::CheckLog,
        items::{
            link::{LinkKind, Selection},
            reference::ItemRef,
        },
        updates::Update,
    },
    protocol::RepoTarget,
};
use shoal_forge::{
    capability::CapabilityRequests,
    ci::checks::{CheckRequests, failed_log_summary, validate_check_name},
};
use shoal_protocol::space_names::SpaceSelector;

impl Manager {
    /// Log failures stay on their CI update and never hide other activity;
    /// a check without a CI job gets no pointer to a log it does not have.
    pub(crate) async fn summarize_failed_checks(
        &self,
        space: &SpaceSelector,
        updates: &mut [Update],
    ) {
        // Logs are read in the space's own context: a chat reads them from
        // its repository's checkout.
        let Ok(space) = self.link_space(space).await else {
            return;
        };
        let target = RepoTarget::from(space.item_context());
        for index in 0..updates.len() {
            let update = &updates[index];
            let Some(check) = &update.check else { continue };
            // An update with a check is of the PR its URL names.
            let Ok(pull) = ItemRef::parse_kind(LinkKind::Pr, &update.url) else {
                continue;
            };
            if !check.result.failed()
                || !self
                    .item_forge(&pull)
                    .await
                    .is_ok_and(|forge| forge.supports(Capability::ReadJobLogs))
            {
                continue;
            }
            let selection = Selection {
                kind: Some(LinkKind::Pr),
                input: Some(pull.into()),
            };
            let log = self.check_log(&target, &selection, &check.name, None).await;
            let Some(summary) = failed_log_summary(log) else {
                continue;
            };
            let message = format!(
                "{}{summary}; read the log with shoal view {} --check '{}'",
                update.message,
                update.url,
                check.name.replace('\'', "'\\''")
            );
            let previous = std::mem::replace(&mut updates[index].message, message);
            if serde_json::to_vec(updates).map_or(true, |bytes| {
                bytes.len() >= crate::protocol::MAX_FRAME - 1024
            }) {
                updates[index].message = previous;
            }
        }
    }

    /// The log of the failed check `name` of the PR `selection` names in
    /// the `target` repository, or of the space's linked PR, read as the
    /// agent account in the request's own repository and as `caller`
    /// elsewhere.
    pub async fn check_log(
        &self,
        target: &RepoTarget,
        selection: &Selection,
        name: &str,
        caller: Option<&Caller>,
    ) -> Result<CheckLog> {
        validate_check_name(name)?;
        let target = (self.selection_target(target, selection, true, caller)).await?;
        let account = match target.own {
            true => self.links.agent_account(),
            false => self.links.elsewhere_account(Access::Read, caller),
        };
        let space = target.own_place().and_then(Place::space);
        let forge = target.forge()?;
        let pull = self.check_log_pr(space, forge, selection).await?;
        forge.failed_check_log(&account, &pull, name).await
    }

    /// The URL of the one PR `selection` names on `forge`, which in a
    /// space may be its linked PR.
    async fn check_log_pr(
        &self,
        space: Option<&LinkSpace>,
        forge: &ForgeRepo,
        selection: &Selection,
    ) -> Result<ItemRef> {
        if !matches!(selection.kind, None | Some(LinkKind::Pr)) {
            bail!(ONE_PR);
        }
        let Some(space) = space else {
            let input = (selection.input.as_ref()).context(NOTHING_LINKED)?;
            return ItemRef::in_repository(forge, LinkKind::Pr, input);
        };
        let selection = Selection {
            kind: Some(LinkKind::Pr),
            input: selection.input.clone(),
        };
        // Of the linked PRs, only an assigned one is read without a number.
        let prs = (self.selection_items(space, &selection, super::wait::followed_items)).await?;
        match prs.as_slice() {
            [pull] if pull.kind() == LinkKind::Pr => Ok(pull.clone()),
            _ => bail!(ONE_PR),
        }
    }
}

/// Why a check log needs a single PR selected.
const ONE_PR: &str = "select one PR to read a check log; use shoal view pr <number> --check <name>";

#[cfg(test)]
mod tests;
