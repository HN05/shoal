//! Recording a new workspace, and creating, configuring and settling its
//! worktree.

use super::super::{
    restore, tree,
    workspace::{NewWorktreeOptions, availability, branches, ownership::claims_git_dir, stack},
};
use crate::Spaces;
use crate::daemon::Daemon;
use crate::{
    config::Config,
    fsutil::off_runtime,
    git::{self, default_branch::DefaultBranchLookup},
    model::{Head, Repository, SpaceKind, Workspace},
    protocol::CreationOptions,
    state::WorkspaceState,
    {spaces, store},
};
use anyhow::{Context, Result, bail, ensure};
pub(super) use pending::PendingWorktree;
pub use pending::{build_pending_in, pending_revisions};
use rusqlite::params;
use shoal_ids::{checkpoint_numbers::CheckpointNumber, space_ids::SpaceId};
use shoal_protocol::space_names::SpaceName;
use shoal_refs::{BranchName, CommitId};
use tokio::sync::OwnedMutexGuard;

#[derive(Debug, Default)]
pub struct NewWorkspace<'a> {
    pub git_profile: Option<&'a str>,
    pub path: Option<std::path::PathBuf>,
    pub options: CreationOptions,
    /// The space it goes below, a chat or a workspace. It is recorded with
    /// the workspace, so one whose creation fails is still below it.
    pub parent: Option<SpaceId>,
    /// The chat it replaces, taking its ID, number, name and place in the
    /// tree, instead of going below `parent`.
    pub converts: Option<crate::conversions::Converting>,
    /// The new branch takes exactly the requested name: a taken name refuses
    /// instead of getting a suffix.
    pub exact_branch: bool,
    /// The ID it keeps, as a restored space does; a new one otherwise.
    pub id: Option<SpaceId>,
    /// Leaves the worktree to the preparation, which creates it through
    /// [`crate::Spaces::create_pending_worktree`], so the record returns
    /// before Git runs. Otherwise the creation creates it as a preparation
    /// would.
    pub pending: bool,
    /// The commit that keeps the unsaved files the new branch starts with,
    /// as a copy another joined machine carried here does.
    pub files: Option<CommitId>,
}

pub(super) struct Configured {
    pub needs_setup: bool,
    /// The settings under which the worktree starts from the seed build.
    /// `None` for a worktree that takes no seed.
    pub seed: Option<crate::config::Effective>,
}

pub enum WorkspaceSource {
    New {
        base: Option<String>,
        parent: Option<SpaceId>,
    },
    /// A restore opens it at its removal checkpoint and writes the unsaved
    /// files the checkpoint keeps into it.
    Existing {
        branch: crate::git::existing_branch::Branch,
        base: Option<String>,
        restores: Option<CheckpointNumber>,
    },
}

impl Spaces {
    pub async fn create_workspace_with(
        &self,
        daemon: &dyn Daemon,
        repository: impl Into<crate::repository::Selector>,
        name: String,
        base: Option<String>,
        new: NewWorkspace<'_>,
    ) -> Result<Workspace> {
        let (repo, git) = daemon.lock_repository(repository.into()).await?;
        git::check_branch_name(&name)?;
        let branch = self.available_branch(&repo, &name).await?;
        ensure!(
            !new.exact_branch || branch == name,
            "branch {name} conflicts with an existing branch or workspace; name another"
        );
        self.create_branch_workspace(
            daemon,
            &repo,
            name,
            branch,
            WorkspaceSource::New { base, parent: None },
            new,
            git,
        )
        .await
    }

