//! The daemon's answers to [`ItemMethod`] requests, who may send them, and what
//! each requires of the space it acts on.
use shoal_daemon_base::permitted_actions;
use std::sync::Arc;

use anyhow::{Context as _, Result, bail};

use super::super::{
    answer::Answer,
    manager::Manager,
    scope::{Caller, Scoped},
    target_checks::{Checked, TargetCheck},
};
use crate::{
    config::permissions::PermissionKey,
    forge::items::action::Action,
    protocol::{self, Body, ItemContext, ItemMethod, RepoTarget, ViewPiece},
};
use shoal_ids::repository_ids::RepositoryId;
use shoal_protocol::space_names::SpaceSelector;

#[cfg(test)]
mod tests;

pub(super) async fn handle(
    manager: &Arc<Manager>,
    method: ItemMethod,
    caller: Option<&Caller>,
) -> Result<Answer> {
    Ok(Answer::Body(match method {
        ItemMethod::PrWait {
            workspace,
            timeout_secs,
        } => {
            return Ok(Answer::wait(pr_updates(
                manager.clone(),
                workspace,
                timeout_secs,
            )));
        }
        ItemMethod::WatchItems {
            space: workspace,
            selection,
            repository,
            timeout_secs,
        } => {
            return Ok(Answer::wait(item_updates(
                manager.clone(),
                workspace,
                selection,
                repository,
                timeout_secs,
            )));
        }
        ItemMethod::ListItems {
            target,
            kind,
            search,
        } => {
            let listed = manager
                .links
                .list_items(&**manager, &target, kind, &search, caller)
                .await;
            return Ok(Answer::pages(listed, Body::Items));
        }
        ItemMethod::Labels { target } => {
            let labels = manager.links.labels(&**manager, &target, caller).await;
            return Ok(Answer::pages(labels, Body::Labels));
        }
        ItemMethod::Reviewers { target, item } => {
            let reviewers = manager
                .links
                .reviewers(&**manager, &target, item, caller)
                .await;
            return Ok(Answer::pages(reviewers, Body::Reviewers));
        }
        ItemMethod::Capabilities { target } => Body::Capabilities(
            manager
                .links
                .capabilities(&**manager, &target, caller)
                .await?,
        ),
        ItemMethod::Reactions { target } => {
            Body::Reactions(manager.links.reactions(&**manager, &target, caller).await?)
        }
        ItemMethod::ReviewThread {
            target,
            item,
            comment,
        } => Body::Thread(
            manager
                .links
                .review_thread(&**manager, &target, item, comment, caller)
                .await?,
        ),
        ItemMethod::NumberKind { target, number } => Body::LinkKind(
            manager
                .links
                .number_kind(&**manager, &target, number, caller)
                .await?,
        ),
        ItemMethod::IssueDetails {
            context,
            item,
            closed,
        } => {
            let issue = manager
                .links
                .issue_details(&**manager, &context, &item, closed, caller)
                .await
                .and_then(|issue| json_pieces(&issue));
            return Ok(Answer::pages(issue, Body::ViewPieces));
        }
        ItemMethod::IssueBlockers { context, item } => {
            let blockers = manager
                .links
                .issue_blockers(&**manager, &context, &item, caller)
                .await;
            return Ok(Answer::pages(blockers, Body::Blockers));
        }
        ItemMethod::PullRequest { context, item } => Body::PullRequest(
            manager
                .links
                .pull_request(&**manager, &context, &item, caller)
                .await?,
        ),
        ItemMethod::ViewItems {
            target,
            selection,
            comments,
        } => {
            let viewed = manager
                .links
                .view_items(&**manager, &target, &selection, comments, caller)
                .await
                .and_then(|viewed| json_pieces(&viewed));
            return Ok(Answer::pages(viewed, Body::ViewPieces));
        }
        ItemMethod::ForgeApi {
            target,
            request: raw,
        } => {
            let response = manager
                .links
                .forge_api(&**manager, &target, &raw, caller)
                .await
                .map(|response| protocol::text_pieces(&response.to_string()));
            return Ok(Answer::pages(response, Body::ApiResponse));
        }
        ItemMethod::ForgeDownload { target, url } => {
            let download = manager
                .links
                .forge_download(&**manager, &target, &url, caller)
                .await
                .map(protocol::DownloadPiece::split);
            return Ok(Answer::pages(download, Body::Downloaded));
        }
        ItemMethod::ItemAction {
            target,
            kind,
            item,
            mut action,
            confirmed,
        } => {
            // A scoped caller's merge passed its gate in its scope, before
            // its permission.
            if caller.is_none() {
                (manager.links.gate_pull_merge(
                    &**manager,
                    &target,
                    item.clone(),
                    &mut action,
                    None,
                ))
                .await?;
            }
            Body::Item(match confirmed {
                Some(confirmed) => {
                    (manager
                        .links
                        .confirmed_merge(&**manager, &target, item, action, &confirmed, caller))
                    .await?
                }
                None => {
                    (manager
                        .links
                        .item_action(&**manager, &target, kind, item, action, caller))
                    .await?
                }
            })
        }
        ItemMethod::MergeWarning { target, item } => {
            // Before the question, which a merge its gate refuses never asks.
            (manager.links.require_pull_review(
                &**manager,
                &target,
                item.clone(),
                None,
                None,
                caller,
            ))
            .await?;
            Body::MergeWarning(
                manager
                    .links
                    .merge_warning(&**manager, &target, item)
                    .await?,
            )
        }
        ItemMethod::OpenPull {
            workspace,
            options,
            push_held,
        } => Body::Opened(
            manager
                .links
                .open_pull(&**manager, &workspace, options, push_held, caller)
                .await?,
        ),
        ItemMethod::RevertPull {
            space: workspace,
            item,
            options,
        } => Body::Opened(
            manager
                .links
                .revert_pull(&**manager, &workspace, item, options, caller)
                .await?,
        ),
        ItemMethod::MergedPull {
            space: workspace,
            item,
        } => Body::MergedPull(
            manager
                .links
                .merged_pull(&**manager, &workspace, item)
                .await?,
        ),
        ItemMethod::OpenIssue {
            target,
            issue,
            link,
        } => Body::Opened(
            manager
                .links
                .open_issue(&**manager, &target, issue, link, caller)
                .await?,
        ),
        ItemMethod::SelectItems {
            target,
            selection,
            elsewhere,
        } => Body::SelectedItems(
            manager
                .links
                .select_context_items(&**manager, &target, &selection, elsewhere, caller)
                .await?,
        ),
        ItemMethod::LinkItem {
            space: workspace,
            kind,
            item,
            relation,
            title,
        } => Body::Linked(
            manager
                .links
                .link_item(&**manager, &workspace, kind, &item, relation, title, caller)
                .await?,
        ),
        ItemMethod::RelinkSpace { space, links } => {
            let id = manager
                .links
                .link_space(&**manager, &space)
                .await?
                .id
                .to_owned();
            let warnings = manager
                .links
                .relink_space(&**manager, &id, &links, caller)
                .await;
            Body::Linked(shoal_protocol::links::Linked {
                title_error: None,
                warnings,
            })
        }
        ItemMethod::UnlinkItems {
            space: workspace,
            relation,
            kind,
            item,
        } => Body::Unlinked(
            manager
                .links
                .unlink_items(&**manager, &workspace, relation, kind, item.as_ref())
                .await?,
        ),
        ItemMethod::SetPr {
            space: workspace,
            action,
        } => {
            manager.links.set_pr(&**manager, &workspace, action).await?;
            Body::Ok
        }
    }))
}

