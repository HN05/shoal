//! Closing a place with its workspace.
use std::time::Duration;

use anyhow::Result;

use super::herdr::{AgentStatus, Tab, TabDirective};
use crate::cli::{client, context::Context};

pub async fn watch(ctx: &Context, workspace: String, tab: String) -> Result<i32> {
    let _closing = ClosingTab::record(&ctx.paths, &workspace, &tab);
    let tab = Tab {
        id: tab,
        close_when_done: true,
    };
    let executable = crate::cli::installed::Executable::current();
    // A missing daemon or a failed query does not prove completion or removal.
    // Retain the observer across daemon restarts, but not deletion of its state.
    // A closed tab leaves nothing to close, whatever the daemon reports; a
    // failed tab query, like a failed daemon query, proves neither.
    while ctx.paths.state.exists() {
        let lifecycle = workspace_lifecycle(ctx, &workspace).await;
        let status = match tab.agent_status().await {
            Ok(Some(status)) => Some(status),
            Ok(None) => break,
            Err(_) => None,
        };
        match lifecycle {
            Ok(lifecycle) if status.is_some_and(|status| lifecycle.closes_tab(status)) => {
                tab.close().await;
                break;
            }
            Ok(_) => {}
            Err(error) if error.is::<client::ProtocolMismatch>() => {
                if let Some(failure) = executable.take_over(crate::cli::installed::arguments) {
                    return Err(failure);
                }
            }
            Err(_) => {}
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    TabDirective::discard_inherited();
    Ok(0)
}

/// Kept while a watcher waits to close a tab with its workspace, so commands
/// run in that tab can tell; the workspace's state goes with its record.
struct ClosingTab(Option<std::path::PathBuf>);

impl ClosingTab {
    fn path(paths: &crate::paths::Paths, workspace: &str, tab: &str) -> Option<std::path::PathBuf> {
        (!tab.is_empty() && !tab.contains('/')).then(|| {
            paths
                .workspace_state(workspace)
                .join(format!("closing-tab-{tab}"))
        })
    }

    /// Best effort: without the record, commands in the tab cannot tell.
    fn record(paths: &crate::paths::Paths, workspace: &str, tab: &str) -> Self {
        let path = Self::path(paths, workspace, tab).filter(|path| {
            path.parent()
                .is_some_and(|parent| std::fs::create_dir_all(parent).is_ok())
                && std::fs::write(path, "").is_ok()
        });
        Self(path)
    }
}

impl Drop for ClosingTab {
    fn drop(&mut self) {
        if let Some(path) = &self.0 {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// Whether this process runs in a Herdr tab that closes once `workspace` is
/// removed.
pub(super) fn closes_with(paths: &crate::paths::Paths, workspace: &str) -> bool {
    std::env::var("HERDR_TAB_ID")
        .ok()
        .and_then(|tab| ClosingTab::path(paths, workspace, &tab))
        .is_some_and(|path| path.exists())
}

#[derive(Debug, PartialEq)]
enum Lifecycle {
    Active,
    /// Done without cleanup. `stopped` while an agent or command waits for
    /// `shoal resume`.
    Kept {
        running: bool,
        stopped: bool,
    },
    Removed,
}

impl Lifecycle {
    /// A kept completion waits for the agent's final response: its tracked execution
    /// resolves, or Herdr reports that the agent finished its turn. A stopped
    /// agent keeps the tab, which shows why it stopped and where it resumes.
    fn closes_tab(&self, agent: AgentStatus) -> bool {
        match self {
            Self::Active | Self::Kept { stopped: true, .. } => false,
            Self::Kept { running, .. } => {
                !running || matches!(agent, AgentStatus::Idle | AgentStatus::Done)
            }
            Self::Removed => true,
        }
    }
}

async fn workspace_lifecycle(ctx: &Context, workspace: &str) -> Result<Lifecycle> {
    match client::inspect(&ctx.paths, workspace.into()).await {
        // Completion cleanup keeps the tab until removal, after its grace period.
        Ok(inspection)
            if inspection
                .completion
                .as_ref()
                .is_some_and(|completion| !completion.cleanup) =>
        {
            Ok(Lifecycle::Kept {
                running: !inspection.executions.is_empty(),
                stopped: crate::execution::recovery::pending(&ctx.paths, workspace)?,
            })
        }
        Ok(_) => Ok(Lifecycle::Active),
        Err(error) => {
            if client::workspaces(&ctx.paths)
                .await?
                .iter()
                .any(|item| item.id == workspace)
            {
                return Err(error);
            }
            Ok(Lifecycle::Removed)
        }
    }
}

#[cfg(test)]
mod tests;
