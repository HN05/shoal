//! Packages of spaces that keep running here under their own IDs, as the
//! hub keeps an intermittent machine's. The state a package holds tells
//! whether its space changed since. See `docs/design/hub.md`.
use crate::Spaces;
use crate::daemon::Daemon;
use std::{path::PathBuf, time::UNIX_EPOCH};

use anyhow::Result;
use serde_json::json;
use sha2::{Digest as _, Sha256};

use super::Prepared;
use crate::{
    model::Space,
    transfer::{ExportedSpace, SessionSource, Sessions},
};

impl Spaces {
    /// Leaves out the spaces below. `file` must be new.
    pub async fn write_kept_package(
        &self,
        daemon: &dyn Daemon,
        space: &Space,
        file: PathBuf,
    ) -> Result<ExportedSpace> {
        let mut record = self.record_now(daemon, space, None).await?;
        super::carried::carry_review(daemon, &mut record).await?;
        let settings = self.agent_settings_of(daemon, std::iter::once(space)).await;
        let prepared = Prepared::of(daemon, record, Vec::new(), settings).await?;
        self.write_prepared(daemon, prepared, file).await
    }

    /// A digest of a workspace's HEAD and files, the size and time of each
    /// of its agents' session files, and its records.
    pub async fn package_state(&self, daemon: &dyn Daemon, space: &Space) -> Result<String> {
        let files = match space.worktree() {
            Some(worktree) => Some(crate::git::checkpoint::files_state(&worktree.path).await?),
            None => None,
        };
        let state = json!({
            "files": files,
            "sessions": self.session_file_states(daemon, space).await?,
            "records": self.record_state(daemon, space).await?,
        });
        let digest = Sha256::digest(state.to_string());
        Ok(digest.iter().map(|byte| format!("{byte:02x}")).collect())
    }

    async fn session_file_states(
        &self,
        daemon: &dyn Daemon,
        space: &Space,
    ) -> Result<Vec<serde_json::Value>> {
        let sessions = Sessions {
            machine: None,
            agents: self.agents_now(&space.id).await?,
            last_sessions: Vec::new(),
        };
        let repos = match &space.repository_id {
            Some(id) => vec![
                daemon
                    .repository(crate::repository::Selector::from(id))
                    .await?,
            ],
            None => Vec::new(),
        };
        let (environments, _) = self.package_environments(daemon, &repos).await;
        let settings = daemon.space_settings(space).await?;
        tokio::task::spawn_blocking(move || {
            let (sources, _) =
                crate::transfer::session_sources(&settings, &environments, &sessions);
            file_states(&sources)
        })
        .await
        .map_err(Into::into)
    }

    /// Its name, mode, permissions, links, ready marks, holds, progress, wake
    /// timer and parent, a workspace's branch, what it builds on and its
    /// pending review, its open threads and its queued work.
    pub async fn record_state(
        &self,
        daemon: &dyn Daemon,
        space: &Space,
    ) -> Result<serde_json::Value> {
        let threads: Vec<_> = (daemon.coordination().open_threads().await?.into_iter())
            .filter(|thread| thread.space_id == space.id)
            .collect();
        // Leave out its place in the queue, which other spaces' work moves.
        let queued: Vec<_> = (daemon
            .coordination()
            .visible_queue(daemon)
            .await?
            .into_iter())
        .filter(|entry| entry.space_id.as_ref() == Some(&space.id))
        .map(|entry| json!([entry.id, entry.kind, entry.state, entry.blockers]))
        .collect();
        Ok(json!({
            "name": space.name,
            "mode": space.mode,
            "narrowing": space.narrowing,
            "links": space.links,
            "review": space.review,
            "holds": space.holds,
            "progress": space.progress,
            "wake_at": space.wake_at,
            "parent": space.parent,
            "head": space.worktree().map(|worktree| &worktree.head),
            "base": space.worktree().and_then(|worktree| worktree.checkout.base()),
            "base_workspace": space.to_workspace().and_then(|workspace| workspace.base_workspace.clone()),
            "threads": threads,
            "queued": queued,
            "pending_review": daemon.pending_review(&space.id).await?,
        }))
    }
}

/// As they are now.
fn file_states(sources: &[SessionSource]) -> Vec<serde_json::Value> {
    (sources.iter())
        .map(|source| {
            let metadata = std::fs::metadata(&source.source).ok();
            let modified = (metadata.as_ref())
                .and_then(|metadata| metadata.modified().ok())
                .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok());
            json!([
                source.source,
                metadata.map(|metadata| metadata.len()),
                modified.map(|modified| modified.as_nanos()),
            ])
        })
        .collect()
}

#[cfg(test)]
mod tests;
