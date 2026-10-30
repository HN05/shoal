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
    protocol::{self, Body, ItemContext, ItemMethod},
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
            workspace,
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
        ItemMethod::ForgeApi {
            workspace,
            request: raw,
        } => {
            let response = manager
                .forge_api(&workspace, &raw, caller.is_some())
                .await
                .map(|response| protocol::text_pieces(&response.to_string()));
            return Ok(Answer::pages(response, Body::ApiResponse));
        }
        ItemMethod::ItemAction {
            context,
            kind,
            item,
            action,
        } => Body::Item(manager.item_action(&context, kind, item, action).await?),
        ItemMethod::OpenPull { workspace, options } => {
            Body::Opened(manager.open_pull(&workspace, options, caller).await?)
        }
        ItemMethod::RevertPull {
            workspace,
            item,
            options,
        } => Body::Opened(
            manager
                .revert_pull(&workspace, item, options, caller)
                .await?,
        ),
        ItemMethod::MergedPull { workspace, item } => {
            Body::MergedPull(manager.merged_pull(&workspace, item).await?)
        }
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
            workspace,
            selection,
        } => Body::SelectedItems(manager.select_items(&workspace, &selection).await?),
        ItemMethod::SetIssue {
            workspace,
            url,
            title,
        } => {
            manager.set_issue(&workspace, &url, title).await?;
            Body::Ok
        }
        ItemMethod::LinkItem {
            workspace,
            kind,
            url,
            relation,
            title,
        } => {
            manager
                .link_item(&workspace, kind, &url, relation, title)
                .await?;
            Body::Ok
        }
        ItemMethod::UnlinkItems {
            workspace,
            relation,
            kind,
            url,
        } => {
            manager
                .unlink_items(&workspace, relation, kind, url.as_deref())
                .await?;
            Body::Ok
        }
        ItemMethod::SetPr { workspace, action } => {
            manager.set_pr(&workspace, action).await?;
            Body::Ok
        }
        ItemMethod::ForgeType { host, scheme } => {
            Body::ForgeType(manager.requested_forge_type(&host, &scheme).await?)
        }
    }))
}

impl Scoped for ItemMethod {
    async fn scope(&mut self, _: &Manager, _: &Caller) -> Result<Option<&mut String>> {
        Ok(match self {
            ItemMethod::ForgeType { .. } => None,
            ItemMethod::ItemAction {
                action: Action::Merge { .. } | Action::AutoMerge { .. } | Action::CancelAutoMerge,
                ..
            } => bail!("merging is the user's decision; ask them with shoal alerts send"),
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
            ItemMethod::OpenIssue { context, .. }
            | ItemMethod::ListItems { context, .. }
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
            } => Some(context_workspace(context)?),
            ItemMethod::SetIssue { workspace, .. }
            | ItemMethod::LinkItem { workspace, .. }
            | ItemMethod::UnlinkItems { workspace, .. }
            | ItemMethod::SetPr { workspace, .. }
            | ItemMethod::PrWait { workspace, .. }
            | ItemMethod::WatchItems { workspace, .. }
            | ItemMethod::OpenPull { workspace, .. }
            | ItemMethod::RevertPull { workspace, .. }
            | ItemMethod::MergedPull { workspace, .. }
            | ItemMethod::ForgeApi { workspace, .. }
            | ItemMethod::SelectItems { workspace, .. } => Some(workspace),
        })
    }
}

/// The workspace `context` names: a workspace process acts on items from its
/// workspace, never from a registered checkout.
fn context_workspace(context: &mut ItemContext) -> Result<&mut String> {
    match context {
        ItemContext::Workspace(workspace) => Ok(workspace),
        ItemContext::Checkout(_) => bail!(
            "workspace processes act on items from their workspace, not from a registered checkout"
        ),
    }
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
