//! Moving repository directories into a new `root_dir`. Intent is recorded
//! before anything moves, so an interrupted move finishes or rolls back.
use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, ensure};
use shoal_core::{
    fsutil, process,
    shoal_dirs::{Contents, ShoalDir},
};
use shoal_daemon_base::gates::{GuardMode, ResourceGuard};
use shoal_ids::repository_ids::RepositoryId;
use shoal_protocol::model::{Execution, MovedRepository, Repository, RepositoryMoves, Workspace};
use shoal_store as store;

use crate::{
    Repositories,
    daemon::Daemon,
    root::{ConfiguredRoot, Recorded, plan_root, reserve_directory},
    spotlight::{self, Layout},
};

pub mod chats;
pub use shoal_daemon_base::root_moves::Running;
mod intent;

impl Repositories {
    /// The repositories [`Self::move_all`] would move into `root`, or the
    /// configured `root_dir`, in `layout`, the chats without a repository
    /// whose files it would move there, and the executions running in them,
    /// which must stop first, refusing as the move would before anything
    /// moves or changes.
    pub async fn plan_moves(
        &self,
        daemon: &dyn Daemon,
        root: Option<&Path>,
        layout: Layout,
    ) -> Result<RepositoryMoves> {
        let _registry = self.gates.lock_registry().await;
        let mut target = self.configured_root()?;
        target.layout = layout;
        if let Some(root) = root {
            target.path = root.to_owned();
        }
        let (plan, repositories) = self
            .movable_repositories(daemon, &target, Running::StoppedFirst)
            .await?;
        let directory = plan.destination()?;
        let mut executions = Vec::new();
        for repo in &repositories {
            executions.extend(self.repository_executions(&repo.id).await?);
        }
        let chats = daemon.outside_chats_moving(&target.path, &directory, Running::StoppedFirst);
        let mut chats = chats.await?;
        for chat in &mut chats {
            executions.append(&mut chat.running);
        }
        Ok(RepositoryMoves {
            directory,
            repositories,
            executions,
            chats: chats.into_iter().map(|chat| chat.name).collect(),
        })
    }

    /// Move every repository directory outside the configured `root_dir` in
    /// `layout` into it, with the workspaces and clone inside, and the files
    /// of the chats without a repository, then link a root these moves
    /// emptied to the directory Spotlight skips. Each repository and chat
    /// must be idle; all are checked before the first moves.
    pub async fn move_all(
        &self,
        daemon: &dyn Daemon,
        layout: Layout,
    ) -> Result<Vec<MovedRepository>> {
        let _registry = self.gates.lock_registry().await;
        let mut target = self.configured_root()?;
        target.layout = layout;
        let (plan, moving) = (self.movable_repositories(daemon, &target, Running::Refused)).await?;
        let root = plan.destination()?;
        let chats = daemon.outside_chats_moving(&target.path, &root, Running::Refused);
        let chats = chats.await?;
        plan.apply()?;
        let mut moved = Vec::with_capacity(moving.len());
        for repo in moving {
            moved.push(self.move_repository(daemon, &repo, &root).await?);
        }
        let _files = self.gates.lock_outside_message_files().await;
        (daemon.move_outside_chats(chats, &target.path, &root)).await?;
        self.root_plan(&target, &self.list().await?)
            .await?
            .apply()?;
        Ok(moved)
    }

    /// What preparing `target` changes and the repositories outside its
    /// destination, after finishing interrupted moves; refused unless every
    /// one of them can move and a root to link would be empty once they,
    /// and the files of the chats without a repository, have. Callers hold
    /// the registry gate.
    async fn movable_repositories(
        &self,
        daemon: &dyn Daemon,
        target: &ConfiguredRoot,
        running: Running,
    ) -> Result<(spotlight::Plan, Vec<Repository>)> {
        self.finish_moves(daemon).await?;
        let repositories = self.list().await?;
        let plan = self.root_plan(target, &repositories).await?;
        let root = plan.destination()?;
        let moving: Vec<_> = repositories
            .into_iter()
            .filter(|repo| outside(repo, &root))
            .collect();
        for repo in &moving {
            self.check_movable(daemon, repo, running).await?;
        }
        let outside = daemon.outside_dirs(&target.path);
        let directories: Vec<_> = (moving.iter())
            .map(|repo| repo.workspaces_dir.as_path())
            .chain(outside.iter().map(PathBuf::as_path))
            .collect();
        plan.ensure_linkable(&directories)?;
        Ok((plan, moving))
    }

