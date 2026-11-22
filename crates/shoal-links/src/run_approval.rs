//! Approving the CI runs a forge holds for approval, from a workspace or a
//! registered checkout: approval lets a fork's or first-time contributor's
//! code run, so it is the user's decision, or a scoped caller's as its
//! `ci_approve` permission allows, as the agent account.
use anyhow::{Context, Result};
use shoal_daemon_base::{callers::Caller, events, permitted_actions::Action};
use shoal_forge::{ci::approval::ApprovalRequests, transport::account::Account};
use shoal_forge_types::{
    ci::approval::RunsApproved,
    items::{
        link::{ItemKind, LinkKind},
        reference::{ItemInput, ItemRef},
    },
};
use shoal_protocol::{config::permissions::PermissionKey, model::Workspace, protocol::RepoTarget};

use crate::{
    Links,
    daemon::Daemon,
    forges,
    place::{Access, Place},
    target::{ForgeTarget, named_or_linked},
};

impl Links {
    /// Approve the run `run` names in the `target` repository, otherwise
    /// every waiting run on the head of the PR `pr` names or the linked PR,
    /// as the `ci_approve` permission of a scoped `caller` allows for the
    /// runs it names in its own repository.
    pub async fn approve_ci_runs(
        &self,
        daemon: &dyn Daemon,
        target: &RepoTarget,
        run: Option<ItemInput>,
        pr: Option<ItemInput>,
        caller: Option<&Caller>,
    ) -> Result<RunsApproved> {
        let named = match run.as_ref().or(pr.as_ref()) {
            Some(input) => forges::of_input(daemon, input).await?,
            None => None,
        };
        let target = (self.forge_target(daemon, target, named, Access::Change, caller)).await?;
        let workspace = target.own_place().and_then(Place::workspace);
        let workspace = workspace.as_ref();
        let forge = target.forge()?;
        // A scoped caller approves as the agent account.
        let account = match caller {
            Some(_) => self.agent_account(),
            None => self.user_account(),
        };
        let approved = match run {
            Some(input) => {
                let run = ItemRef::in_repository(forge, LinkKind::Ci, &input)?;
                // It asks once the forge would approve the run.
                let waiting = forge.waiting_run(&account, &run).await?;
                let approval = format!("approve CI run {}", waiting.url);
                authorize_approval(daemon, caller, &target, approval).await?;
                let url = forge
                    .approve_waiting(&account, waiting)
                    .await
                    .with_context(|| format!("could not approve CI run {run}"))?;
                self.record_approval(workspace, LinkKind::Ci, &url).await?;
                vec![url]
            }
            None => (self.approve_pr_runs(daemon, &target, &account, pr, caller)).await?,
        };
        Ok(RunsApproved { approved })
    }

    /// Approve each waiting run on the PR's head, stopping at the first that
    /// fails. A scoped `caller`'s approval names those runs, so a run that
    /// starts waiting after it, such as for a later push, asks again.
    async fn approve_pr_runs(
        &self,
        daemon: &dyn Daemon,
        target: &ForgeTarget,
        account: &Account,
        pr: Option<ItemInput>,
        caller: Option<&Caller>,
    ) -> Result<Vec<String>> {
        let (forge, place) = (target.forge()?, target.own_place());
        let space = place.and_then(Place::space_id);
        let pull = (named_or_linked(daemon, pr, space, forge, ItemKind::Pr)).await?;
        let (number, url) = (pull.pull()?, pull.to_string());
        let waiting = forge
            .pr_waiting_runs(account, number)
            .await
            .with_context(|| format!("could not approve the CI runs of {url}"))?;
        let runs: Vec<String> = waiting.iter().map(|run| run.url.to_string()).collect();
        let approval = format!("approve the CI runs of {url} waiting: {}", runs.join(", "));
        authorize_approval(daemon, caller, target, approval).await?;
        let (approved, result) = forge.approve_runs(account, &waiting).await;
        if !approved.is_empty() {
            let workspace = place.and_then(Place::workspace);
            (self.record_approval(workspace.as_ref(), LinkKind::Pr, &url)).await?;
        }
        result.with_context(|| {
            format!(
                "could not approve every waiting CI run of {url}; {} of {} approved",
                approved.len(),
                waiting.len()
            )
        })?;
        Ok(approved)
    }

    /// Record the approval as a change of the workspace to the run or PR, as
    /// rerunning and cancelling are; a registered checkout records none.
    async fn record_approval(
        &self,
        workspace: Option<&Workspace>,
        kind: LinkKind,
        url: &str,
    ) -> Result<()> {
        let Some(workspace) = workspace else {
            return Ok(());
        };
        let (id, url) = (workspace.id.clone(), url.to_owned());
        self.store
            .run(move |db| events::record_item(db, &id, kind, &url, "approve"))
            .await
    }
}

/// Let a scoped `caller` take `approval` of runs in its own repository,
/// `target`, as its `ci_approve` permission allows; only the user approves
/// runs of another.
async fn authorize_approval(
    daemon: &dyn Daemon,
    caller: Option<&Caller>,
    target: &ForgeTarget,
    approval: String,
) -> Result<()> {
    let Some(place) = target.own_place() else {
        return Ok(());
    };
    let action = Action::new(PermissionKey::CiApprove, approval)
        .in_repository(place.repository_id().clone());
    daemon.authorize_action(caller, action).await
}
