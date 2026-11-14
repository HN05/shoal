//! Approving the CI runs a forge holds for approval, from a workspace or a
//! registered checkout: approval lets a fork's or first-time contributor's
//! code run, so it is the user's decision, or a scoped caller's as its
//! `ci_approve` permission allows, as the agent account.
use anyhow::{Context, Result};

use super::actions::Place;
use crate::{
    config::permissions::PermissionKey,
    daemon::{events, permissions::Action, scope::Caller, workspace::Manager},
    forge::{
        ForgeRepo,
        account::Account,
        ci::approval::RunsApproved,
        items::{
            link::{ItemKind, LinkKind},
            reference::{ItemInput, ItemRef},
        },
    },
    model::Workspace,
    protocol::ItemContext,
};

impl Manager {
    /// Approve the run `run` names in the repository of `context`, otherwise
    /// every waiting run on the head of the PR `pr` names or the linked PR,
    /// as the `ci_approve` permission of a scoped `caller` allows for the
    /// runs it names.
    pub async fn approve_ci_runs(
        &self,
        context: &ItemContext,
        run: Option<ItemInput>,
        pr: Option<ItemInput>,
        caller: Option<&Caller>,
    ) -> Result<RunsApproved> {
        let place = self.place(context).await?;
        let workspace = place.workspace();
        let forge = self.place_forge(&place).await?;
        // A scoped caller approves as the agent account.
        let account = match caller {
            Some(_) => self.agent_account(),
            None => self.user_account(),
        };
        let approved = match run {
            Some(input) => {
                let run = ItemRef::in_repository(&forge, LinkKind::Ci, &input)?;
                // It asks once the forge would approve the run.
                let waiting = forge.waiting_run(&account, &run).await?;
                let approval = format!("approve CI run {waiting}");
                self.authorize_approval(caller, &place, approval).await?;
                let url = forge
                    .approve_run(&account, &run)
                    .await
                    .with_context(|| format!("could not approve CI run {run}"))?;
                self.record_approval(workspace, LinkKind::Ci, &url).await?;
                vec![url]
            }
            None => (self.approve_pr_runs(&place, &forge, &account, pr, caller)).await?,
        };
        Ok(RunsApproved { approved })
    }

    /// Let a scoped `caller` take `approval` of runs in `place`'s repository
    /// as its `ci_approve` permission allows.
    async fn authorize_approval(
        &self,
        caller: Option<&Caller>,
        place: &Place,
        approval: String,
    ) -> Result<()> {
        let action = Action::new(PermissionKey::CiApprove, approval)
            .in_repository(place.repository_id().clone());
        self.authorize_action(caller, action).await
    }

    /// Approve each waiting run on the PR's head, stopping at the first that
    /// fails. A scoped `caller`'s approval names those runs, so a run that
    /// starts waiting after it, such as for a later push, asks again.
    async fn approve_pr_runs(
        &self,
        place: &Place,
        forge: &ForgeRepo,
        account: &Account,
        pr: Option<ItemInput>,
        caller: Option<&Caller>,
    ) -> Result<Vec<String>> {
        let pull = (self.named_or_linked(pr, place.space_id(), forge, ItemKind::Pr)).await?;
        let (number, url) = (pull.pull()?, pull.to_string());
        let waiting = forge
            .pr_waiting_runs(account, number)
            .await
            .with_context(|| format!("could not approve the CI runs of {url}"))?;
        let runs: Vec<String> = waiting.iter().map(|run| run.url.to_string()).collect();
        let approval = format!("approve the CI runs of {url} waiting: {}", runs.join(", "));
        self.authorize_approval(caller, place, approval).await?;
        let (approved, result) = forge.approve_runs(account, &waiting).await;
        if !approved.is_empty() {
            self.record_approval(place.workspace(), LinkKind::Pr, &url)
                .await?;
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