impl Checked for ItemMethod {
    fn target_check(&self) -> Option<TargetCheck> {
        match self {
            Self::SetPr { .. }
            | Self::PrWait { .. }
            | Self::WatchItems { .. }
            | Self::RevertPull { .. }
            | Self::MergedPull { .. }
            | Self::OpenIssue { .. }
            | Self::MergeWarning { .. }
            | Self::ItemAction { .. }
            | Self::ListItems { .. }
            | Self::Labels { .. }
            | Self::Reviewers { .. }
            | Self::ViewItems { .. }
            | Self::ForgeApi { .. }
            | Self::ForgeDownload { .. }
            | Self::Capabilities { .. }
            | Self::Reactions { .. }
            | Self::ReviewThread { .. }
            | Self::NumberKind { .. }
            | Self::IssueDetails { .. }
            | Self::IssueBlockers { .. }
            | Self::PullRequest { .. }
            | Self::SelectItems { .. } => Some(TargetCheck::Space),
            Self::LinkItem { .. } | Self::RelinkSpace { .. } | Self::UnlinkItems { .. } => {
                Some(TargetCheck::LinkingSpace)
            }
            Self::OpenPull { .. } => Some(TargetCheck::LinkingWorkspace),
        }
    }
}

impl Scoped for ItemMethod {
    async fn scope(
        &mut self,
        manager: &Manager,
        caller: &Caller,
    ) -> Result<Option<&mut SpaceSelector>> {
        Ok(match self {
            ItemMethod::WatchItems {
                space: workspace,
                selection,
                ..
            } => {
                manager
                    .links
                    .ensure_own_repository_run(manager, workspace, selection)
                    .await?;
                Some(workspace)
            }
            // Merging a PR is the `merge` permission's, and the merge then
            // names the head its request binds, once its review gate passes
            // for that head.
            ItemMethod::ItemAction {
                target,
                item,
                action:
                    action @ (Action::Merge { .. } | Action::AutoMerge { .. } | Action::CancelAutoMerge),
                ..
            } => {
                let context = own_context(caller, target);
                let repository = reached_change_context(manager, caller, context).await?;
                let subject =
                    manager
                        .links
                        .bind_merge(manager, target, item.clone(), action, caller);
                let subject = subject.await?;
                (manager.links.gate_pull_merge(
                    manager,
                    target,
                    item.clone(),
                    action,
                    Some(caller),
                ))
                .await?;
                let merge = permitted_actions::Action::new(PermissionKey::Merge, subject)
                    .in_repository(repository);
                manager.authorize_action(Some(caller), merge).await?;
                Some(target_selector(target)?)
            }
            // Approving and requesting changes are the `pr_review`
            // permission's.
            ItemMethod::ItemAction {
                target,
                item,
                action: Action::Review(review),
                ..
            } => {
                if review.has_verdict() {
                    let context = own_context(caller, target);
                    let repository = reached_change_context(manager, caller, context).await?;
                    let key = PermissionKey::PrReview;
                    manager
                        .refuse_denied(caller, key, Some(&repository))
                        .await?;
                    let verdict = manager.links.verdict_action(
                        manager,
                        target,
                        item.clone(),
                        review,
                        repository,
                        caller,
                    );
                    manager
                        .authorize_action(Some(caller), verdict.await?)
                        .await?;
                }
                let context = own_context(caller, target);
                Some(change_context_space(manager, caller, context).await?)
            }
            // Reads go out as the agent account wherever they run.
            ItemMethod::ListItems { target, .. }
            | ItemMethod::Labels { target }
            | ItemMethod::Capabilities { target }
            | ItemMethod::Reactions { target }
            | ItemMethod::MergeWarning { target, .. }
            | ItemMethod::Reviewers { target, .. }
            | ItemMethod::ReviewThread { target, .. }
            | ItemMethod::NumberKind { target, .. }
            | ItemMethod::SelectItems { target, .. }
            | ItemMethod::ViewItems { target, .. } => {
                read_context_space(manager, caller, own_context(caller, target)).await?
            }
            ItemMethod::IssueDetails { context, .. }
            | ItemMethod::IssueBlockers { context, .. }
            | ItemMethod::PullRequest { context, .. } => {
                read_context_space(manager, caller, context).await?
            }
            ItemMethod::OpenIssue { target, .. }
            | ItemMethod::ForgeApi { target, .. }
            | ItemMethod::ForgeDownload { target, .. }
            | ItemMethod::ItemAction {
                target,
                action:
                    Action::Edit(_)
                    | Action::Comment { .. }
                    | Action::EditComment(_)
                    | Action::Thread(_)
                    | Action::React(_)
                    | Action::Close
                    | Action::Reopen
                    | Action::Update { .. },
                ..
            } => {
                let context = own_context(caller, target);
                Some(change_context_space(manager, caller, context).await?)
            }
            ItemMethod::LinkItem {
                space: workspace, ..
            }
            | ItemMethod::RelinkSpace {
                space: workspace, ..
            }
            | ItemMethod::UnlinkItems {
                space: workspace, ..
            }
            | ItemMethod::SetPr {
                space: workspace, ..
            }
            | ItemMethod::PrWait { workspace, .. }
            | ItemMethod::OpenPull { workspace, .. }
            | ItemMethod::RevertPull {
                space: workspace, ..
            }
            | ItemMethod::MergedPull {
                space: workspace, ..
            } => Some(workspace),
        })
    }
}

