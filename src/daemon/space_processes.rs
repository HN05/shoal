//! The processes of a space that Shoal does not track, as
//! [`spaces`](crate::process::identity::spaces) finds them, and the one path
//! that stops them, for `shoal stop` and removal alike.
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

use crate::{
    daemon::{store, workspace::Manager},
    model::Workspace,
    process::identity::{
        self, Identity,
        spaces::{self, Space, Untracked},
    },
    tmp_dirs::Layout,
};

/// Which of a space's untracked processes an operation takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Selection {
    /// Every untracked process, leftovers included.
    Processes,
    /// Only the processes left behind by what started them.
    Leftovers,
}

impl Selection {
    fn takes(self, process: &Untracked) -> bool {
        match self {
            Self::Processes => true,
            Self::Leftovers => process.leftover,
        }
    }
}

/// Stopping a parent detaches the processes it started, which may start
/// others meanwhile; each round stops what the one before it left.
const ROUNDS: usize = 3;

impl Manager {
    /// What ties processes to `workspace`, through its worktree too when
    /// `worktree` is set, which the caller verified is the workspace's.
    pub(crate) async fn workspace_space(
        &self,
        workspace: &Workspace,
        worktree: bool,
    ) -> Result<Space> {
        let repo = self.repository(&workspace.repository_id).await?;
        Ok(Space {
            id: workspace.id.clone(),
            worktree: worktree.then(|| workspace.path.clone()),
            tmp_dir: Layout::of(&repo).map(|layout| layout.locate(&workspace.id)),
        })
    }

    /// The processes Shoal tracks: every execution's wrapper and command,
    /// and every chat's agent.
    async fn tracked_processes(&self) -> Result<Vec<Identity>> {
        let executions = self.store.run(|db| store::all_executions(db)).await?;
        let mut tracked: Vec<Identity> = executions
            .into_iter()
            .flat_map(|execution| [execution.wrapper, execution.child])
            .flatten()
            .collect();
        tracked.extend(self.chat_processes().await?);
        Ok(tracked)
    }

    /// The untracked processes of each of `spaces`, by space ID, other than
    /// `requester` and the processes that started it.
    pub(crate) async fn untracked_processes(
        &self,
        spaces: Vec<Space>,
        requester: Option<Identity>,
    ) -> Result<std::collections::HashMap<String, Vec<Untracked>>> {
        let mut tracked = self.tracked_processes().await?;
        tracked.extend(requester);
        spaces::survey(spaces, tracked).await
    }

    /// Stop the untracked processes of `space` that `selection` takes, other
    /// than `requester` and the processes that started it, returning how
    /// many there were. Each is checked against its identity before every
    /// signal, gets SIGTERM, then SIGKILL after the grace period; the error
    /// names those that survived.
    pub(crate) async fn stop_untracked(
        &self,
        space: Space,
        selection: Selection,
        requester: Option<Identity>,
    ) -> Result<usize> {
        let mut stopped = std::collections::HashSet::new();
        for _ in 0..ROUNDS {
            let targets = self.selected(&space, selection, requester.clone()).await?;
            if targets.is_empty() {
                return Ok(stopped.len());
            }
            stop(&targets).await?;
            stopped.extend(targets);
        }
        let remaining = self.selected(&space, selection, requester).await?;
        ensure!(
            remaining.is_empty(),
            "processes survived stopping: {}; stop them yourself, then retry",
            pids(&remaining)
        );
        Ok(stopped.len())
    }

    async fn selected(
        &self,
        space: &Space,
        selection: Selection,
        requester: Option<Identity>,
    ) -> Result<Vec<Identity>> {
        let mut found = self
            .untracked_processes(vec![space.clone()], requester)
            .await?;
        Ok(found
            .remove(&space.id)
            .unwrap_or_default()
            .into_iter()
            .filter(|process| selection.takes(process))
            .map(|process| process.identity)
            .collect())
    }
}

/// Stop `targets`, naming each that survived.
async fn stop(targets: &[Identity]) -> Result<()> {
    let Err(error) = identity::stop_verified(targets).await else {
        return Ok(());
    };
    let survivors: Vec<&Identity> = targets
        .iter()
        .filter(|process| identity::alive(process).unwrap_or(true))
        .collect();
    ensure!(
        survivors.is_empty(),
        "{error:#}; these processes did not stop: {}",
        pids(survivors)
    );
    Err(error)
}

fn pids<'a>(processes: impl IntoIterator<Item = &'a Identity>) -> String {
    processes
        .into_iter()
        .map(|process| process.pid.to_string())
        .collect::<Vec<_>>()
        .join(", ")
}