    /// Checks without creating anything. A missing branch is one the caller
    /// has yet to choose.
    pub async fn check_creation(
        &self,
        daemon: &dyn Daemon,
        repository: impl Into<crate::repository::Selector>,
        branch: Option<&str>,
        existing: Option<&str>,
        options: NewWorktreeOptions<'_>,
    ) -> Result<()> {
        let repo = daemon.repository(repository.into()).await?;
        daemon.ensure_available(&repo.id).await?;
        ensure_disposable_new(options.disposable, existing)?;
        let (name, opened) = match (branch, existing) {
            (Some(name), _) => {
                git::check_branch_name(name)?;
                (Some(name.to_owned()), None)
            }
            (None, Some(selector)) => {
                match self.open_target(daemon, &repo, selector, options).await? {
                    branches::OpenTarget::Reopen(workspace) => {
                        return self
                            .check_reopen(
                                daemon,
                                &workspace,
                                workspace.branch().map(BranchName::as_str),
                            )
                            .await;
                    }
                    branches::OpenTarget::Create(branch) => {
                        (Some(branch.name.to_string()), Some(branch))
                    }
                }
            }
            (None, None) => (None, None),
        };
        if let Some(profile) = options.git_profile {
            self.config
                .current()
                .machine
                .git_profiles
                .profile(profile)?;
        }
        if let Some(base) = options.base {
            let source = match (&name, &opened) {
                (_, Some(branch)) => BaseSource::Opened(branch),
                (Some(_), None) => BaseSource::New,
                (None, None) => BaseSource::Unsettled,
            };
            check_base(&repo, base, source).await?;
        }
        self.check_new_workspace_path(daemon, &repo, name.as_deref(), options.path)
            .await
    }

