//! Opening a workspace's PR, or proposing it, and the review gate a new
//! PR waits for.
use std::sync::Arc;

use anyhow::Result;
use shoal_daemon_base::callers::Caller;
use shoal_forge::{
    conventions,
    items::{attach::AttachRequests, create::CreateRequests, item::ItemRequests},
    transport::account::Account,
};
use shoal_forge_types::{
    ForgeRepo,
    items::{
        create::{NewPull, PullOptions},
        item::Opened,
        link::ItemKind,
    },
};
use shoal_protocol::{
    config::permissions::Opening,
    links::OpenOutcome,
    model::{Assigner, Workspace},
    space_names::SpaceSelector,
};

use super::{ensure_branch_head, head_repository};
use crate::{
    Links,
    daemon::Daemon,
    issue::assignees::AssignerOf,
    proposals::{OpenOutcomeExt, proposer},
};

impl Links {
    /// Link the open PR of the workspace's branch, or open one as its agent
    /// account and link it; only a removal that starts meanwhile leaves it
    /// unlinked. A workspace process's new PR is recorded for the user's
    /// approval instead when its `pr_open` permission says so, or when
    /// `push_held` says the `push` permission held back the branch's push,
    /// which approving it runs first; a held push to an open PR is recorded
    /// on its own.
    pub async fn open_pull(
        self: &Arc<Self>,
        daemon: &dyn Daemon,
        selector: &SpaceSelector,
        options: PullOptions,
        push_held: bool,
        caller: Option<&Caller>,
    ) -> Result<OpenOutcome> {
        let proposer = proposer(daemon, caller).await;
        // Cleanup holds the workspace's gate, so it cannot remove the
        // workspace between opening the PR and linking it.
        let (workspace, _gate) = self.lock_pr_gate(daemon, selector).await?;
        let branch = workspace.require_branch("shoal pr open")?.clone();
        daemon.verify_worktree(&workspace).await?;
        let head = crate::pr::current_head(&workspace).await?;
        let push = match push_held {
            true => Some(
                self.held_push(daemon, &workspace, proposer.as_ref(), caller)
                    .await?,
            ),
            false => None,
        };
        anyhow::ensure!(
            workspace.state.accepts_links(),
            "workspace cannot accept a PR in its current state"
        );
        let settings = daemon.workspace_settings(&workspace).await?;
        let required = &settings.pr.required_labels;
        let (forge, account) = self.workspace_forge_account(&workspace).await?;
        let head_repository = head_repository(&workspace.path, &branch, &forge).await?;
        let existing = forge
            .open_pull_for(&account, &branch, head_repository.as_deref())
            .await?;
        let (item, body, unfinished) = match existing {
            Some(number) => {
                let item = forge.read(&account, ItemKind::Pr, number).await?.item;
                // The PR is linked now; approving the push updates it.
                if let Some(push) = push {
                    self.link_open_pull(daemon, &workspace, &item).await?;
                    self.ensure_workspace_not_pushing(daemon, &workspace)
                        .await?;
                    let proposed = self.propose_push(daemon, &workspace, push).await?;
                    return Ok(OpenOutcome::Proposed(proposed));
                }
                (item, None, None)
            }
            None => {
                daemon.require_pr_review(&workspace, &head, caller).await?;
                conventions::require(&forge, &account, ItemKind::Pr, required, &options.labels)
                    .await?;
                let pull = NewPull {
                    head_repository,
                    ..new_pull(&workspace, &forge, &account, options, settings.pr.draft).await?
                };
                // A PR whose branch is not pushed cannot open before the
                // push is approved, which then opens it too.
                let proposes =
                    push.is_some() || self.proposes_pull(daemon, &workspace, caller).await?;
                if let Some(proposer) = proposer.filter(|_| proposes) {
                    forge.check_uploads(&account, &pull.attachments).await?;
                    self.ensure_workspace_not_pushing(daemon, &workspace)
                        .await?;
                    let proposed = self
                        .propose_pull(daemon, &workspace, pull, head, push, proposer)
                        .await?;
                    return Ok(OpenOutcome::Proposed(proposed));
                }
                let created = forge.create_pull(&account, &pull).await?;
                (created.item, Some(pull.body), created.unfinished)
            }
        };
        let created = body.is_some();
        let (linked, warning) = if created {
            ensure_branch_head(&workspace, &item)?;
            (self.keep_created(
                daemon,
                &workspace.id,
                ItemKind::Pr,
                &item,
                true,
                Assigner::of(caller),
            ))
            .await?
        } else {
            self.link_open_pull(daemon, &workspace, &item).await?;
            (true, Vec::new())
        };
        let checked = conventions::warnings(
            &forge,
            &account,
            &workspace.path,
            &item,
            body.as_deref(),
            required,
        )
        .await;
        Ok(OpenOutcome::opened(Opened {
            item,
            created,
            linked,
            warnings: warning
                .into_iter()
                .chain(unfinished)
                .chain(checked)
                .collect(),
        }))
    }

