//! The processes of a space that Shoal does not track, as
//! [`spaces`](crate::process::identity::spaces) finds them, and the one path
//! that stops them, for `shoal stop` and removal alike.
use crate::Spaces;
use crate::daemon::Daemon;
use shoal_ids::space_ids::SpaceId;
use std::path::PathBuf;

use anyhow::{Result, ensure};

use crate::{
    fsutil::off_runtime,
    model::{self, Workspace},
    process::{
        described,
        identity::{
            self, Identity,
            spaces::{self, Space, Untracked},
        },
    },
    state::WorkspaceState,
    {events::StoppedProcess, leftovers::COMMAND_LIMIT, store, workspace::ownership::CommonDir},
};
pub use shoal_protocol::processes::{Selection, Stopped};
use shoal_protocol::space_names::SpaceSelector;

/// Operations on the client type [`Selection`] that only the `shoal` command uses.
trait SelectionExt {
    fn takes(self, process: &Untracked) -> bool;
}

impl SelectionExt for Selection {
    fn takes(self, process: &Untracked) -> bool {
        match self {
            Self::Processes => true,
            Self::Leftovers => process.leftover,
        }
    }
}

/// What a workspace's worktree ties to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorktreeTies {
    /// Nothing, as for a worktree that is gone or not verified.
    None,
    /// The programs built in it, wherever they run.
    Programs,
    /// The processes working in it and the programs built in it.
    All,
}

impl WorktreeTies {
    /// All ties when `verified`, none otherwise.
    pub fn when(verified: bool) -> Self {
        if verified { Self::All } else { Self::None }
    }
}

/// Stopping a parent detaches the processes it started, which may start
/// others meanwhile; each round stops what the one before it left.
const ROUNDS: usize = 3;

impl Spaces {
    /// Stop every untracked process of the space being removed, such as
    /// servers its hooks or apps started, scripts a recording left behind,
    /// or an agent Shoal did not start under a wrapper, since none outlives
    /// removal. Its worktree, where it has one, ties processes to it as
    /// `ties` says once the directory is verified to be its worktree still,
    /// of the repository whose shared Git directory is `common_dir` when
    /// the removal read it.
    pub async fn stop_removed_processes(
        &self,
        daemon: &dyn Daemon,
        space: &model::Space,
        ties: WorktreeTies,
        common_dir: Option<&CommonDir>,
    ) -> Result<()> {
        if let Some(workspace) = space.to_workspace()
            && ties != WorktreeTies::None
        {
            self.verify_removed_worktree_with(daemon, &workspace, common_dir)
                .await?;
        }
        let space = self.space_ties(daemon, space, ties).await?;
        self.stop_untracked(space, Selection::Processes, None)
            .await?;
        Ok(())
    }

    /// Stop the untracked processes that `selection` takes of the workspace
    /// or chat `selector` names, as [`Self::stop_untracked`] does.
    pub async fn stop_space_processes(
        &self,
        daemon: &dyn Daemon,
        selector: &SpaceSelector,
        selection: Selection,
        requester: Option<Identity>,
    ) -> Result<Stopped> {
        let space = self.space(selector).await?;
        let stopped = match space.to_workspace() {
            Some(workspace) => {
                (self.stop_workspace_untracked(daemon, &workspace, selection, requester)).await?
            }
            None => {
                let ties = self.space_ties(daemon, &space, WorktreeTies::None).await?;
                self.stop_untracked(ties, selection, requester).await?
            }
        };
        Ok(Stopped {
            space: space.name.clone(),
            processes: stopped.len(),
        })
    }

    /// [`Self::stop_untracked`] for `workspace` under a reserved lifecycle,
    /// so no execution registers between reading the tracked processes and
    /// signalling: its processes would count as untracked. The worktree ties
    /// processes to the workspace once it is verified to be its. Nothing
    /// runs in one whose preparation waits.
    pub async fn stop_workspace_untracked(
        &self,
        daemon: &dyn Daemon,
        workspace: &Workspace,
        selection: Selection,
        requester: Option<Identity>,
    ) -> Result<Vec<StoppedProcess>> {
        let first = std::future::ready(Ok(()));
        (self.stop_workspace_untracked_after(daemon, workspace, selection, requester, first)).await
    }

    /// [`Self::stop_workspace_untracked`], once `first` has run under the
    /// reserved lifecycle, before anything is signaled; a `first` that fails
    /// signals nothing.
    pub async fn stop_workspace_untracked_after(
        &self,
        daemon: &dyn Daemon,
        workspace: &Workspace,
        selection: Selection,
        requester: Option<Identity>,
        first: impl Future<Output = Result<()>>,
    ) -> Result<Vec<StoppedProcess>> {
        if workspace.preparation_waits.is_some() {
            return Ok(Vec::new());
        }
        let reserved = self
            .reserve_lifecycle(&workspace.id, WorkspaceState::Stopping)
            .await?;
        let stopped = async {
            first.await?;
            (self.stop_reserved_untracked(daemon, workspace, selection, requester)).await
        }
        .await;
        self.set_state(&workspace.id, reserved, workspace.error.clone())
            .await?;
        stopped
    }

