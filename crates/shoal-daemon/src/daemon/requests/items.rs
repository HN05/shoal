//! The daemon's answers to [`ItemMethod`] requests, and who may send them.
use std::sync::Arc;

use anyhow::{Result, bail};

use super::super::{
    answer::Answer,
    permissions,
    scope::{Caller, Scoped},
    workspace::Manager,
};
use crate::client::repository_ids::RepositoryId;
use crate::client::space_names::SpaceSelector;
use crate::{
    config::permissions::PermissionKey,
    forge::items::action::Action,
    protocol::{self, Body, ItemContext, ItemMethod, ViewPiece},
};

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
            context,
            kind,
            search,
            repository,
        } => {
            let listed = manager
                .list_items(&context, kind, &search, repository.as_deref(), caller)
                .await;
            return Ok(Answer::pages(listed, Body::Items));
        }
        ItemMethod::Labels {
            context,
            repository,
        } => {
            let labels = manager
                .labels(&context, repository.as_deref(), caller)
                .await;
            return Ok(Answer::pages(labels, Body::Labels));
        }
        ItemMethod::Reviewers { context, item } => {
            let reviewers = manager.reviewers(&context, item, caller).await;
            return Ok(Answer::pages(reviewers, Body::Reviewers));
        }
        ItemMethod::Capabilities {
            context,
            repository,
        } => Body::Capabilities(
            manager
                .capabilities(&context, repository.as_deref(), caller)
                .await?,
        ),
        ItemMethod::Reactions {
            context,
            repository,
        } => Body::Reactions(
            manager
                .reactions(&context, repository.as_deref(), caller)
                .await?,
        ),
        ItemMethod::ReviewThread {
            context,
            item,
            comment,
        } => Body::Thread(
            manager
                .review_thread(&context, item, comment, caller)
                .await?,
        ),
        ItemMethod::NumberKind { context, number } => {
            Body::LinkKind(manager.number_kind(&context, number, caller).await?)
        }
        ItemMethod::IssueDetails {
            context,
            item,
            closed,
        } => {
            let issue = manager
                .issue_details(&context, &item, closed, caller)
                .await
                .and_then(|issue| json_pieces(&issue));
            return Ok(Answer::pages(issue, Body::ViewPieces));
        }
        ItemMethod::IssueBlockers { context, item } => {
            let blockers = manager.issue_blockers(&context, &item, caller).await;
            return Ok(Answer::pages(blockers, Body::Blockers));
        }
        ItemMethod::PullRequest { context, item } => {
            Body::PullRequest(manager.pull_request(&context, &item, caller).await?)
        }
        ItemMethod::ViewItems {
            context,
            selection,
            comments,
        } => {
            let viewed = manager
                .view_items(&context, &selection, comments, caller)
                .await
                .and_then(|viewed| json_pieces(&viewed));
            return Ok(Answer::pages(viewed, Body::ViewPieces));
        }
        ItemMethod::ForgeApi {
            context,
            request: raw,
            repository,
        } => {
            let response = manager
                .forge_api(&context, &raw, repository.as_deref(), caller)
                .await
                .map(|response| protocol::text_pieces(&response.to_string()));
            return Ok(Answer::pages(response, Body::ApiResponse));
        }
        ItemMethod::ForgeDownload {
            context,
            url,
            repository,
        } => {
            let download = manager
                .forge_download(&context, &url, repository.as_deref(), caller)
                .await
                .map(protocol::DownloadPiece::split);
            return Ok(Answer::pages(download, Body::Downloaded));
        }
        ItemMethod::ItemAction {
            context,
            kind,
            item,
            action,
            confirmed,
        } => Body::Item(match confirmed {
            Some(confirmed) => {
                (manager.confirmed_merge(&context, item, action, &confirmed, caller)).await?
            }
            None => {
                manager
                    .item_action(&context, kind, item, action, caller)
                    .await?
            }
        }),
        ItemMethod::MergeWarning { context, item } => {
            Body::MergeWarning(manager.merge_warning(&context, item).await?)
        }
        ItemMethod::OpenPull {
            workspace,
            options,
            push_held,
        } => Body::Opened(
            manager
                .open_pull(&workspace, options, push_held, caller)
                .await?,
        ),
        ItemMethod::RevertPull {
            space: workspace,
            item,
            options,
        } => Body::Opened(
            manager
                .revert_pull(&workspace, item, options, caller)
                .await?,
        ),
        ItemMethod::MergedPull {
            space: workspace,
            item,
        } => Body::MergedPull(manager.merged_pull(&workspace, item).await?),
        ItemMethod::OpenIssue {
            context,
            issue,
            repository,
            link,
        } => Body::Opened(
            manager
                .open_issue(&context, issue, repository, link, caller)
                .await?,
        ),
        ItemMethod::SelectItems {
            context,
            selection,
            elsewhere,
        } => Body::SelectedItems(
            manager
                .select_context_items(&context, &selection, elsewhere, caller)
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
                .link_item(&workspace, kind, &item, relation, title, caller)
                .await?,
        ),
        ItemMethod::RelinkSpace { space, links } => {
            let id = manager.link_space(&space).await?.id().to_owned();
            let warnings = manager.relink_space(&id, &links, caller).await;
            Body::Linked(crate::client::links::Linked {
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
                .unlink_items(&workspace, relation, kind, item.as_ref())
                .await?,
        ),
        ItemMethod::SetPr {
            space: workspace,
            action,
        } => {
            manager.set_pr(&workspace, action).await?;
            Body::Ok
        }
    }))
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
                    .ensure_own_repository_run(workspace, selection)
                    .await?;
                Some(workspace)
            }
            // Merging a PR and deleting its branch are the `merge`
            // permission's.
            ItemMethod::ItemAction {
                context,
                item,
                action:
                    action @ (Action::Merge { .. }
                    | Action::AutoMerge { .. }
                    | Action::CancelAutoMerge
                    | Action::DeleteBranch),
                ..
            } => {
                let repository = reached_change_context(manager, caller, context).await?;
                let subject = manager.merge_subject(context, item.clone(), action);
                let merge = permissions::Action::new(PermissionKey::Merge, subject.await?)
                    .in_repository(repository);
                manager.authorize_action(Some(caller), merge).await?;
                Some(context_workspace(context)?)
            }
            ItemMethod::MergeWarning { context, .. } => {
                read_context_space(manager, caller, context).await?
            }
            // Approving and requesting changes are the `pr_review`
            // permission's.
            ItemMethod::ItemAction {
                context,
                item,
                action: Action::Review(review),
                ..
            } => {
                if review.has_verdict() {
                    let repository = reached_change_context(manager, caller, context).await?;
                    let verdict = manager.verdict_action(context, item.clone(), review, repository);
                    manager
                        .authorize_action(Some(caller), verdict.await?)
                        .await?;
                }
                Some(change_context_space(manager, caller, context).await?)
            }
            // Reads go out as the agent account wherever they run.
            ItemMethod::ListItems { context, .. }
            | ItemMethod::Labels { context, .. }
            | ItemMethod::Reviewers { context, .. }
            | ItemMethod::Capabilities { context, .. }
            | ItemMethod::Reactions { context, .. }
            | ItemMethod::ReviewThread { context, .. }
            | ItemMethod::NumberKind { context, .. }
            | ItemMethod::IssueDetails { context, .. }
            | ItemMethod::IssueBlockers { context, .. }
            | ItemMethod::PullRequest { context, .. }
            | ItemMethod::SelectItems { context, .. }
            | ItemMethod::ViewItems { context, .. } => {
                read_context_space(manager, caller, context).await?
            }
            ItemMethod::OpenIssue { context, .. }
            | ItemMethod::ForgeApi { context, .. }
            | ItemMethod::ForgeDownload { context, .. }
            | ItemMethod::ItemAction {
                context,
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
            } => Some(change_context_space(manager, caller, context).await?),
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

