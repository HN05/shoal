//! Revert a merged PR of a space's repository: through a forge that opens
//! the revert itself, or by reading what reverting its merge with Git in a
//! new workspace needs.
use anyhow::{Result, bail, ensure};

use super::{proposals::OpenOutcome, space::LinkSpace};
use crate::client::space_names::SpaceSelector;
use crate::{
    config::repo::{Opening, Pushes},
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
    /// the space's linked PR, as its agent account, and record it as opened
    /// by the space. Its branch is the forge's, not a workspace's, so it is
    /// not assigned.
    pub async fn revert_pull(
        &self,
        selector: &SpaceSelector,
        input: Option<String>,
        options: PullOptions,
        caller: Option<&Caller>,
    ) -> Result<OpenOutcome> {
        ensure!(
            options.base.is_none(),
            "a revert targets the reverted PR's base branch"
        );
        let id = self.link_space(selector).await?.id().to_owned();
        // Cleanup holds the workspace's gate, so it cannot remove the
        // workspace between opening the revert and recording it.
        let _gate = self.gates.lock_prs([&id]).await;
        let space = self.link_space(&SpaceSelector::id(&id)).await?;
        self.verify_space(&space).await?;
        let settings = match &space {
            LinkSpace::Workspace(workspace) => {
                ensure!(
                    workspace.state.accepts_links(),
                    "workspace cannot accept a PR in its current state"
                );
                self.workspace_settings(workspace).await?
            }
            LinkSpace::Chat { checkout, .. } => self.repository_settings(&checkout.id).await?,
        };
        if self.proposer(caller).await.is_some() {
            if settings.permissions.pr_open == Opening::Propose {
                bail!(
                    "[permissions] pr_open needs the user's approval for new PRs, and a forge's revert cannot wait for it; ask the user to run shoal pr revert"
                );
            }
            if settings.permissions.push != Pushes::Direct {
                bail!(
                    "[permissions] push holds back or refuses agents' pushes, and a forge's revert pushes its branch at once; ask the user to run shoal pr revert"
                );
            }
        }
        let (forge, account) = (self.space_forge(&space).await?, self.agent_account());
        let input = match input {
            Some(input) => input,
            None => self.linked_item(space.id(), ItemKind::Pr).await?,
        };
        let (number, _) = forge.pull(&input)?;
        let current = forge.merged(&account, number).await?;
        let options = revert_options(&current.item, options);
        let required = &settings.pr.required_labels;
        conventions::require(&forge, &account, ItemKind::Pr, required, &options.labels).await?;
        let revert = NewRevert::new(options, settings.pr.draft);
        let created = forge.revert_pull(&account, &current, &revert).await?;
        let (_, warning) = self
            .keep_created(space.id(), ItemKind::Pr, &created.item, false)
            .await?;
        let checked = conventions::warnings(
            &forge,
            &account,
            space.origin_path(),
            &created.item,
            Some(&revert.body),
            required,
        )
        .await;
        Ok(OpenOutcome::opened(Opened {
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

    /// The merged PR `input` names, or the space's linked PR, with what
    /// reverting its merge with Git needs, read as the agent account.
    pub async fn merged_pull(
        &self,
        selector: &SpaceSelector,
        input: Option<String>,
    ) -> Result<MergedPull> {
        let space = self.link_space(selector).await?;
        self.verify_space(&space).await?;
        let (forge, account) = (self.space_forge(&space).await?, self.agent_account());
        let input = match input {
            Some(input) => input,
            None => self.linked_item(space.id(), ItemKind::Pr).await?,
        };
        let (number, _) = forge.pull(&input)?;
        forge.merged_pull(&account, number).await
    }
}

#[cfg(test)]
mod tests;
