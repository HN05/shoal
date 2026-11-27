//! A space's status: the workspace's worktree status or the chat, with the
//! state of the items it links and what awaits the user.
use crate::Spaces;
use crate::daemon::Daemon;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::model::{Asleep, ByKind};
use shoal_protocol::status::{Attention, IssueStatus, SpaceDetail, SpaceStatus};
use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::{
    config::permissions::Permissions,
    forge::activity::status::PrStatus,
    model::{Completion, Space},
    repository, {approvals, store, workspace_files},
};
use shoal_protocol::space_names::SpaceSelector;
use shoal_protocol::{
    executions::{ProxyRetry, TurnRetry},
    links::{issue, pr::Registration},
};

/// Its status shows them with their state on the forge.
pub struct SpaceRecords {
    pub issues: Vec<issue::Registration>,
    pub pr_cleanup: Option<Registration>,
    pub completion: Option<Completion>,
}

impl Spaces {
    /// A workspace's adds what needs its worktree.
    pub async fn space_status(
        &self,
        daemon: &dyn Daemon,
        selector: &SpaceSelector,
    ) -> Result<SpaceStatus> {
        let space = self.space(selector).await?;
        let (permissions, permissions_error) = match self.status_permissions(daemon, &space).await {
            Ok(permissions) => (Some(permissions), None),
            Err(error) => (None, Some(format!("{error:#}"))),
        };
        let cleanup_reason = self.cleanup_reason(daemon, &space).await;
        let cleanup_at = self.cleanup_at(&space.id).await?;
        let records = self.space_records(daemon, &space.id).await?;
        let (space, origin) = self.space_detail(daemon, space).await?;
        let (prs, issue_states) = self.linked_states(daemon, &origin, &records).await;
        let attention = self.attention(daemon, &space).await?;
        let jobs = daemon.linked_job_workspaces(space.id()).await?;
        let waiting_jobs = daemon.linked_waiting_jobs(space.id()).await?;
        let (turn_retry, proxy_retry) = self.agent_retries(daemon, space.id()).await?;
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
            cleanup_reason,
            cleanup_at,
            jobs,
            waiting_jobs,
            turn_retry,
            proxy_retry,
            machine: None,
        })
    }

    /// In the space's own repository, or for a chat without one in the
    /// global config.
    async fn status_permissions(&self, daemon: &dyn Daemon, space: &Space) -> Result<Permissions> {
        (daemon
            .coordination()
            .space_permissions(daemon, &space.id, space.repository_id.as_ref()))
        .await
    }

    /// Also returns the directory whose origin names its forge: the
    /// workspace's worktree or the chat's checkout.
    async fn space_detail(
        &self,
        daemon: &dyn Daemon,
        space: Space,
    ) -> Result<(SpaceDetail, PathBuf)> {
        Ok(match space.by_kind() {
            ByKind::Workspace(workspace) => {
                let mut status =
                    (self.worktree_status(daemon, &SpaceSelector::id(&workspace.id))).await?;
                let workspace = &mut status.workspace;
                add_asleep_details(daemon, &workspace.id, workspace.asleep.as_mut()).await;
                let path = workspace.path.clone();
                (SpaceDetail::Workspace(Box::new(status)), path)
            }
            ByKind::Chat(mut chat) => {
                add_asleep_details(daemon, &chat.id, chat.asleep.as_mut()).await;
                let checkout = self.chat_checkout_path(daemon, &chat).await?;
                let chat = Box::new(chat);
                (SpaceDetail::Chat { chat }, checkout)
            }
        })
    }

    async fn attention(&self, daemon: &dyn Daemon, space: &SpaceDetail) -> Result<Attention> {
        let unread_notifications = self.notifications.unread_of_space(space.id()).await?;
        let waiting_threads = self.waiting_threads(daemon, space.id()).await?;
        let id = space.id().to_owned();
        self.store
            .run(move |db| {
                Ok(Attention {
                    unread_notifications,
                    waiting_threads,
                    pending_approvals: approvals::pending_count(db, &id)?,
                    attached_files: workspace_files::count(db, &id)?,
                })
            })
            .await
    }

    /// The retry of the turn an API failure ended, and the request the
    /// agent's proxy waits to retry.
    async fn agent_retries(
        &self,
        daemon: &dyn Daemon,
        id: &SpaceId,
    ) -> Result<(Option<TurnRetry>, Option<ProxyRetry>)> {
        let id = id.to_owned();
        let records = self.records.clone();
        let (turn_retry, executions) = (self.store)
            .read(move |tx| Ok((records.turn_retry(tx, &id)?, store::executions(tx, &id)?)))
            .await?;
        Ok((turn_retry, daemon.proxy_retry(&executions)))
    }

    pub async fn space_records(&self, daemon: &dyn Daemon, id: &SpaceId) -> Result<SpaceRecords> {
        Ok(SpaceRecords {
            issues: daemon.issue_registrations(id).await?,
            pr_cleanup: daemon.pr_registration(id).await?,
            completion: self.completion(id).await?,
        })
    }

    /// Looked up on the forge of `origin`'s remote.
    async fn linked_states(
        &self,
        daemon: &dyn Daemon,
        origin: &Path,
        records: &SpaceRecords,
    ) -> (Vec<PrStatus>, Vec<IssueStatus>) {
        let remote = self
            .file_access
            .checked(
                &self.notifications,
                origin,
                repository::remote_url_from_path(origin),
            )
            .await;
        (daemon.linked_states(&remote, records.pr_cleanup.as_ref(), &records.issues)).await
    }
}

/// Add the leases an asleep agent keeps and what holds back its woken
/// restore. A status that cannot read either shows none.
async fn add_asleep_details(daemon: &dyn Daemon, id: &SpaceId, asleep: Option<&mut Asleep>) {
    let Some(asleep) = asleep else {
        return;
    };
    match daemon.kept_leases(id).await {
        Ok(keeps) => asleep.keeps = keeps,
        Err(error) => crate::log!("leases the agent of {id} keeps unread: {error:#}"),
    }
    match daemon.asleep_hold(id).await {
        Ok(held_by) => asleep.held_by = held_by,
        Err(error) => crate::log!("what holds back the restore of {id} unread: {error:#}"),
    }
}