    /// Finish or roll back every move a stopped daemon or failed attempt left.
    pub async fn finish_moves(&self, daemon: &dyn Daemon) -> Result<()> {
        for id in self.pending_moves().await? {
            let _fetch = self.gates.lock_fetch(&id).await;
            let _git = self.gates.lock_git(&id).await;
            let _files = self.gates.lock_message_files(&id).await;
            let _proposals = self.gates.lock_proposal_files().await;
            self.finish_move(daemon, &id)
                .await
                .context("finish an interrupted repository move")?;
        }
        Ok(())
    }

    /// What preparing `target` changes, refused when a location it creates or
    /// links is inside a directory that would move into it or a workspace
    /// placed elsewhere.
    async fn root_plan(
        &self,
        target: &ConfiguredRoot,
        repositories: &[Repository],
    ) -> Result<spotlight::Plan> {
        let workspaces = self.workspaces().await?;
        let recorded = Recorded {
            repositories,
            workspaces: &workspaces,
        };
        let state = fs::canonicalize(&self.paths.state)?;
        plan_root(target, &state, &recorded, None).await
    }

    /// Refuse a repository whose directory is missing or in use, or whose
    /// workspaces are busy or not verifiably its own.
    async fn check_movable(
        &self,
        daemon: &dyn Daemon,
        repo: &Repository,
        running: Running,
    ) -> Result<()> {
        self.ensure_available(&repo.id).await?;
        let directory = &repo.workspaces_dir;
        ensure!(
            directory.is_dir() && !directory.is_symlink(),
            "repository directory {} is missing",
            directory.display()
        );
        self.ensure_only_its_own(repo, directory).await?;
        self.movable_states(&repo.id, running).await?;
        daemon.ensure_no_reviews_in(&repo.id).await?;
        daemon.ensure_no_gate_runs_in(&repo.id).await?;
        let imports = ShoalDir::of(repo, Contents::Imports);
        daemon.ensure_no_import_in(imports).await?;
        let stopping = self.stopping_processes(&repo.id, running);
        let processes = process::in_directory_besides(directory, stopping).await?;
        ensure!(
            processes.is_empty(),
            "processes are using {}: {}; stop them or leave the directory first",
            directory.display(),
            processes.join(", ")
        );
        for workspace in self.repository_workspaces(&repo.id).await? {
            daemon.ensure_movable_workspace(&workspace).await?;
        }
        Ok(())
    }

    /// The processes that stopping the repository's commands and agents
    /// ends, when `running` has the caller stop them first.
    async fn stopping_processes(&self, id: &RepositoryId, running: Running) -> Result<Vec<u32>> {
        if running == Running::Refused {
            return Ok(Vec::new());
        }
        let executions = self.repository_executions(id).await?;
        let workspaces = self.repository_workspaces(id).await?;
        let agents = self.store.read(|db| store::agent_wrappers(db)).await?;
        execution_processes(&executions, &workspaces, &agents).await
    }

    /// Refuse a directory holding Shoal's state or another repository's
    /// checkout, directory or workspaces, whose records would not follow it.
    async fn ensure_only_its_own(&self, repo: &Repository, directory: &Path) -> Result<()> {
        ensure!(
            !fs::canonicalize(&self.paths.state)?.starts_with(directory),
            "{} contains Shoal's state directory",
            directory.display()
        );
        for other in self.list().await? {
            if other.id == repo.id {
                continue;
            }
            for path in [&other.path, &other.workspaces_dir] {
                ensure!(
                    !path.starts_with(directory),
                    "{} contains {}, which belongs to the repository {}",
                    directory.display(),
                    path.display(),
                    other.display_name()
                );
            }
        }
        for workspace in self.workspaces().await? {
            ensure!(
                workspace.repository() == &repo.id || !workspace.path.starts_with(directory),
                "{} contains the workspace {} of another repository",
                directory.display(),
                workspace.name
            );
        }
        Ok(())
    }