    /// Runs under the repository's Git gate `git`, which settling releases.
    #[expect(
        clippy::too_many_arguments,
        reason = "what the workspace is created from and with, beside the daemon"
    )]
    pub async fn create_branch_workspace(
        &self,
        daemon: &dyn Daemon,
        repo: &crate::model::Repository,
        name: String,
        branch: BranchName,
        source: WorkspaceSource,
        new: NewWorkspace<'_>,
        git: OwnedMutexGuard<()>,
    ) -> Result<Workspace> {
        let NewWorkspace {
            git_profile,
            path,
            options,
            parent: below,
            converts,
            exact_branch: _,
            id,
            pending,
            files,
        } = new;
        let existing = match &source {
            WorkspaceSource::Existing { .. } => Some(branch.as_str()),
            WorkspaceSource::New { .. } => None,
        };
        ensure_disposable_new(options.disposable, existing)?;
        if let Some(name) = git_profile {
            self.config.current().machine.git_profiles.profile(name)?;
        }
        let base_branch = match &source {
            WorkspaceSource::New { base, .. } | WorkspaceSource::Existing { base, .. } => {
                match base {
                    Some(base) => stack::base_branch(&repo.path, base).await?,
                    None => None,
                }
            }
        };
        let parent = match &source {
            WorkspaceSource::New { parent, .. } => parent.clone(),
            WorkspaceSource::Existing { .. } => None,
        }
        .or(below);
        let chosen = path.is_none();
        let path = self.new_workspace_path(daemon, repo, &name, path).await?;
        let name = match &options.name {
            Some(given) => given.checked()?,
            None => SpaceName::derive(&name, "workspace"),
        };
        let mut workspace = Workspace::record(
            id.unwrap_or_else(SpaceId::generate),
            repo.id.clone(),
            name,
            path,
            Head::Branch(branch),
            WorkspaceState::Preparing,
        );
        workspace.setup_skipped = options.skip_setup;
        workspace.disposable = options.disposable;
        let mode = converts.as_ref().map(|converting| converting.mode.clone());
        let placement = match converts {
            Some(converting) => Placement::Replacing(Box::new(converting)),
            None => Placement::Below(parent),
        };
        let record = Inserted {
            placement,
            chosen,
            pending: Some(PendingWorktree::of((source, files), git_profile, mode)),
        };
        let workspace = self
            .insert_workspace_as(workspace, base_branch, record)
            .await?;
        if pending {
            return self.workspace_by_id(&workspace.id).await;
        }
        self.create_recorded_worktree(daemon, repo, &workspace, git)
            .await
    }

    /// Releases the Git gate, starts a configured worktree from the seed
    /// build, then records how creation went. Once it went well, releases
    /// the carried commits its plan kept. Removal and setup wait for it to
    /// end.
    pub(super) async fn settle_creation(
        &self,
        daemon: &dyn Daemon,
        repo: &Repository,
        workspace: &Workspace,
        materialized: Result<Configured>,
        git: OwnedMutexGuard<()>,
    ) -> Result<Workspace> {
        let _creation = self.hold_creation(workspace, git).await;
        let materialized = self
            .seed_worktree(daemon, repo, workspace, materialized)
            .await;
        let created = self.record_creation(workspace, materialized).await?;
        self.release_carried(daemon, &repo.id).await;
        Ok(created)
    }

    /// Replaces the repository's Git gate `git`, so other starts' Git
    /// operations need not wait for the seed clone while removal and setup
    /// still do.
    pub(super) async fn hold_creation(
        &self,
        workspace: &Workspace,
        git: OwnedMutexGuard<()>,
    ) -> OwnedMutexGuard<()> {
        let creation = self.gates.lock_creation(&workspace.id).await;
        drop(git);
        creation
    }

    /// Returns whether setup is still needed.
    pub(super) async fn seed_worktree(
        &self,
        daemon: &dyn Daemon,
        repo: &Repository,
        workspace: &Workspace,
        materialized: Result<Configured>,
    ) -> Result<bool> {
        let configured = materialized?;
        if let Some(settings) = &configured.seed {
            daemon.apply_seed(repo, workspace, settings).await;
        }
        Ok(configured.needs_setup)
    }

    /// Ready, preparing while it needs setup, or failed.
    pub(super) async fn record_creation(
        &self,
        workspace: &Workspace,
        materialized: Result<bool>,
    ) -> Result<Workspace> {
        match materialized {
            Ok(needs_setup) => {
                self.finish_materialization(&workspace.id, needs_setup && !workspace.setup_skipped)
                    .await?
            }
            Err(error) => {
                self.set_state(
                    &workspace.id,
                    WorkspaceState::Failed,
                    Some(format!("{error:#}")),
                )
                .await?;
                let (name, number) = (&workspace.name, workspace.number);
                bail!(
                    "creating the worktree of {name} failed: {error:#}; `shoal setup {number}` retries it, and `shoal rm {number}` removes the workspace"
                );
            }
        }
        self.workspace_by_id(&workspace.id).await
    }

    /// Stacks the workspace on the workspace that owns `base_branch`.
    /// `pending` is the worktree it is still to get, or none for a worktree
    /// it adopts. Refuses a parent chat being removed, which takes no new
    /// workspace, and, when `chosen`, a directory Shoal chose whose name
    /// another workspace took meanwhile. Returns the record with the number
    /// it took.
    pub(super) async fn insert_workspace(
        &self,
        record: Workspace,
        base_branch: Option<BranchName>,
        parent: Option<SpaceId>,
        pending: Option<PendingWorktree>,
        chosen: bool,
    ) -> Result<Workspace> {
        let inserted = Inserted {
            placement: Placement::Below(parent),
            chosen,
            pending,
        };
        self.insert_workspace_as(record, base_branch, inserted)
            .await
    }

    /// `inserted` says whether it goes below a parent or in place of the
    /// chat it converts.
    async fn insert_workspace_as(
        &self,
        record: Workspace,
        base_branch: Option<BranchName>,
        Inserted {
            placement,
            chosen,
            pending,
        }: Inserted,
    ) -> Result<Workspace> {
        let [pending, pending_source, pending_checkpoint] =
            pending::rows::values(pending.as_ref())?;
        let record = match &placement {
            Placement::Replacing(converting) => converting.workspace(record),
            Placement::Below(_) => record,
        };
        ensure!(
            (pending != rusqlite::types::Value::Null) == record.checkout.is_pending(),
            "a workspace records the plan of its worktree exactly while it is pending"
        );
        let marker = match record.checkout.git_dir().map(std::path::Path::to_owned) {
            Some(git_dir) => {
                off_runtime::run(move || shoal_daemon_base::ownership::marked_owner(&git_dir))
                    .await?
            }
            None => None,
        };
        let (removing, records) = (self.space_removals.clone(), self.records.clone());
        self.store
            .write(move |tx| {
                let replaced = match &placement {
                    Placement::Replacing(_) => Some(&record.id),
                    Placement::Below(_) => None,
                };
                availability::ensure_branch_available(
                    tx,
                    record.repository(),
                    record.branch().map(BranchName::as_str),
                    replaced,
                )?;
                store::ensure_path_unrecorded(tx, &record.path)?;
                if chosen {
                    availability::ensure_directory_free(tx, &record.path)?;
                }
                availability::ensure_path_outside_moves(tx, &record.path)?;
                let workspaces = tx.prepare(&format!("SELECT {} FROM spaces JOIN workspaces USING(id)", store::WORKSPACE_COLUMNS))?
                    .query_map([], store::workspace)?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                for workspace in workspaces {
                    let claimed = record.checkout.git_dir().is_some_and(|git_dir| {
                        claims_git_dir(&workspace, git_dir, marker.as_deref())
                    });
                    ensure!(!claimed,
                        "Git worktree is already owned by {}; restore its recorded path", workspace.name);
                }
                let base_workspace = match &base_branch {
                    Some(branch) => stack::owner(tx, record.repository(), branch)?,
                    None => None,
                };
                // Recorded first, so the workspace's creation names it.
                if let Placement::Below(Some(parent)) = &placement {
                    ensure!(
                        !removing.reserved(parent),
                        "the space the workspace would go below is being removed"
                    );
                    tree::place_below(tx, record.repository_id.as_ref(), &record.id, parent)?;
                }
                let (branch, start_commit, made_for) = columns::head_columns(&record.head)?;
                let (base_commit, base_ref, git_dir) = columns::checkout_columns(&record.checkout);
                tx.execute(
                    "INSERT INTO workspaces (id,path,branch,state,git_dir,base_commit,base_ref,setup_finished,observed_branch,base_workspace_id,start_commit,made_for,setup_skipped,disposable,pending_plan,pending_source,pending_checkpoint) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?3,?9,?10,?11,?12,?13,?14,?15,?16)",
                    params![
                        record.id,
                        record.path.to_str(),
                        branch,
                        record.state,
                        git_dir,
                        base_commit,
                        base_ref,
                        record.state == WorkspaceState::Ready,
                        base_workspace,
                        start_commit,
                        made_for,
                        record.setup_skipped,
                        record.disposable,
                        pending,
                        pending_source,
                        pending_checkpoint,
                    ],
                )?;
                let number = match &placement {
                    Placement::Replacing(converting) => {
                        converting.replace_chat(tx)?;
                        record.number
                    }
                    Placement::Below(_) => spaces::records::record(tx, &*records, &spaces::records::NewSpace {
                        id: &record.id,
                        kind: SpaceKind::Workspace,
                        repository_id: record.repository_id.as_ref(),
                        name: &record.name,
                        number: record.number,
                        mode: record.mode.as_ref(),
                        narrowing: &record.narrowing,
                        created_at: crate::time::UnixTime::now(),
                    })?,
                };
                if let Some(repository) = &record.repository_id {
                    records.touch_repository(tx, repository)?;
                }
                Ok(Workspace { number, ..record })
            })
            .await
    }

    /// A restore's `files` go in before the workspace can be used.
    pub(super) async fn materialize_worktree(
        &self,
        daemon: &dyn Daemon,
        repo: &crate::model::Repository,
        workspace: &Workspace,
        source: WorkspaceSource,
        files: Option<&restore::RestoredFiles>,
        git_profile: Option<&str>,
    ) -> Result<Configured> {
        if let WorkspaceSource::Existing { branch, .. } = &source {
            self.materialize_branch(repo, branch).await?;
        }
        let base = self.resolve_worktree_base(repo, workspace, &source).await?;
        let commit = self.record_worktree_base(repo, workspace, &base).await?;
        let new_branch = matches!(&source, WorkspaceSource::New { .. })
            && creates_branch(repo, workspace).await?;
        git::worktree::add(
            &repo.path,
            &workspace.path,
            workspace.require_branch("creating a branch worktree")?,
            new_branch.then_some(commit.as_str()),
        )
        .await?;
        self.configure_worktree(daemon, workspace, files, git_profile)
            .await
    }

    /// Records the worktree's identity, writes a restore's files into it,
    /// then applies its Git profile. Settling does the seed build.
    pub(super) async fn configure_worktree(
        &self,
        daemon: &dyn Daemon,
        workspace: &Workspace,
        files: Option<&restore::RestoredFiles>,
        git_profile: Option<&str>,
    ) -> Result<Configured> {
        self.record_worktree_identity(workspace).await?;
        if let Some(files) = files {
            restore::write_files(&workspace.path, files).await?;
        }
        let config = self.config.current();
        let settings = daemon.workspace_settings_from(&config, workspace).await?;
        self.apply_workspace_git_profile(workspace, &config, &settings, git_profile)
            .await?;
        Ok(Configured {
            needs_setup: settings.setup.command.is_some()
                || settings.hooks.pre_setup.is_some()
                || settings.setup.copy_ignored,
            seed: Some(settings),
        })
    }

    async fn resolve_worktree_base(
        &self,
        repo: &Repository,
        workspace: &Workspace,
        source: &WorkspaceSource,
    ) -> Result<String> {
        match source {
            WorkspaceSource::Existing { base, .. } => {
                self.existing_branch_base(
                    repo,
                    workspace.require_branch("opening a branch")?,
                    base.as_deref(),
                )
                .await
            }
            WorkspaceSource::New { base, .. } => self.new_branch_base(repo, base.as_deref()).await,
        }
    }

    async fn existing_branch_base(
        &self,
        repo: &Repository,
        branch: &BranchName,
        base: Option<&str>,
    ) -> Result<String> {
        match base {
            Some(base) => Ok(base.to_owned()),
            None => existing_base(repo, branch).await,
        }
    }

    /// The local default branch as the background sync left it, or a named
    /// base, fetching a remote branch it names.
    pub(super) async fn new_branch_base(
        &self,
        repo: &Repository,
        base: Option<&str>,
    ) -> Result<String> {
        let Some(base) = base else {
            let default =
                crate::git::default_branch::resolve(&repo.path, DefaultBranchLookup::Discover)
                    .await?;
            return Ok(git::local_ref(&default));
        };
        let remotes = git::remote_names(&repo.path).await?;
        // An explicit ref stays an escape hatch when remote default-branch
        // discovery is unavailable. Only a remote branch it names is
        // fetched.
        let lookup = DefaultBranchLookup::Cached;
        if let Ok(default) =
            crate::git::default_branch::locate_among(&repo.path, &remotes, lookup).await
        {
            let default_ref = git::local_ref(&default.name);
            if default.name == base || default_ref == base {
                return Ok(default_ref);
            }
        }
        if let Some((remote, branch)) = git::remote_branch_among(&repo.path, base, &remotes).await?
        {
            return git::fetch::configured_branch(&repo.path, &remote, &branch)
                .await
                .with_context(|| {
                    format!("could not fetch {remote}/{branch} before creating workspace; pass a commit to start from the local state")
                });
        }
        Ok(base.to_owned())
    }

    /// Records the base before Git creates the worktree.
    pub(super) async fn record_worktree_base(
        &self,
        repo: &Repository,
        workspace: &Workspace,
        base: &str,
    ) -> Result<String> {
        let (commit, reference) = recorded_base(repo, base).await?;
        let (id, recorded_commit) = (workspace.id.clone(), commit.clone());
        self.store
            .run(move |db| {
                db.execute(
                    "UPDATE workspaces SET base_commit=?2,base_ref=?3 WHERE id=?1",
                    params![id, recorded_commit, reference],
                )?;
                Ok(())
            })
            .await?;
        Ok(commit)
    }

    async fn apply_workspace_git_profile(
        &self,
        workspace: &Workspace,
        config: &Config,
        settings: &crate::config::Effective,
        git_profile: Option<&str>,
    ) -> Result<()> {
        if let Some(name) = git_profile.or(settings.git_profile.as_deref()) {
            crate::git_profile::apply(&workspace.path, config.machine.git_profiles.profile(name)?)
                .await
                .with_context(|| format!("apply git profile {name}"))?;
        }
        Ok(())
    }

    async fn finish_materialization(&self, id: &SpaceId, needs_setup: bool) -> Result<()> {
        let id = id.to_owned();
        self.store
            .write(move |db| {
                db.execute(
                    "UPDATE workspaces SET state=?2,error=NULL,setup_finished=?3 AND NOT setup_skipped,
                        pending_plan=NULL,pending_source=NULL,pending_checkpoint=NULL WHERE id=?1",
                    params![
                        id,
                        if needs_setup {
                            WorkspaceState::Preparing
                        } else {
                            WorkspaceState::Ready
                        },
                        !needs_setup
                    ],
                )?;
                Ok(())
            })
            .await
    }
}

