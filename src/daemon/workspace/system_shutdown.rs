//! Stopping tracked work before the system shuts down, as `shoal stop --all`
//! does, while the daemon and wrappers still run to save it.
use std::collections::HashSet;

use anyhow::Result;
use futures_util::future::join_all;

use super::{Manager, StopRequest};
use crate::{
    daemon::{
        log,
        notifications::{About, NotificationKind},
    },
    execution::recovery::{self, Saved},
    model::Workspace,
};

impl Manager {
    /// Stop the workspaces `shoal stop --all` stops, each wrapper saving its
    /// agent before the stop and requesting restores that need no user,
    /// which the stop keeps with those already waiting, and hold the agent
    /// queue until the shutdown is cancelled, so nothing starts again before
    /// it. The user learns what to continue. An update waits for the stop.
    pub(crate) async fn stop_for_system_shutdown(&self) {
        let _operation = self.gates.operation().await;
        self.agent_queue.suspend(true);
        let targets = match self.stop_targets().await {
            Ok(targets) => targets,
            Err(error) => {
                log!("system shutdown: running workspaces unavailable: {error:#}");
                return;
            }
        };
        let results = join_all(targets.iter().map(|workspace| {
            self.stop_keeping_restores(&workspace.id, StopRequest::system_shutdown())
        }))
        .await;
        let mut stopped = Vec::new();
        for (workspace, result) in targets.iter().zip(results) {
            match result {
                Ok(()) => stopped.push(workspace),
                Err(error) => self.notify_shutdown_stop_failed(workspace, &error).await,
            }
        }
        self.notify_shutdown_stop(&stopped).await;
    }

    /// Admit queued agents again once the shutdown was cancelled.
    pub(crate) fn system_shutdown_cancelled(&self) {
        self.agent_queue.suspend(false);
    }

    async fn notify_shutdown_stop_failed(&self, workspace: &Workspace, error: &anyhow::Error) {
        log!("system shutdown: {} not stopped: {error:#}", workspace.name);
        self.notify_about(
            About::workspace(&workspace.name, &workspace.id),
            NotificationKind::StopFailed,
            format!("Could not stop tracked executions before the system shut down: {error:#}"),
        )
        .await;
    }

    /// Tell the user what the stop of `stopped` saved and how it continues.
    async fn notify_shutdown_stop(&self, stopped: &[&Workspace]) {
        if stopped.is_empty() {
            return;
        }
        let waits = stopped.iter().any(|workspace| {
            waits_for_resume(&self.paths, &workspace.id).unwrap_or_else(|error| {
                log!(
                    "system shutdown: stopped work of {} unreadable: {error:#}",
                    workspace.name
                );
                true
            })
        });
        let continues = if waits {
            "continue it with shoal resume --all"
        } else {
            "the agent queue restores its agents"
        };
        let count = stopped.len();
        let workspaces = if count == 1 {
            "workspace"
        } else {
            "workspaces"
        };
        self.notify(
            None,
            NotificationKind::SystemShutdown,
            format!(
                "Stopped work in {count} {workspaces} before the system shut down; {continues}"
            ),
        )
        .await;
    }
}

/// Whether the workspace saved work only `shoal resume` continues: a
/// command, or an agent whose restore no request queues.
fn waits_for_resume(paths: &crate::paths::Paths, workspace_id: &str) -> Result<bool> {
    let saved = Saved::load(paths, workspace_id)?;
    let requested: HashSet<String> = recovery::restore_requests(paths, workspace_id)?
        .into_iter()
        .map(|(execution, _)| execution)
        .collect();
    Ok(!saved.commands.is_empty()
        || saved
            .agents
            .iter()
            .any(|(execution, _)| !requested.contains(execution)))
}

#[cfg(test)]
mod tests;