    /// Whether `caller`'s `shoal pr open` would open a new PR that its
    /// review gate refuses; checked before anything is pushed.
    pub async fn check_pr_review_gate(
        self: &Arc<Self>,
        daemon: &dyn Daemon,
        selector: &SpaceSelector,
        caller: Option<&Caller>,
    ) -> Result<()> {
        let workspace = daemon.workspace(selector).await?;
        let Some(branch) = workspace.branch().cloned() else {
            return Ok(());
        };
        let settings = daemon.workspace_settings(&workspace).await?;
        if !daemon.pr_review_required(&settings, caller) {
            return Ok(());
        }
        let (forge, account) = self.workspace_forge_account(&workspace).await?;
        let head_repository = head_repository(&workspace.path, &branch, &forge).await?;
        let existing = forge
            .open_pull_for(&account, &branch, head_repository.as_deref())
            .await?;
        match existing {
            Some(_) => Ok(()),
            None => {
                let head = crate::pr::current_head(&workspace).await?;
                daemon.require_pr_review(&workspace, &head, caller).await
            }
        }
    }
}

/// Fill what the caller left unset: the title from the first linked issue,
/// then the last commit; a closing reference to each linked issue; the base
/// workspace's branch, then the default branch; draft state from
/// `default_draft`.
async fn new_pull(
    workspace: &Workspace,
    forge: &ForgeRepo,
    account: &Account,
    options: PullOptions,
    default_draft: bool,
) -> Result<NewPull> {
    let issues = (workspace.links.issues.iter())
        .map(|linked| {
            linked.url.belongs_to(forge)?;
            linked.url.issue()
        })
        .collect::<Result<Vec<_>>>()?;
    let linked_title = (workspace.links.issue()).and_then(|linked| linked.title.clone());
    let title = match (options.title, linked_title, issues.first()) {
        (Some(title), _, _) => title,
        (None, Some(title), _) => title,
        (None, None, Some(number)) => {
            forge
                .read(account, ItemKind::Issue, number.get())
                .await?
                .item
                .title
        }
        (None, None, None) => shoal_core::git::latest_subject(&workspace.path)
            .await?
            .trim()
            .to_owned(),
    };
    let base_branch = workspace
        .base_workspace
        .as_ref()
        .and_then(|base| base.branch.clone());
    let base = match (options.base, base_branch) {
        (Some(base), _) => base,
        (None, Some(branch)) => branch,
        (None, None) => {
            shoal_core::git::default_branch::resolve(
                &workspace.path,
                shoal_core::git::default_branch::DefaultBranchLookup::Discover,
            )
            .await?
        }
    };
    Ok(NewPull {
        title,
        body: conventions::pull_description(options.body, &issues),
        head: workspace.require_branch("shoal pr open")?.clone(),
        head_repository: None,
        base,
        draft: options.draft.unwrap_or(default_draft),
        labels: options.labels,
        reviewers: options.reviewers,
        assignees: options.assignees,
        milestone: options.milestone,
        attachments: options.attachments,
    })
}

impl Links {
    /// Whether a scoped `caller`'s `pr_open` permission has it propose the
    /// new PR of `workspace` instead of opening it; unscoped callers open.
    async fn proposes_pull(
        self: &Arc<Self>,
        daemon: &dyn Daemon,
        workspace: &Workspace,
        caller: Option<&Caller>,
    ) -> Result<bool> {
        let Some(caller) = caller else {
            return Ok(false);
        };
        let permissions = daemon
            .caller_permissions(caller, Some(workspace.repository()))
            .await?;
        Ok(permissions.pr_open.value != Opening::Direct)
    }
}
