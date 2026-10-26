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
    protocol::{self, Body, ItemMethod},
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
            workspace,
            kind,
            search,
        } => {
            let listed = manager
                .list_items(&workspace, kind, &search, caller.is_some())
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
            workspace,
            kind,
            item,
            action,
        } => Body::Item(manager.item_action(&workspace, kind, item, action).await?),
        ItemMethod::OpenPull { workspace, options } => {
            let proposer = manager.proposer(caller).await;
            Body::Opened(manager.open_pull(&workspace, options, proposer).await?)
        }
        ItemMethod::OpenIssue {
            workspace,
            issue,
            repository,
            link,
        } => {
            let proposer = manager.proposer(caller).await;
            Body::Opened(
                manager
                    .open_issue(&workspace, issue, repository, link, proposer)
                    .await?,
            )
        }
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
                action: Action::Merge { .. },
                ..
            } => bail!("merging is the user's decision; ask them with shoal alerts send"),
            ItemMethod::SetIssue { workspace, .. }
            | ItemMethod::LinkItem { workspace, .. }
            | ItemMethod::UnlinkItems { workspace, .. }
            | ItemMethod::SetPr { workspace, .. }
            | ItemMethod::PrWait { workspace, .. }
            | ItemMethod::WatchItems { workspace, .. }
            | ItemMethod::OpenPull { workspace, .. }
            | ItemMethod::OpenIssue { workspace, .. }
            | ItemMethod::ItemAction {
                workspace,
                action:
                    Action::Edit(_)
                    | Action::Comment { .. }
                    | Action::Thread(_)
                    | Action::Close
                    | Action::Reopen,
                ..
            }
            | ItemMethod::ListItems { workspace, .. }
            | ItemMethod::ForgeApi { workspace, .. }
            | ItemMethod::SelectItems { workspace, .. } => Some(workspace),
        })
    }
}

/// The workspace's next PR updates.
async fn pr_updates(
    manager: Arc<Manager>,
    workspace: String,
    timeout_secs: Option<u64>,
) -> Result<Body> {
    let selection = crate::forge::items::link::Selection {
        kind: Some(crate::forge::items::link::ItemKind::Pr),
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
