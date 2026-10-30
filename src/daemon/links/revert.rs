//! Revert a merged PR of a workspace's repository: through a forge that
//! opens the revert itself, or by reading what reverting its merge with Git
//! in a new workspace needs.
use anyhow::{Result, bail, ensure};

use super::{actions::linked, proposals::OpenOutcome};
use crate::{
    config::repo::Opening,
    daemon::{scope::Caller, workspace::Manager},
    forge::{
        conventions,
        items::{
            create::PullOptions,
            item::Opened,
            link::ItemKind,
            revert::{MergedPull, NewRevert, revert_options},
        },
    },
};

impl Manager {
    /// Have the forge open the revert of the merged PR `input` names, or of
    /// the workspace's linked PR, as its agent account, and record it as
    /// opened by the workspace. Its branch is the forge's, not the
    /// workspace's, so it is not assigned.
    pub async fn revert_pull(
        &self,
        selector: &str,
        input: Option<String>,
        options: PullOptions,
        caller: Option<&Caller>,
    ) -> Result<OpenOutcome> {
        ensure!(
            options.base.is_none(),
            "a revert targets the reverted PR's base branch"
        );
        // Cleanup holds the workspace's gate, so it cannot remove the
        // workspace between opening the revert and recording it.
        let (workspace, _gate) = self.lock_pr_gate(selector).await?;
        self.verify_worktree(&workspace).await?;
        ensure!(
            workspace.state.accepts_links(),
            "workspace cannot accept a PR in its current state"
        );
        let settings = self.workspace_settings(&workspace).await?;
        if self.proposer(caller).await.is_some() && settings.permissions.pr_open == Opening::Propose
        {
            bail!(
                "[permissions] pr_open needs the user's approval for new PRs, and a forge's revert cannot wait for it; ask the user to run shoal pr revert"
            );
        }
        let (forge, account) = self.workspace_forge_account(&workspace).await?;
        let input = match input {
            Some(input) => input,
            None => linked(&workspace, ItemKind::Pr)?,
        };
        let (number, _) = forge.pull(&input)?;
        let current = forge.merged(&account, number).await?;
        let options = revert_options(&current.item, options);
        let required = &settings.pr.required_labels;
        conventions::require(&forge, &account, ItemKind::Pr, required, &options.labels).await?;
        let revert = NewRevert::new(options, settings.pr.draft);
        let created = forge.revert_pull(&account, &current, &revert).await?;
        let (_, warning) = self
            .keep_created(&workspace, ItemKind::Pr, &created.item, false)
            .await?;
        let checked = conventions::warnings(
            &forge,
            &account,
            &workspace.path,
            &created.item,
            Some(&revert.body),
            required,
        )
        .await;
        Ok(OpenOutcome::Opened(Opened {
            item: created.item,
            created: true,
            linked: false,
            warnings: warning
                .into_iter()
                .chain(created.unfinished)
                .chain(checked)
                .collect(),
        }))
    }

    /// The merged PR `input` names, or the workspace's linked PR, with what
    /// reverting its merge with Git needs, read as the workspace's agent
    /// account.
    pub async fn merged_pull(&self, selector: &str, input: Option<String>) -> Result<MergedPull> {
        let workspace = self.workspace(selector).await?;
        self.verify_worktree(&workspace).await?;
        let (forge, account) = self.workspace_forge_account(&workspace).await?;
        let input = match input {
            Some(input) => input,
            None => linked(&workspace, ItemKind::Pr)?,
        };
        let (number, _) = forge.pull(&input)?;
        forge.merged_pull(&account, number).await
    }
}

#[cfg(test)]
mod tests;
