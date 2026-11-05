//! The processes of a space that Shoal does not track, as
//! [`spaces`](crate::process::identity::spaces) finds them, and the one path
//! that stops them, for `shoal stop` and removal alike.
use std::path::PathBuf;

use anyhow::{Result, ensure};

pub(crate) use crate::client::processes::{Selection, Stopped};
use crate::{
    daemon::{store, workspace::Manager},
    fsutil::off_runtime,
    model::{Chat, Workspace},
    process::identity::{
        self, Identity,
        spaces::{self, Space, Untracked},
    },
    state::WorkspaceState,
    tmp_dirs::Layout,
};

impl Selection {
    fn takes(self, process: &Untracked) -> bool {
        match self {
            Self::Processes => true,
            Self::Leftovers => process.leftover,
        }
    }
}

/// What a workspace's worktree ties to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WorktreeTies {
    /// Nothing, as for a worktree that is gone or not verified.
    None,
    /// The programs built in it, wherever they run.
    Programs,
    /// The processes working in it and the programs built in it.
    All,
}

impl WorktreeTies {
    /// All ties when `verified`, none otherwise.
    pub(crate) fn when(verified: bool) -> Self {
        if verified { Self::All } else { Self::None }
    }
}

/// Stopping a parent detaches the processes it started, which may start
/// others meanwhile; each round stops what the one before it left.
const ROUNDS: usize = 3;

impl Manager {
    /// Stop the untracked processes that `selection` takes of the workspace
    /// or chat `selector` names, as [`Self::stop_untracked`] does.
    pub(crate) async fn stop_space_processes(
        &self,
        selector: &str,
        selection: Selection,
        requester: Option<Identity>,
    ) -> Result<Stopped> {
        let workspace = match self.workspace(selector).await {
            Ok(workspace) => workspace,
            Err(error) => {
                let chat = self.running_chat(selector).await?.ok_or(error)?;
                let processes = self
                    .stop_untracked(self.chat_space(&chat).await?, selection, requester)
                    .await?;
                return Ok(Stopped {
                    space: chat.name,
                    processes,
                });
            }
        };
        let processes = self
            .stop_workspace_untracked(&workspace, selection, requester)
            .await?;
        Ok(Stopped {
            space: workspace.name,
            processes,
        })
    }

    /// [`Self::stop_untracked`] for `workspace` under a reserved lifecycle,
    /// so no execution registers between reading the tracked processes and
    /// signalling: its processes would count as untracked. The worktree ties
    /// processes to the workspace once it is verified to be its.
    async fn stop_workspace_untracked(
        &self,
        workspace: &Workspace,
        selection: Selection,
        requester: Option<Identity>,
    ) -> Result<usize> {
        let reserved = self
            .reserve_lifecycle(&workspace.id, WorkspaceState::Stopping)
            .await?;
        let stopped = async {
            let verified = off_runtime::is_dir(&workspace.path).await?
                && self.verify_worktree(workspace).await.is_ok();
            let space = self
                .workspace_space(workspace, WorktreeTies::when(verified))
                .await?;
            self.stop_untracked(space, selection, requester).await
        }
        .await;
        self.set_state(&workspace.id, reserved, workspace.error.clone())
            .await?;
        stopped
    }

    /// What ties processes to `workspace`, through its worktree as `ties`
    /// says, which the caller verified is the workspace's.
    pub(crate) async fn workspace_space(
        &self,
        workspace: &Workspace,
        ties: WorktreeTies,
    ) -> Result<Space> {
        let repo = self.repository(&workspace.repository_id).await?;
        let worktree = || workspace.path.clone();
        let mut directories: Vec<PathBuf> = (ties == WorktreeTies::All)
            .then(worktree)
            .into_iter()
            .collect();
        directories.extend(Layout::of(&repo).and_then(|layout| layout.existing(&workspace.id)));
        Ok(Space {
            id: workspace.id.clone(),
            directories,
            programs: (ties != WorktreeTies::None).then(worktree),
        })
    }

    /// What ties processes to `chat`: its identity and its temporary
    /// directory, while that is the one Shoal created.
    pub(crate) async fn chat_space(&self, chat: &Chat) -> Result<Space> {
        let repo = self.chat_repository(chat).await?;
        Ok(Space {
            id: chat.id.clone(),
            directories: (self.chat_tmp_layout(repo.as_ref())?)
                .and_then(|layout| layout.existing(&chat.id))
                .into_iter()
                .collect(),
            programs: None,
        })
    }

    /// The processes Shoal tracks: every execution's wrapper and command,
    /// every chat's agent, and the daemon, which runs hooks.
    async fn tracked_processes(&self) -> Result<Vec<Identity>> {
        let executions = self.store.run(|db| store::all_executions(db)).await?;
        let mut tracked: Vec<Identity> = executions
            .into_iter()
            .flat_map(|execution| [execution.wrapper, execution.child])
            .flatten()
            .collect();
        tracked.extend(self.chat_processes().await?);
        tracked.extend(identity::capture(std::process::id())?);
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
        let _handoffs = self.chat_handoffs.read().await;
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
