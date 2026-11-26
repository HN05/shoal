//! Reverting a merged PR: through the forge where it reverts PRs, or else
//! by reading what a revert with Git needs.
use std::sync::Arc;

use anyhow::{Result, ensure};
use shoal_daemon_base::callers::Caller;
use shoal_forge::{
    conventions,
    items::revert::{NewRevert, RevertRequests, revert_options},
};
use shoal_forge_types::items::{
    create::PullOptions, item::Opened, link::ItemKind, reference::ItemInput, revert::MergedPull,
};
use shoal_protocol::{
    config::permissions::{Opening, Pushes},
    links::OpenOutcome,
    model::Assigner,
    space_names::SpaceSelector,
};

use crate::{
    Links, daemon::Daemon, forges, issue::assignees::AssignerOf, proposals::OpenOutcomeExt,
    target::named_or_linked,
};

impl Links {
    /// Have the forge open the revert of the merged PR `input` names, or of
    /// the space's linked PR, as its agent account, and record it as opened
    /// by the space. Its branch is the forge's, not a workspace's, so it is
    /// not assigned.
    pub async fn revert_pull(
        self: &Arc<Self>,
        daemon: &dyn Daemon,
        selector: &SpaceSelector,
        input: Option<ItemInput>,
        options: PullOptions,
        caller: Option<&Caller>,
    ) -> Result<OpenOutcome> {
        ensure!(
            options.base.is_none(),
            "a revert targets the reverted PR's base branch"
        );
        let id = self.link_space(daemon, selector).await?.id.to_owned();
        // Cleanup holds the workspace's gate, so it cannot remove the
        // workspace between opening the revert and recording it.
        let _gate = self.gates.lock_prs([&id]).await;
        let space = self.link_space(daemon, &SpaceSelector::id(&id)).await?;
        daemon.verify_space(&space).await?;
        if let Some(workspace) = space.to_workspace() {
            ensure!(
                workspace.state.accepts_links(),
                "workspace cannot accept a PR in its current state"
            );
        }
        let settings = daemon.acting_settings(&space).await?;
        if let Some(caller) = caller {
            let permissions = daemon
                .caller_permissions(caller, Some(space.repository_id()))
                .await?;
            ensure!(
                permissions.pr_open.value == Opening::Direct,
                "{}, and a forge's revert cannot wait for approval; ask the user to run shoal pr \
                 revert",
                permissions.pr_open.cited("pr_open")
            );
            ensure!(
                permissions.push.value == Pushes::Direct,
                "{}, and a forge's revert pushes its branch at once; ask the user to run shoal pr \
                 revert",
                permissions.push.cited("push")
            );
        }
        let (forge, account) = (
            forges::of_space(&self.forges, &space).await?,
            self.forges.agent_account(),
        );
        let pull = (named_or_linked(daemon, input, Some(&space.id), &forge, ItemKind::Pr)).await?;
        let current = forge.merged(&account, pull.pull()?).await?;
        let options = revert_options(&current.item, options);
        let required = &settings.pr.required_labels;
        conventions::require(&forge, &account, ItemKind::Pr, required, &options.labels).await?;
        let revert = NewRevert::new(options, settings.pr.draft);
        let created = forge.revert_pull(&account, &current, &revert).await?;
        let (_, warning) = self
            .keep_created(
                daemon,
                &space.id,
                ItemKind::Pr,
                &created.item,
                false,
                Assigner::of(caller),
            )
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
        self: &Arc<Self>,
        daemon: &dyn Daemon,
        selector: &SpaceSelector,
        input: Option<ItemInput>,
    ) -> Result<MergedPull> {
        let space = self.link_space(daemon, selector).await?;
        daemon.verify_space(&space).await?;
        let (forge, account) = (
            forges::of_space(&self.forges, &space).await?,
            self.forges.agent_account(),
        );
        let pull = (named_or_linked(daemon, input, Some(&space.id), &forge, ItemKind::Pr)).await?;
        forge.merged_pull(&account, pull.pull()?).await
    }
}