    /// [`Self::stop_untracked`] for `workspace`, whose lifecycle the caller
    /// reserved.
    async fn stop_reserved_untracked(
        &self,
        daemon: &dyn Daemon,
        workspace: &Workspace,
        selection: Selection,
        requester: Option<Identity>,
    ) -> Result<Vec<StoppedProcess>> {
        let verified = off_runtime::is_dir(&workspace.path).await?
            && self.verify_worktree(daemon, workspace).await.is_ok();
        let space = crate::model::Space::from(workspace.clone());
        let space = self
            .space_ties(daemon, &space, WorktreeTies::when(verified))
            .await?;
        self.stop_untracked(space, selection, requester).await
    }

    /// What ties processes to `space`: its identity, its temporary directory
    /// while that is the one Shoal created, and its worktree, where it has
    /// one, as `ties` says, which the caller verified is the space's.
    pub async fn space_ties(
        &self,
        daemon: &dyn Daemon,
        space: &crate::model::Space,
        ties: WorktreeTies,
    ) -> Result<Space> {
        let worktree = space.worktree().map(|workspace| workspace.path.clone());
        let mut directories: Vec<PathBuf> = (worktree.clone())
            .filter(|_| ties == WorktreeTies::All)
            .into_iter()
            .collect();
        let layout = self.space_tmp_layout(daemon, space).await?;
        directories.extend(layout.and_then(|layout| layout.existing(&space.id)));
        Ok(Space {
            id: space.id.to_owned(),
            directories,
            programs: worktree.filter(|_| ties != WorktreeTies::None),
        })
    }

    /// The processes Shoal tracks: every execution's wrapper and command,
    /// a chat's agent's among them, and the daemon, which runs hooks.
    async fn tracked_processes(&self) -> Result<Vec<Identity>> {
        let executions = self.store.run(|db| store::all_executions(db)).await?;
        let mut tracked: Vec<Identity> = executions
            .into_iter()
            .flat_map(|execution| [execution.wrapper, execution.child])
            .flatten()
            .collect();
        tracked.extend(identity::capture(std::process::id())?);
        Ok(tracked)
    }

    /// The untracked processes of each of `spaces`, by space ID, other than
    /// `requester` and the processes that started it.
    pub async fn untracked_processes(
        &self,
        spaces: Vec<Space>,
        requester: Option<Identity>,
    ) -> Result<std::collections::HashMap<SpaceId, Vec<Untracked>>> {
        let mut tracked = self.tracked_processes().await?;
        tracked.extend(requester);
        spaces::survey(spaces, tracked).await
    }

    /// Stop the untracked processes of `space` that `selection` takes, other
    /// than `requester` and the processes that started it, returning them
    /// as they ran. Each is checked against its identity before every
    /// signal, gets SIGTERM, then SIGKILL after the grace period; the error
    /// names those that survived.
    pub async fn stop_untracked(
        &self,
        space: Space,
        selection: Selection,
        requester: Option<Identity>,
    ) -> Result<Vec<StoppedProcess>> {
        let mut stopped = Vec::new();
        for _ in 0..ROUNDS {
            let targets = self.selected(&space, selection, requester.clone()).await?;
            if targets.is_empty() {
                return Ok(stopped);
            }
            let described = described::describe(&pids_of(&targets))?;
            stop(&targets).await?;
            stopped.extend(as_stopped(&targets, described));
        }
        let remaining = self.selected(&space, selection, requester).await?;
        ensure!(
            remaining.is_empty(),
            "processes survived stopping: {}; stop them yourself, then retry",
            pids(&remaining)
        );
        Ok(stopped)
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

/// The processes of `targets` that `described` found running, as it shows them.
fn as_stopped(
    targets: &[Identity],
    mut described: std::collections::HashMap<u32, described::Description>,
) -> Vec<StoppedProcess> {
    targets
        .iter()
        .filter_map(|process| {
            let description = described.remove(&process.pid)?;
            Some(StoppedProcess {
                pid: process.pid,
                command: description.command.chars().take(COMMAND_LIMIT).collect(),
            })
        })
        .collect()
}

fn pids_of(processes: &[Identity]) -> Vec<u32> {
    processes.iter().map(|process| process.pid).collect()
}

fn pids<'a>(processes: impl IntoIterator<Item = &'a Identity>) -> String {
    processes
        .into_iter()
        .map(|process| process.pid.to_string())
        .collect::<Vec<_>>()
        .join(", ")
}
