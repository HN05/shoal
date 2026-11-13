//! A space's status: the workspace's worktree status or the chat, with the
//! state of the items it links and what awaits the user.
use crate::client::space_ids::SpaceId;
use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::client::space_names::SpaceSelector;
use crate::{
    client::executions::{ProxyRetry, TurnRetry},
    client::links::{issue, pr::Registration},
    config::permissions::Permissions,
    daemon::{
        approvals,
        forge_hosts::ForgeHosts,
        links::pr,
        questions, store,
        workspace::{Manager, turn_retry},
        workspace_files,
    },
    forge::{ForgeRepo, account::Account, items::reference::ItemRef, review::status::PrStatus},
    model::{Attention, Completion, IssueStatus, PresentSpace, SpaceDetail, SpaceStatus},
    repository,
};

/// What a space records about its links and work, which its status shows
/// with their state on the forge.
pub(crate) struct Records {
    pub(crate) issues: Vec<issue::Registration>,
    pub(crate) pr_cleanup: Option<Registration>,
    pub(crate) completion: Option<Completion>,
}

impl Manager {
    /// The status of the workspace or chat, running or stopped, `selector`
    /// names: a workspace's adds what needs its worktree.
    pub async fn space_status(&self, selector: &SpaceSelector) -> Result<SpaceStatus> {
        let space = self.space(selector).await?;
        let (permissions, permissions_error) = match self.status_permissions(&space).await {
            Ok(permissions) => (Some(permissions), None),
            Err(error) => (None, Some(format!("{error:#}"))),
        };
        let (space, records, origin) = self.space_detail(space).await?;
        let (prs, issue_states) = self.linked_states(&origin, &records).await;
        let attention = self.attention(&space).await?;
        let jobs = self.linked_job_workspaces(space.id()).await?;
        let waiting_jobs = self.linked_waiting_jobs(space.id()).await?;
        let (turn_retry, proxy_retry) = self.agent_retries(space.id()).await?;
        Ok(SpaceStatus {
            space,
            issues: records.issues,
            completion: records.completion,
            pr_cleanup: records.pr_cleanup,
            attention,
            prs,
            issue_states,
            permissions,
            permissions_error,
            jobs,
            waiting_jobs,
            turn_retry,
            proxy_retry,
        })
    }

    /// The effective permissions of `space` in its own repository, or for a
    /// chat without one in the global config.
    async fn status_permissions(&self, space: &PresentSpace) -> Result<Permissions> {
        let repository = match space {
            PresentSpace::Workspace(workspace) => Some(&workspace.repository_id),
            PresentSpace::Chat(chat) => chat.repository_id.as_ref(),
        };
        self.space_permissions(space.id(), repository).await
    }

    /// What the status of `space` shows for its kind, what it records about
    /// its links and work, and the directory whose origin names its forge:
    /// the workspace's worktree or the chat's checkout.
    async fn space_detail(&self, space: PresentSpace) -> Result<(SpaceDetail, Records, PathBuf)> {
        Ok(match space {
            PresentSpace::Workspace(workspace) => {
                let (status, records) = self
                    .worktree_status(&SpaceSelector::id(&workspace.id))
                    .await?;
                let path = status.workspace.path.clone();
                (SpaceDetail::Workspace(Box::new(status)), records, path)
            }
            PresentSpace::Chat(chat) => {
                let records = self.chat_records(&chat.id, chat.completion.clone()).await?;
                let checkout = self.chat_checkout_path(&chat).await?;
                let chat = Box::new(chat);
                (SpaceDetail::Chat { chat }, records, checkout)
            }
        })
    }

    /// What awaits the user about `space`.
    async fn attention(&self, space: &SpaceDetail) -> Result<Attention> {
        let unread_notifications = self.unread_space_notifications(space.id()).await?;
        let id = space.id().to_owned();
        self.store
            .run(move |db| {
                Ok(Attention {
                    unread_notifications,
                    open_questions: questions::open_count(db, &id)?,
                    pending_approvals: approvals::pending_count(db, &id)?,
                    attached_files: workspace_files::count(db, &id)?,
                })
            })
            .await
    }

    /// The retry of the turn an API failure ended for the agent of the space
    /// `id`, and the request its proxy waits to retry.
    async fn agent_retries(&self, id: &SpaceId) -> Result<(Option<TurnRetry>, Option<ProxyRetry>)> {
        let id = id.to_owned();
        let (turn_retry, executions) = (self.store)
            .read(move |tx| Ok((turn_retry::get(tx, &id)?, store::executions(tx, &id)?)))
            .await?;
        Ok((turn_retry, self.proxy_retry(&executions)))
    }

    /// What the chat `id` records about its links, with its `completion`.
    async fn chat_records(&self, id: &SpaceId, completion: Option<Completion>) -> Result<Records> {
        Ok(Records {
            issues: self.issue_registrations(id).await?,
            pr_cleanup: self.pr_registration(id).await?,
            completion,
        })
    }

    /// The state of the watched PRs and assigned issues in `records`, looked
    /// up on the forge of `origin`'s remote.
    async fn linked_states(
        &self,
        origin: &Path,
        records: &Records,
    ) -> (Vec<PrStatus>, Vec<IssueStatus>) {
        let remote = self
            .check_access(origin, repository::remote_url_from_path(origin))
            .await;
        let account = self.user_account();
        let hosts = self.forge_hosts();
        tokio::join!(
            pr::watched(&hosts, &account, &remote, records.pr_cleanup.as_ref()),
            issue_states(&hosts, &account, &remote, &records.issues)
        )
    }
}

/// The state of each of the assigned `issues` on the forge of `remote`, the
/// space's origin remote.
async fn issue_states(
    hosts: &ForgeHosts<'_>,
    account: &Account,
    remote: &Result<Option<String>>,
    issues: &[issue::Registration],
) -> Vec<IssueStatus> {
    let states = (issues.iter()).map(|issue| issue_status(hosts, account, remote, &issue.url));
    futures_util::future::join_all(states).await
}

/// The state of the issue at `url` on the forge of `remote`.
async fn issue_status(
    hosts: &ForgeHosts<'_>,
    account: &Account,
    remote: &Result<Option<String>>,
    url: &ItemRef,
) -> IssueStatus {
    let closed = async {
        let remote = pr::origin(remote, "issue lookup needs an origin remote")?;
        let forge = ForgeRepo::resolve(remote, hosts).await?;
        forge.issue_closed(account, url).await
    }
    .await;
    let url = url.clone();
    match closed {
        Ok(closed) => IssueStatus {
            url,
            state: Some(if closed { "closed" } else { "open" }.into()),
            error: None,
        },
        Err(error) => IssueStatus {
            url,
            state: None,
            error: Some(format!("{error:#}")),
        },
    }
}

#[cfg(test)]
mod tests;
