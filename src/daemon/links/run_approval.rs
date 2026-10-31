//! Approving the CI runs a forge holds for approval, as the user, from a
//! workspace or a registered checkout: approval lets a fork's or first-time
//! contributor's code run, so it is the user's decision.
use anyhow::{Context, Result};

use super::actions::named_or_linked;
use crate::{
    daemon::{events, workspace::Manager},
    forge::{
        ForgeRepo,
        account::Account,
        ci::approval::RunsApproved,
        items::link::{ItemKind, LinkKind},
    },
    model::Workspace,
    protocol::ItemContext,
};

impl Manager {
    /// Approve the run `run` names in the repository of `context`, otherwise
    /// every waiting run on the head of the PR `pr` names or the linked PR.
    pub async fn approve_ci_runs(
        &self,
        context: &ItemContext,
        run: Option<String>,
        pr: Option<String>,
    ) -> Result<RunsApproved> {
        let place = self.place(context).await?;
        let workspace = place.workspace();
        let forge = self.place_forge(&place).await?;
        let account = self.user_account();
        let approved = match run {
            Some(input) => {
                let url = forge
                    .approve_run(&account, &input)
                    .await
                    .with_context(|| format!("could not approve CI run {input}"))?;
                self.record_approval(workspace, LinkKind::Ci, &url).await?;
                vec![url]
            }
            None => {
                self.approve_pr_runs(workspace, &forge, &account, pr)
                    .await?
            }
        };
        Ok(RunsApproved { approved })
    }

    /// Approve each waiting run on the PR's head, stopping at the first that
    /// fails.
    async fn approve_pr_runs(
        &self,
        workspace: Option<&Workspace>,
        forge: &ForgeRepo,
        account: &Account,
        pr: Option<String>,
    ) -> Result<Vec<String>> {
        let input = named_or_linked(pr, workspace, ItemKind::Pr)?;
        let (number, url) = forge.pull(&input)?;
        let waiting = forge
            .pr_waiting_runs(account, number)
            .await
            .with_context(|| format!("could not approve the CI runs of {url}"))?;
        let mut approved = Vec::new();
        let mut failure = None;
        for run in &waiting {
            match forge.approve_run(account, &run.url).await {
                Ok(run) => approved.push(run),
                Err(error) => {
                    failure = Some(error);
                    break;
                }
            }
        }
        if !approved.is_empty() {
            self.record_approval(workspace, LinkKind::Pr, &url).await?;
        }
        match failure {
            Some(error) => Err(error.context(format!(
                "could not approve every waiting CI run of {url}; {} of {} approved",
                approved.len(),
                waiting.len()
            ))),
            None => Ok(approved),
        }
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