struct Inserted {
    placement: Placement,
    chosen: bool,
    pending: Option<PendingWorktree>,
}

enum Placement {
    Below(Option<SpaceId>),
    Replacing(Box<crate::conversions::Converting>),
}

/// Its removal would delete a branch the user had before.
fn ensure_disposable_new(disposable: bool, existing: Option<&str>) -> Result<()> {
    match (disposable, existing) {
        (true, Some(branch)) => {
            bail!("--disposable applies only to a new branch; {branch} already exists")
        }
        _ => Ok(()),
    }
}

/// True when the workspace has a branch that no earlier, cut-short try
/// created. The record reserved its name, which no branch had then.
pub(super) async fn creates_branch(repo: &Repository, workspace: &Workspace) -> Result<bool> {
    match workspace.branch() {
        Some(branch) => Ok(!git::ref_exists(&repo.path, &git::local_ref(branch)).await?),
        None => Ok(false),
    }
}

/// As [`creates_branch`] says, but refuses a branch an earlier try created
/// at another commit, which the copied files would not fit.
pub(super) async fn creates_branch_at(
    repo: &Repository,
    workspace: &Workspace,
    head: &str,
) -> Result<bool> {
    let creates = creates_branch(repo, workspace).await?;
    if let (false, Some(branch)) = (creates, workspace.branch()) {
        let tip =
            git::resolve_commit(&repo.path, &git::local_ref(branch), git::run_isolated).await?;
        ensure!(
            tip.as_str() == head,
            "branch {branch}, which an earlier try created at {tip}, no longer starts where the copy does, at {head}; delete it with `git branch -D {branch}`, then try again"
        );
    }
    Ok(creates)
}