/// Where `target` comes from: for a scoped `caller`, its own space unless
/// `target` names another.
pub(super) fn own_context<'a>(caller: &Caller, target: &'a mut RepoTarget) -> &'a mut ItemContext {
    (target.context).get_or_insert_with(|| ItemContext::space(SpaceSelector::id(&caller.space_id)))
}

/// The space `target` comes from, which [`context_selector`] names.
pub(super) fn target_selector(target: &mut RepoTarget) -> Result<&mut SpaceSelector> {
    let context = target.context.as_mut();
    context_selector(context.context("scoped processes act on items from their space")?)
}

/// The space `context` names: a scoped process acts on items from its
/// space, never from a registered checkout.
pub(super) fn context_selector(context: &mut ItemContext) -> Result<&mut SpaceSelector> {
    match context {
        ItemContext::Space { space, .. } => Ok(space),
        ItemContext::Checkout(_) => {
            bail!("scoped processes act on items from their space, not from a registered checkout")
        }
    }
}

/// The space a change from `context` is made by, which [`context_selector`]
/// names; a caller whose scope reaches every space, which has every
/// registered repository for its own, changes the items of a registered
/// checkout as its own space acting there, under its permissions in that
/// repository's config.
pub(super) async fn change_context_space<'a>(
    manager: &Manager,
    caller: &Caller,
    context: &'a mut ItemContext,
) -> Result<&'a mut SpaceSelector> {
    if let ItemContext::Checkout(repository) = context
        && manager.reach_root(Some(caller)).await?.is_none()
    {
        *context = ItemContext::Space {
            space: SpaceSelector::id(&caller.space_id),
            repository: Some(repository.clone()),
        };
    }
    context_selector(context)
}

