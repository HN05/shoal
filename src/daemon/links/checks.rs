//! Failed Actions logs resolved from a PR's current check, using the agent
//! account of the workspace or registered checkout.
use anyhow::{Context as _, Result, bail};

use crate::{
    daemon::workspace::Manager,
    forge::{
        ForgeRepo,
        capability::Capability,
        ci::checks::{CheckLog, NoJobLog, validate_check_name},
        items::link::{LinkKind, Selection},
        updates::Update,
    },
    model::Workspace,
    protocol::ItemContext,
};

impl Manager {
    /// Log failures stay on their CI update and never hide other activity;
    /// a check without a CI job gets no pointer to a log it does not have.
    pub(crate) async fn summarize_failed_checks(&self, workspace: &str, updates: &mut [Update]) {
        for index in 0..updates.len() {
            let update = &updates[index];
            let Some(check) = &update.check else { continue };
            if !check.failed()
                || !self
                    .pull_forge(&update.url)
                    .await
                    .is_ok_and(|forge| forge.supports(Capability::ReadJobLogs))
            {
                continue;
            }
            let selection = Selection {
                kind: Some(LinkKind::Pr),
                input: Some(update.url.clone()),
            };
            let context = ItemContext::Workspace(workspace.into());
            let log = self.check_log(&context, &selection, &check.name).await;
            let summary = match log {
                Ok(log) => log
                    .failed_step
                    .map(|step| format!("; failed step: {step}"))
                    .unwrap_or_default(),
                Err(error) if error.downcast_ref::<NoJobLog>().is_some() => continue,
                Err(error) => format!(
                    "; log unavailable: {}",
                    format!("{error:#}").chars().take(512).collect::<String>()
                ),
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
    /// the repository of `context`, or of the workspace's linked PR.
    pub async fn check_log(
        &self,
        context: &ItemContext,
        selection: &Selection,
        name: &str,
    ) -> Result<CheckLog> {
        validate_check_name(name)?;
        let place = self.place(context).await?;
        let (forge, account) = self.place_forge_account(&place).await?;
        let url = self
            .check_log_pr(place.workspace(), &forge, selection)
            .await?;
        forge.failed_check_log(&account, &url, name).await
    }

    /// The URL of the one PR `selection` names on `forge`, which in a
    /// workspace may be its linked PR.
    async fn check_log_pr(
        &self,
        workspace: Option<&Workspace>,
        forge: &ForgeRepo,
        selection: &Selection,
    ) -> Result<String> {
        if !matches!(selection.kind, None | Some(LinkKind::Pr)) {
            bail!(ONE_PR);
        }
        let Some(workspace) = workspace else {
            let input = (selection.input.as_deref()).context(super::actions::NOTHING_LINKED)?;
            return Ok(forge.pull(input)?.1);
        };
        let selection = Selection {
            kind: Some(LinkKind::Pr),
            input: selection.input.clone(),
        };
        match self.selected_items(workspace, &selection).await?.as_slice() {
            [(url, LinkKind::Pr)] => Ok(url.clone()),
            _ => bail!(ONE_PR),
        }
    }
}

/// Why a check log needs a single PR selected.
const ONE_PR: &str = "select one PR to read a check log; use shoal view pr <number> --check <name>";

#[cfg(test)]
mod tests;