/// The workspace `context` names: a workspace process acts on items from its
/// workspace, never from a registered checkout.
pub(super) fn context_workspace(context: &mut ItemContext) -> Result<&mut SpaceSelector> {
    match context {
        ItemContext::Workspace(space)
        | ItemContext::Chat(space)
        | ItemContext::ChatIn { chat: space, .. } => Ok(space),
        ItemContext::Checkout(_) => {
            bail!("scoped processes act on items from their space, not from a registered checkout")
        }
    }
}

/// The space a change from `context` is made by, which [`context_workspace`]
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
        *context = ItemContext::ChatIn {
            chat: SpaceSelector::id(&caller.space_id),
            repository: repository.clone(),
        };
    }
    context_workspace(context)
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
    let space = manager.context_space(context).await?;
    Ok(space.repository_id().clone())
}

/// The space a read from `context` runs in, which [`context_workspace`]
/// names; a caller whose scope reaches every space, which has every
/// registered repository for its own, also reads from a registered
/// checkout, which names none.
async fn read_context_space<'a>(
    manager: &Manager,
    caller: &Caller,
    context: &'a mut ItemContext,
) -> Result<Option<&'a mut SpaceSelector>> {
    if let ItemContext::Checkout(_) = context
        && manager.reach_root(Some(caller)).await?.is_none()
    {
        return Ok(None);
    }
    context_workspace(context).map(Some)
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
        .wait_items(&workspace, &selection, repository.as_ref(), timeout_secs)
        .await?;
    Ok(Body::PrUpdates(updates))
}
