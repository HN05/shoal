//! Packages of spaces that keep running here under their own IDs, as the
//! hub keeps an intermittent machine's: a space as its removal would record
//! it now, with its agents, which continue their sessions where the package
//! is imported, a workspace at a new checkpoint with its pending review. The
//! state a package holds tells whether its space changed since: a
//! workspace's HEAD and files, its agents' session files and its records.
use std::{path::PathBuf, time::UNIX_EPOCH};

use anyhow::Result;
use serde_json::json;
use sha2::{Digest as _, Sha256};

use super::Prepared;
use crate::{
    daemon::manager::Manager,
    model::Space,
    transfer::{ExportedSpace, SessionSource, Sessions},
};

impl Manager {
    /// Write a package of `space`, which keeps running here, under its own
    /// ID, without the spaces below it, to the new file `file`.
    pub(crate) async fn write_kept_package(
        &self,
        space: &Space,
        file: PathBuf,
    ) -> Result<ExportedSpace> {
        let mut record = self.record_now(space, None).await?;
        self.carry_review(&mut record).await?;
        let settings = self.agent_settings_of(std::iter::once(space)).await;
        let prepared = Prepared::of(self, record, Vec::new(), settings).await?;
        self.write_prepared(prepared, file).await
    }

    /// What a package of `space` taken now would hold, as a digest that
    /// changes with it: a workspace's HEAD and files, the size and time of
    /// each of its agents' session files, and its records.
    pub(crate) async fn package_state(&self, space: &Space) -> Result<String> {
        let files = match space.worktree() {
            Some(worktree) => Some(crate::git::checkpoint::files_state(&worktree.path).await?),
            None => None,
        };
        let state = json!({
            "files": files,
            "sessions": self.session_file_states(space).await?,
            "records": self.record_state(space).await?,
        });
        let digest = Sha256::digest(state.to_string());
        Ok(digest.iter().map(|byte| format!("{byte:02x}")).collect())
    }

    /// The path, size and modification time of each file of the sessions
    /// the agents of `space` reported.
    async fn session_file_states(&self, space: &Space) -> Result<Vec<serde_json::Value>> {
        let sessions = Sessions {
            machine: None,
            agents: self.agents_now(&space.id).await?,
            last_sessions: Vec::new(),
        };
        let repos = match &space.repository_id {
            Some(id) => vec![self.repositories.repository(id).await?],
            None => Vec::new(),
        };
        let (environments, _) = self.package_environments(&repos).await;
        let settings = self.repositories.space_settings(self, space).await?;
        tokio::task::spawn_blocking(move || {
            let (sources, _) =
                crate::transfer::session_sources(&settings, &environments, &sessions);
            file_states(&sources)
        })
        .await
        .map_err(Into::into)
    }

    /// What the package of `space` keeps of its records: its name, mode,
    /// permissions, links, ready marks, holds, progress, wake timer and
    /// parent, a workspace's branch, what it builds on and its pending
    /// review, its open threads and its queued work.
    async fn record_state(&self, space: &Space) -> Result<serde_json::Value> {
        let threads: Vec<_> = (self.coordination.open_threads().await?.into_iter())
            .filter(|thread| thread.space_id == space.id)
            .collect();
        // Without its place in the queue, which other spaces' work moves.
        let queued: Vec<_> = (self.coordination.visible_queue(self).await?.into_iter())
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
            "pending_review": self.reviews.pending(&space.id).await?,
        }))
    }
}

/// The path, size and modification time of each of `sources`' files, as
/// they are now.
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
