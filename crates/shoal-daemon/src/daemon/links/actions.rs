//! Opening a workspace's PR, or proposing it, and the review gate a new
//! PR waits for.
use anyhow::{Context, Result};
use shoal_links::actions::ensure_branch_head;
use shoal_links::issue::assignees::AssignerOf;
use shoal_links::proposals::OpenOutcomeExt;
use shoal_protocol::links::OpenOutcome;
use shoal_refs::BranchName;

use super::issue::Assigner;
use crate::{
    config::permissions::Opening,
    daemon::{manager::Manager, scope::Caller},
    forge::{
        ForgeRepo,
        items::{
            create::{NewPull, PullOptions},
            item::Opened,
            link::ItemKind,
        },
    },
    model::Workspace,
};
use shoal_forge::{
    account::Account,
    conventions,
    items::{action::ActionRequests, attach::AttachRequests, create::CreateRequests},
};
use shoal_protocol::space_names::SpaceSelector;
use shoal_reviews::required::Gate;

mod open_issue;

impl Manager {
    /// Link the open PR of the workspace's branch, or open one as its agent
    /// account and link it; only a removal that starts meanwhile leaves it
    /// unlinked. A workspace process's new PR is recorded for the user's
    /// approval instead when its `pr_open` permission says so, or when
    /// `push_held` says the `push` permission held back the branch's push,
    /// which approving it runs first; a held push to an open PR is recorded
    /// on its own.
    pub async fn open_pull(
        &self,
        selector: &SpaceSelector,
        options: PullOptions,
        push_held: bool,
        caller: Option<&Caller>,
    ) -> Result<OpenOutcome> {
        let proposer = shoal_links::proposals::proposer(self, caller).await;
        // Cleanup holds the workspace's gate, so it cannot remove the
        // workspace between opening the PR and linking it.
        let (workspace, _gate) = self.lock_pr_gate(selector).await?;
        let branch = workspace.require_branch("shoal pr open")?.clone();
        self.verify_worktree(&workspace).await?;
        let head = shoal_links::pr::current_head(&workspace).await?;
        let push = match push_held {
            true => Some(
                self.links
                    .held_push(self, &workspace, proposer.as_ref(), caller)
                    .await?,
            ),
            false => None,
        };
        anyhow::ensure!(
            workspace.state.accepts_links(),
            "workspace cannot accept a PR in its current state"
        );
        let settings = self.workspace_settings(&workspace).await?;
        let required = &settings.pr.required_labels;
        let (forge, account) = self.links.workspace_forge_account(self, &workspace).await?;
        let head_repository = head_repository(&workspace.path, &branch, &forge).await?;
        let existing = forge
            .open_pull_for(&account, &branch, head_repository.as_deref())
            .await?;
        let (item, body, unfinished) = match existing {
            Some(number) => {
                let item = forge.read(&account, ItemKind::Pr, number).await?.item;
                // The PR is linked now; approving the push updates it.
                if let Some(push) = push {
                    self.links.link_open_pull(self, &workspace, &item).await?;
                    self.links
                        .ensure_workspace_not_pushing(self, &workspace)
                        .await?;
                    let proposed = self.links.propose_push(self, &workspace, push).await?;
                    return Ok(OpenOutcome::Proposed(proposed));
                }
                (item, None, None)
            }
            None => {
                self.reviews
                    .require(self, &workspace, Gate::Pr, &head, caller)
                    .await?;
                conventions::require(&forge, &account, ItemKind::Pr, required, &options.labels)
                    .await?;
                let pull = NewPull {
                    head_repository,
                    ..new_pull(&workspace, &forge, &account, options, settings.pr.draft).await?
                };
                // A PR whose branch is not pushed cannot open before the
                // push is approved, which then opens it too.
                let proposes = push.is_some() || self.proposes_pull(&workspace, caller).await?;
                if let Some(proposer) = proposer.filter(|_| proposes) {
                    forge.check_uploads(&account, &pull.attachments).await?;
                    self.links
                        .ensure_workspace_not_pushing(self, &workspace)
                        .await?;
                    let proposed = self
                        .links
                        .propose_pull(self, &workspace, pull, head, push, proposer)
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
            (self.links.keep_created(
                self,
                &workspace.id,
                ItemKind::Pr,
                &item,
                true,
                Assigner::of(caller),
            ))
            .await?
        } else {
            self.links.link_open_pull(self, &workspace, &item).await?;
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
    pub(crate) async fn check_pr_review_gate(
        &self,
        selector: &SpaceSelector,
        caller: Option<&Caller>,
    ) -> Result<()> {
        let workspace = self.workspace(selector).await?;
        let Some(branch) = workspace.branch().cloned() else {
            return Ok(());
        };
        let settings = self.workspace_settings(&workspace).await?;
        if !Gate::Pr.holds(&settings, caller) {
            return Ok(());
        }
        let (forge, account) = self.links.workspace_forge_account(self, &workspace).await?;
        let head_repository = head_repository(&workspace.path, &branch, &forge).await?;
        let existing = forge
            .open_pull_for(&account, &branch, head_repository.as_deref())
            .await?;
        match existing {
            Some(_) => Ok(()),
            None => {
                let head = shoal_links::pr::current_head(&workspace).await?;
                self.reviews
                    .require(self, &workspace, Gate::Pr, &head, caller)
                    .await
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
        (None, None, None) => crate::git::latest_subject(&workspace.path)
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
            crate::git::default_branch::resolve(
                &workspace.path,
                crate::git::default_branch::DefaultBranchLookup::Discover,
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

/// `owner/repository` of the fork the branch's push remote names, or `None`
/// when it pushes to the PR's own repository. A remote is named by where it
/// fetches from, as `origin` is, whatever its push URL.
pub(super) async fn head_repository(
    worktree: &std::path::Path,
    branch: &BranchName,
    forge: &ForgeRepo,
) -> Result<Option<String>> {
    let remote = crate::git::push_remote(worktree, branch).await?;
    let url = crate::git::remote_url(worktree, &remote).await?;
    let pushed = ForgeRepo::parse(url.trim())
        .with_context(|| format!("the branch's push remote {remote} is not a forge repository"))?;
    if pushed == *forge {
        return Ok(None);
    }
    anyhow::ensure!(
        pushed.host == forge.host,
        "the branch's push remote {remote} is on {}, so a PR on {} cannot have it as its head",
        pushed.host,
        forge.host
    );
    Ok(Some(pushed.path))
}

impl Manager {
    /// Whether a scoped `caller`'s `pr_open` permission has it propose the
    /// new PR of `workspace` instead of opening it; unscoped callers open.
    async fn proposes_pull(&self, workspace: &Workspace, caller: Option<&Caller>) -> Result<bool> {
        let Some(caller) = caller else {
            return Ok(false);
        };
        let permissions = self
            .caller_permissions(caller, Some(workspace.repository()))
            .await?;
        Ok(permissions.pr_open.value != Opening::Direct)
    }
}

#[cfg(test)]
mod tests;
