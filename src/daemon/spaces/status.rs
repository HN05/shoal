//! A space's status: the workspace's worktree status or the chat, with the
//! state of the items it links and what awaits the user.
use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::{
    client::links::{issue, pr::Registration},
    daemon::{
        approvals, forge_hosts::ForgeHosts, links::pr, questions, workspace::Manager,
        workspace_files,
    },
    forge::{ForgeRepo, account::Account, review::status::PrStatus},
    model::{Attention, Completion, IssueStatus, PresentSpace, SpaceDetail, SpaceStatus},
    repository,
};

/// What a space records about its links and work, which its status shows
/// with their state on the forge.
pub(crate) struct Records {
    pub(crate) issue: Option<issue::Registration>,
    pub(crate) pr_cleanup: Option<Registration>,
    pub(crate) completion: Option<Completion>,
}

impl Manager {
    /// The status of the workspace or chat, running or stopped, `selector`
    /// names: a workspace's adds what needs its worktree.
    pub async fn space_status(&self, selector: &str) -> Result<SpaceStatus> {
        let space = self.present_space(selector).await?;
        let (space, records, origin) = self.space_detail(space).await?;
        let (prs, issue_status) = self.linked_states(&origin, &records).await;
        let attention = self.attention(&space).await?;
        Ok(SpaceStatus {
            space,
            issue: records.issue,
            completion: records.completion,
            pr_cleanup: records.pr_cleanup,
            attention,
            prs,
            issue_status,
        })
    }

    /// What the status of `space` shows for its kind, what it records about
    /// its links and work, and the directory whose origin names its forge:
    /// the workspace's worktree or the chat's checkout.
    async fn space_detail(&self, space: PresentSpace) -> Result<(SpaceDetail, Records, PathBuf)> {
        Ok(match space {
            PresentSpace::Workspace(workspace) => {
                let (status, records) = self.worktree_status(&workspace.id).await?;
                let path = status.workspace.path.clone();
                (SpaceDetail::Workspace(Box::new(status)), records, path)
            }
            PresentSpace::Chat(chat) => {
                let records = self.chat_records(&chat.id, chat.completion.clone()).await?;
                let checkout = self.repository(&chat.repository_id).await?.path;
                let chat = Box::new(chat);
                (SpaceDetail::Chat { chat }, records, checkout)
            }
        })
    }

    /// What awaits the user about `space`.
    async fn attention(&self, space: &SpaceDetail) -> Result<Attention> {
        let unread_notifications = self.unread_space_notifications(space.id()).await?;
        let id = space.id().to_owned();
        let chat = space.worktree().is_none();
        self.store
            .run(move |db| {
                Ok(Attention {
                    unread_notifications,
                    open_questions: questions::open_count(db, &id)?,
                    pending_approvals: approvals::pending_count(db, &id, chat)?,
                    attached_files: workspace_files::count(db, &id)?,
                })
            })
            .await
    }

    /// What the chat `id` records about its links, with its `completion`.
    async fn chat_records(&self, id: &str, completion: Option<Completion>) -> Result<Records> {
        Ok(Records {
            issue: self.issue_registration(id).await?,
            pr_cleanup: self.pr_registration(id).await?,
            completion,
        })
    }

    /// The state of the watched PRs and linked issue in `records`, looked
    /// up on the forge of `origin`'s remote.
    async fn linked_states(
        &self,
        origin: &Path,
        records: &Records,
    ) -> (Vec<PrStatus>, Option<IssueStatus>) {
        let remote = self
            .check_access(origin, repository::remote_url_from_path(origin))
            .await;
        let account = self.user_account();
        let hosts = self.forge_hosts();
        tokio::join!(
            pr::watched(&hosts, &account, &remote, records.pr_cleanup.as_ref()),
            issue_status(&hosts, &account, &remote, records.issue.as_ref())
        )
    }
}

/// The state of the linked `issue` on the forge of `remote`, the space's
/// origin remote.
async fn issue_status(
    hosts: &ForgeHosts<'_>,
    account: &Account,
    remote: &Result<Option<String>>,
    issue: Option<&issue::Registration>,
) -> Option<IssueStatus> {
    let url = &issue?.url;
    let closed = async {
        let remote = pr::origin(remote, "issue lookup needs an origin remote")?;
        let forge = ForgeRepo::resolve(remote, hosts).await?;
        forge.issue_url_closed(account, url).await
    }
    .await;
    Some(match closed {
        Ok(closed) => IssueStatus {
            state: Some(if closed { "closed" } else { "open" }.into()),
            error: None,
        },
        Err(error) => IssueStatus {
            state: None,
            error: Some(format!("{error:#}")),
        },
    })
}

#[cfg(test)]
mod tests;
