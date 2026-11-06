//! The daemon's answers to [`ItemMethod`] requests, and who may send them.
use std::sync::Arc;

use anyhow::{Result, bail};

use super::super::{
    answer::Answer,
    scope::{Caller, Scoped},
    workspace::Manager,
};
use crate::{
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
            timeout_secs,
        } => {
            return Ok(Answer::wait(item_updates(
                manager.clone(),
                workspace,
                selection,
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
                .list_items(
                    &context,
                    kind,
                    &search,
                    repository.as_deref(),
                    caller.is_some(),
                )
                .await;
            return Ok(Answer::pages(listed, Body::Items));
        }
        ItemMethod::Labels {
            context,
            repository,
        } => {
            let labels = manager
                .labels(&context, repository.as_deref(), caller.is_some())
                .await;
            return Ok(Answer::pages(labels, Body::Labels));
        }
        ItemMethod::Reviewers { context, item } => {
            let reviewers = manager.reviewers(&context, item, caller.is_some()).await;
            return Ok(Answer::pages(reviewers, Body::Reviewers));
        }
        ItemMethod::Capabilities {
            context,
            repository,
        } => Body::Capabilities(
            manager
                .capabilities(&context, repository.as_deref(), caller.is_some())
                .await?,
        ),
        ItemMethod::Reactions {
            context,
            repository,
        } => Body::Reactions(
            manager
                .reactions(&context, repository.as_deref(), caller.is_some())
                .await?,
        ),
        ItemMethod::ReviewThread {
            context,
            item,
            comment,
        } => Body::Thread(
            manager
                .review_thread(&context, item, comment, caller.is_some())
                .await?,
        ),
        ItemMethod::NumberKind { context, number } => Body::LinkKind(
            manager
                .number_kind(&context, number, caller.is_some())
                .await?,
        ),
        ItemMethod::IssueDetails { context, item } => {
            let issue = manager
                .issue_details(&context, &item, caller.is_some())
                .await
                .and_then(|issue| json_pieces(&issue));
            return Ok(Answer::pages(issue, Body::ViewPieces));
        }
        ItemMethod::IssueBlockers { context, item } => {
            let blockers = manager
                .issue_blockers(&context, &item, caller.is_some())
                .await;
            return Ok(Answer::pages(blockers, Body::Dependencies));
        }
        ItemMethod::PullRequest { context, item } => Body::PullRequest(
            manager
                .pull_request(&context, &item, caller.is_some())
                .await?,
        ),
        ItemMethod::ViewItems {
            context,
            selection,
            comments,
        } => {
            let viewed = manager
                .view_items(&context, &selection, comments, caller.is_some())
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
                .forge_api(&context, &raw, repository.as_deref(), caller.is_some())
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
                .forge_download(&context, &url, repository.as_deref(), caller.is_some())
                .await
                .map(protocol::DownloadPiece::split);
            return Ok(Answer::pages(download, Body::Downloaded));
        }
        ItemMethod::ItemAction {
            context,
            kind,
            item,
            action,
        } => Body::Item(manager.item_action(&context, kind, item, action).await?),
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
                .select_context_items(&context, &selection, elsewhere, caller.is_some())
                .await?,
        ),
        ItemMethod::SetIssue {
            space: workspace,
            url,
            title,
        } => {
            manager.set_issue(&workspace, &url, title).await?;
            Body::Ok
        }
        ItemMethod::LinkItem {
            space: workspace,
            kind,
            url,
            relation,
            title,
        } => {
            manager
                .link_item(&workspace, kind, &url, relation, title, caller.is_some())
                .await?;
            Body::Ok
        }
        ItemMethod::UnlinkItems {
            space: workspace,
            relation,
            kind,
            url,
        } => {
            manager
                .unlink_items(&workspace, relation, kind, url.as_deref())
                .await?;
            Body::Ok
        }
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
    async fn scope(&mut self, manager: &Manager, caller: &Caller) -> Result<Option<&mut String>> {
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
            ItemMethod::ItemAction {
                action: Action::Merge { .. } | Action::AutoMerge { .. } | Action::CancelAutoMerge,
                ..
            }
            | ItemMethod::MergeWarning { .. } => {
                bail!("merging is the user's decision; ask them with shoal alerts send")
            }
            ItemMethod::ItemAction {
                action: Action::DeleteBranch,
                ..
            } => bail!(
                "deleting a PR's branch is the user's decision; ask them with shoal alerts send"
            ),
            ItemMethod::ItemAction {
                action: Action::Review(review),
                ..
            } if review.has_verdict() => bail!(
                "approving and requesting changes are the user's decision; ask them with shoal alerts send"
            ),
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
                    | Action::Review(_)
                    | Action::React(_)
                    | Action::Close
                    | Action::Reopen
                    | Action::Update { .. },
                ..
            } => Some(change_context_space(manager, caller, context).await?),
            ItemMethod::SetIssue {
                space: workspace, ..
            }
            | ItemMethod::LinkItem {
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
pub(super) fn context_workspace(context: &mut ItemContext) -> Result<&mut String> {
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
/// checkout as its own space acting there, under that repository's
/// `[permissions]`.
pub(super) async fn change_context_space<'a>(
    manager: &Manager,
    caller: &Caller,
    context: &'a mut ItemContext,
) -> Result<&'a mut String> {
    if let ItemContext::Checkout(repository) = context
        && manager.reach_root(Some(caller)).await?.is_none()
    {
        *context = ItemContext::ChatIn {
            chat: caller.space_id.clone(),
            repository: repository.clone(),
        };
    }
    context_workspace(context)
}

/// The space a read from `context` runs in, which [`context_workspace`]
/// names; a caller whose scope reaches every space, which has every
/// registered repository for its own, also reads from a registered
/// checkout, which names none.
async fn read_context_space<'a>(
    manager: &Manager,
    caller: &Caller,
    context: &'a mut ItemContext,
) -> Result<Option<&'a mut String>> {
    if let ItemContext::Checkout(_) = context
        && manager.reach_root(Some(caller)).await?.is_none()
    {
        return Ok(None);
    }
    context_workspace(context).map(Some)
}

/// `value` as pieces of its JSON, each fitting a frame: one PR's discussion
/// or one issue's description may outgrow a frame of its own.
fn json_pieces(value: &impl serde::Serialize) -> Result<Vec<ViewPiece>> {
    let json = serde_json::to_string(value)?;
    Ok(protocol::text_pieces(&json)
        .into_iter()
        .map(ViewPiece)
        .collect())
}

/// The workspace's next PR updates.
async fn pr_updates(
    manager: Arc<Manager>,
    workspace: String,
    timeout_secs: Option<u64>,
) -> Result<Body> {
    let selection = crate::forge::items::link::Selection {
        kind: Some(crate::forge::items::link::LinkKind::Pr),
        input: None,
    };
    item_updates(manager, workspace, selection, timeout_secs).await
}

/// The workspace's next updates of the selected items.
async fn item_updates(
    manager: Arc<Manager>,
    workspace: String,
    selection: crate::forge::items::link::Selection,
    timeout_secs: Option<u64>,
) -> Result<Body> {
    let updates = manager
        .wait_items(&workspace, &selection, timeout_secs)
        .await?;
    Ok(Body::PrUpdates(updates))
}