/// The commit and full ref name `shoal diff` records for a workspace base.
/// A base that names no ref, such as a commit, records no ref.
pub(super) async fn recorded_base(
    repo: &crate::model::Repository,
    base: &str,
) -> Result<(String, Option<String>)> {
    let (commit, reference) =
        git::resolve_named_commit(&repo.path, base, git::run_isolated).await?;
    Ok((
        commit.into(),
        reference.starts_with("refs/").then_some(reference),
    ))
}

enum BaseSource<'a> {
    New,
    Opened(&'a git::existing_branch::Branch),
    /// Not chosen yet: either may follow, and the settled request is checked
    /// again.
    Unsettled,
}

/// Opening a remote branch fetches and creates it before resolving the
/// base, which may name it in any form, so only creation can check that
/// base.
async fn check_base(
    repo: &crate::model::Repository,
    base: &str,
    source: BaseSource<'_>,
) -> Result<()> {
    match source {
        BaseSource::New => check_new_branch_base(repo, base).await,
        BaseSource::Opened(branch) if branch.remote.is_some() => Ok(()),
        BaseSource::Opened(_) => check_local_base(repo, base).await,
        BaseSource::Unsettled => match check_local_base(repo, base).await {
            Ok(()) => Ok(()),
            Err(_) => check_new_branch_base(repo, base).await,
        },
    }
}