/// The repository a change from `context` is made in, once
/// [`change_context_space`] names the space that makes it, which the
/// caller's scope must reach before the change asks approval: a chat
/// without a repository resolves its permissions in it.
pub(super) async fn reached_change_context(
    manager: &Manager,
    caller: &Caller,
    context: &mut ItemContext,
) -> Result<RepositoryId> {
    let space = change_context_space(manager, caller, context).await?;
    manager.ensure_selected_reached(caller, Some(space)).await?;
    let space = manager.links.context_space(manager, context).await?;
    Ok(space.repository_id().clone())
}

/// The space a read from `context` runs in, which [`context_selector`]
/// names; a caller whose scope reaches every space, which has every
/// registered repository for its own, also reads from a registered
/// checkout, which names none.
pub(super) async fn read_context_space<'a>(
    manager: &Manager,
    caller: &Caller,
    context: &'a mut ItemContext,
) -> Result<Option<&'a mut SpaceSelector>> {
    if let ItemContext::Checkout(_) = context
        && manager.reach_root(Some(caller)).await?.is_none()
    {
        return Ok(None);
    }
    context_selector(context).map(Some)
}

/// `value` as pieces of its JSON, each fitting a frame: one PR's discussion,
/// one issue's description or one job's log may outgrow a frame of its own.
pub(super) fn json_pieces(value: &impl serde::Serialize) -> Result<Vec<ViewPiece>> {
    let json = serde_json::to_string(value)?;
    Ok(protocol::text_pieces(&json)
        .into_iter()
        .map(ViewPiece)
        .collect())
}

/// The workspace's next PR updates.
async fn pr_updates(
    manager: Arc<Manager>,
    workspace: SpaceSelector,
    timeout_secs: Option<u64>,
) -> Result<Body> {
    let selection = crate::forge::items::link::Selection {
        kind: Some(crate::forge::items::link::LinkKind::Pr),
        input: None,
    };
    item_updates(manager, workspace, selection, None, timeout_secs).await
}

/// The workspace's next updates of the selected items, in the registered
/// `repository` for a chat without a repository.
async fn item_updates(
    manager: Arc<Manager>,
    workspace: SpaceSelector,
    selection: crate::forge::items::link::Selection,
    repository: Option<RepositoryId>,
    timeout_secs: Option<u64>,
) -> Result<Body> {
    let updates = manager
        .links
        .wait_items(
            &*manager,
            &workspace,
            &selection,
            repository.as_ref(),
            timeout_secs,
        )
        .await?;
    Ok(Body::PrUpdates(updates))
}
