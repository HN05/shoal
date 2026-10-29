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
        let stopped = self
            .stop_every_target("before the system shut down", |workspace| async move {
                self.stop_keeping_restores(&workspace, StopRequest::system_shutdown())
                    .await
            })
            .await;
        self.notify_shutdown_stop(&stopped).await;
    }

    /// Admit queued agents again once the shutdown was cancelled.
    pub(crate) async fn system_shutdown_cancelled(&self) {
        self.agent_queue.suspend(false).await;
    }

    /// Stop each workspace `shoal stop --all` stops with `stop`, given its
    /// ID, in parallel, holding the agent queue so nothing starts again, and
    /// tell the user of each that failed to stop `moment`. Returns those
    /// stopped.
    pub(super) async fn stop_every_target<F, Fut>(&self, moment: &str, stop: F) -> Vec<Workspace>
    where
        F: Fn(String) -> Fut,
        Fut: Future<Output = Result<()>>,
    {
        let _operation = self.gates.operation().await;
        self.agent_queue.suspend(true).await;
        let targets = match self.stop_targets().await {
            Ok(targets) => targets,
            Err(error) => {
                log!("stop {moment}: running workspaces unavailable: {error:#}");
                return Vec::new();
            }
        };
        let results = join_all(targets.iter().map(|workspace| stop(workspace.id.clone()))).await;
        let mut stopped = Vec::new();
        for (workspace, result) in targets.into_iter().zip(results) {
            match result {
                Ok(()) => stopped.push(workspace),
                Err(error) => self.notify_stop_failed(moment, &workspace, &error).await,
            }
        }
        stopped
    }

    async fn notify_stop_failed(&self, moment: &str, workspace: &Workspace, error: &anyhow::Error) {
        log!("stop {moment}: {} not stopped: {error:#}", workspace.name);
        self.notify_about(
            About::workspace(&workspace.name, &workspace.id),
            NotificationKind::StopFailed,
            format!("Could not stop tracked executions {moment}: {error:#}"),
        )
        .await;
    }

    /// Tell the user what the stop of `stopped` saved and how it continues.
    async fn notify_shutdown_stop(&self, stopped: &[Workspace]) {
        if stopped.is_empty() {
            return;
        }
        let queued = self
            .queued_stopped_restores()
            .await
            .unwrap_or_else(|error| {
                log!("system shutdown: agent queue unavailable: {error:#}");
                HashSet::new()
            });
        let waiting: Vec<&Workspace> = stopped
            .iter()
            .filter(|workspace| {
                waits_for_resume(&self.paths, workspace, &queued).unwrap_or_else(|error| {
                    log!(
                        "system shutdown: stopped work of {} unreadable: {error:#}",
                        workspace.name
                    );
                    true
                })
            })
            .collect();
        let continues = continuation(&waiting);
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
/// command, or an agent whose restore is neither `queued` nor requested. A
/// done workspace's requests wait for `shoal resume` to name it.
fn waits_for_resume(
    paths: &crate::paths::Paths,
    workspace: &Workspace,
    queued: &HashSet<String>,
) -> Result<bool> {
    let saved = Saved::load(paths, &workspace.id)?;
    let requested: HashSet<String> = if workspace.done {
        HashSet::new()
    } else {
        recovery::restore_requests(paths, &workspace.id)?
            .into_iter()
            .map(|(execution, _)| execution)
            .collect()
    };
    Ok(!saved.commands.is_empty()
        || saved
            .agents
            .iter()
            .any(|(execution, _)| !requested.contains(execution) && !queued.contains(execution)))
}

/// How the work of the `waiting` workspaces continues. `shoal resume --all`
/// skips done workspaces, so they are named.
fn continuation(waiting: &[&Workspace]) -> String {
    let (done, open): (Vec<&Workspace>, Vec<&Workspace>) =
        waiting.iter().partition(|workspace| workspace.done);
    let mut commands = Vec::new();
    if !open.is_empty() {
        commands.push("shoal resume --all".to_owned());
    }
    if !done.is_empty() {
        let names: Vec<&str> = done
            .iter()
            .map(|workspace| workspace.name.as_str())
            .collect();
        commands.push(format!("shoal resume {}", names.join(" ")));
    }
    if commands.is_empty() {
        "the agent queue restores its agents".into()
    } else {
        format!("continue it with {}", commands.join(" and "))
    }
}

#[cfg(test)]
mod tests;