/// A new branch first fetches a remote branch base, which the remote must
/// have. Any other base must resolve here.
async fn check_new_branch_base(repo: &crate::model::Repository, base: &str) -> Result<()> {
    if let Some((remote, branch)) = git::remote_branch(&repo.path, base).await? {
        ensure!(
            git::remote_has_branch(&repo.path, &remote, &branch).await?,
            "base {base} is not a branch on {remote}"
        );
        return Ok(());
    }
    check_local_base(repo, base).await
}

/// An opened local branch uses its base without fetching.
async fn check_local_base(repo: &crate::model::Repository, base: &str) -> Result<()> {
    git::resolve_commit(&repo.path, base, git::run_isolated)
        .await
        .with_context(|| format!("base {base} is not a commit"))?;
    Ok(())
}

/// A live local default ref, or the commit the worktree opened at.
pub async fn existing_base(repo: &crate::model::Repository, branch: &BranchName) -> Result<String> {
    if let Ok(name) =
        crate::git::default_branch::resolve(&repo.path, DefaultBranchLookup::Cached).await
    {
        let default = git::local_ref(&name);
        if name != *branch && git::ref_exists(&repo.path, &default).await? {
            return Ok(default);
        }
    }
    Ok(
        git::verify_revision(&repo.path, &git::local_ref(branch), git::run_isolated)
            .await?
            .trim()
            .to_owned(),
    )
}

mod columns;
mod pending;

#[cfg(test)]
mod tests;