    async fn move_repository(
        &self,
        daemon: &dyn Daemon,
        repo: &Repository,
        root: &Path,
    ) -> Result<MovedRepository> {
        let _seed = self.stop_seed_build(&repo.id).await;
        let _fetch = self.gates.lock_fetch(&repo.id).await;
        let _git = self.gates.lock_git(&repo.id).await;
        let _files = self.gates.lock_message_files(&repo.id).await;
        // Nor may a proposal copy files into it meanwhile.
        let _proposals = self.gates.lock_proposal_files().await;
        let repo = self.repository(&repo.id).await?;
        self.check_movable(daemon, &repo, Running::Refused).await?;
        let workspaces = self.repository_workspaces(&repo.id).await?;
        let _resources = self.exclude_resource_hooks(&workspaces)?;
        let source = repo.workspaces_dir.clone();
        let destination = self.begin_move(&repo.id, root, &source).await?;
        let moved = async {
            fsutil::copy::move_directory(&source, &destination).await?;
            self.switch_move(&repo.id, &source, &destination).await
        }
        .await;
        let finished = self.finish_move(daemon, &repo.id).await;
        moved?;
        finished?;
        Ok(MovedRepository {
            repository: self.repository(&repo.id).await?,
            from: source,
            workspace_paths: workspaces.into_iter().map(|w| w.part.path).collect(),
        })
    }

    /// Keep permit hooks, which run in the workspaces, from starting.
    fn exclude_resource_hooks(&self, workspaces: &[Workspace]) -> Result<Vec<ResourceGuard>> {
        let mut guards = Vec::with_capacity(workspaces.len());
        for workspace in workspaces {
            guards.push(
                self.gates
                    .resource_guard(&workspace.id, GuardMode::Exclusive)?,
            );
        }
        Ok(guards)
    }

    /// Reserve the destination in `root` and record the move, giving up the
    /// reservation when the move cannot be recorded.
    async fn begin_move(&self, id: &RepositoryId, root: &Path, source: &Path) -> Result<PathBuf> {
        let destination = self.reserve_destination(root, source).await?;
        if let Err(error) = self.record_move(id, source, &destination).await {
            // The reservation belongs exclusively to this attempt.
            let _ = fs::remove_dir(&destination);
            return Err(error);
        }
        Ok(destination)
    }

    /// Reserve the source directory's name in `root`, suffixed when taken,
    /// including by a recorded workspace whose directory is gone.
    async fn reserve_destination(&self, root: &Path, source: &Path) -> Result<PathBuf> {
        let name = source
            .file_name()
            .context("repository directory has no name")?
            .to_str()
            .context("repository directory is not UTF-8")?;
        let workspaces: Vec<_> = (self.workspaces().await?.into_iter())
            .map(|workspace| workspace.part.path)
            .collect();
        reserve_directory(root, name, &self.list().await?, &workspaces)
    }

    async fn repository_executions(&self, repository_id: &RepositoryId) -> Result<Vec<Execution>> {
        let id = repository_id.to_owned();
        self.store
            .run(move |db| store::repository_executions(db, &id))
            .await
    }
}

/// Whether the repository's directory lies outside `root`.
fn outside(repo: &Repository, root: &Path) -> bool {
    repo.workspaces_dir.parent() != Some(root)
}

/// The processes of `executions`, running in `workspaces`, which stopping
/// them ends: those a stop would signal, their wrappers and the processes
/// that started the wrappers, such as a terminal session that closes with
/// them. The executions whose wrappers are `agents` own their agents.
async fn execution_processes(
    executions: &[Execution],
    workspaces: &[Workspace],
    agents: &[process::identity::Identity],
) -> Result<Vec<u32>> {
    let ids = executions.iter().map(|e| e.id.clone()).collect();
    let scan = process::identity::scan(ids).await?;
    let mut pids = Vec::new();
    for execution in executions {
        let workspace = workspaces
            .iter()
            .find(|workspace| workspace.id == execution.space_id)
            .context("an execution's workspace is missing")?;
        let processes =
            process::execution::Processes::inspect(execution, &workspace.path, &scan, agents)
                .await?;
        pids.extend(processes.owned.iter().map(|process| process.pid));
        if let Some(wrapper) = &processes.wrapper {
            pids.push(wrapper.pid);
            pids.extend(process::identity::parent_of(wrapper));
        }
    }
    Ok(pids)
}
