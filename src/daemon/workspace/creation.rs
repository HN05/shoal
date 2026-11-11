//! Worktree creation: recording a new workspace and creating, configuring
//! and settling its worktree.

use super::{
    Manager, NewWorktreeOptions, branches, identity, ownership::owns_git_dir, restore, stack, tree,
};
use crate::client::space_ids::SpaceId;
use crate::{client::space_names::SpaceName, model::SpaceKind};
use crate::{
    config::Config,
    daemon::store,
    fsutil::off_runtime,
    git::{self, default_branch::DefaultBranchLookup, worktrunk},
    model::{Repository, Workspace},
    protocol::CreationOptions,
    state::WorkspaceState,
};
use anyhow::{Context, Result, bail, ensure};
use rusqlite::params;

/// Where a new workspace goes, its Git profile, and what it leaves out.
#[derive(Debug, Default)]
pub(crate) struct NewWorkspace<'a> {
    pub git_profile: Option<&'a str>,
    pub path: Option<std::path::PathBuf>,
    pub options: CreationOptions,
    /// The space it goes below, a chat or a workspace, recorded with it so
    /// that one whose creation fails is still there.
    pub parent: Option<SpaceId>,
    /// The chat it replaces, taking its ID, number, name and place in the
    /// tree, instead of going below `parent`.
    pub converts: Option<crate::daemon::conversions::Converting>,
    /// Its name and directory's name, in place of those its branch gives.
    pub name: Option<SpaceName>,
}

pub(super) enum WorkspaceSource {
    /// A new branch from an optional base ref, split from an optional parent
    /// workspace.
    New {
        base: Option<String>,
        parent: Option<SpaceId>,
    },
    /// An existing branch, with an optional base ref overriding the default
    /// branch, and the unsaved files a restore writes into it.
    Existing {
        branch: crate::git::existing_branch::Branch,
        base: Option<String>,
        files: Option<restore::RestoredFiles>,
    },
}

impl Manager {
    /// Create the workspace `name` of the repository on a new branch from
    /// `base`, where and as `new` says.
    pub(crate) async fn create_workspace_with(
        &self,
        repository: impl Into<crate::repository::Selector>,
        name: String,
        base: Option<String>,
        new: NewWorkspace<'_>,
    ) -> Result<Workspace> {
        let (repo, _guard) = self.lock_repository(repository).await?;
        git::check_branch_name(Some(&repo.path), &name).await?;
        let branch = self.available_branch(&repo, &name).await?;
        self.create_branch_workspace(
            &repo,
            name,
            branch,
            WorkspaceSource::New { base, parent: None },
            new,
        )
        .await
    }

    /// Check what creating or opening a workspace would refuse in its request
    /// without creating anything: the repository, the new `branch` or the
    /// `existing` one, and the new-worktree options. A missing branch is the
    /// one the caller has yet to choose.
    pub async fn check_creation(
        &self,
        repository: impl Into<crate::repository::Selector>,
        branch: Option<&str>,
        existing: Option<&str>,
        options: NewWorktreeOptions<'_>,
    ) -> Result<()> {
        let repo = self.repository(repository).await?;
        self.ensure_repository_available(&repo.id).await?;
        ensure_disposable_new(options.disposable, existing)?;
        let (name, opened) = match (branch, existing) {
            (Some(name), _) => {
                git::check_branch_name(Some(&repo.path), name).await?;
                (Some(name.to_owned()), None)
            }
            (None, Some(selector)) => match self.open_target(&repo, selector, options).await? {
                branches::OpenTarget::Reopen(workspace) => {
                    return self
                        .check_reopen(&workspace, workspace.branch.as_deref())
                        .await;
                }
                branches::OpenTarget::Create(branch) => (Some(branch.name.clone()), Some(branch)),
            },
            (None, None) => (None, None),
        };
        if let Some(profile) = options.git_profile {
            self.config().machine.git_profiles.profile(profile)?;
        }
        if let Some(base) = options.base {
            let source = match (&name, &opened) {
                (_, Some(branch)) => BaseSource::Opened(branch),
                (Some(_), None) => BaseSource::New,
                (None, None) => BaseSource::Unsettled,
            };
            check_base(&repo, base, source).await?;
        }
        let name = name.map(|name| SpaceName::derive(SpaceKind::Workspace, &name));
        if let Some(name) = name.clone() {
            self.store
                .run(move |db| store::ensure_name_available(db, &name, None))
                .await?;
        }
        self.check_new_workspace_path(&repo, name.as_deref(), options.path)
            .await
    }

    // The caller holds the per-repository Git gate through materialization.
    pub(super) async fn create_branch_workspace(
        &self,
        repo: &crate::model::Repository,
        name: String,
        branch: String,
        source: WorkspaceSource,
        new: NewWorkspace<'_>,
    ) -> Result<Workspace> {
        let NewWorkspace {
            git_profile,
            path,
            options,
            parent: below,
            converts,
            name: given_name,
        } = new;
        let existing = match &source {
            WorkspaceSource::Existing { .. } => Some(branch.as_str()),
            WorkspaceSource::New { .. } => None,
        };
        ensure_disposable_new(options.disposable, existing)?;
        if let Some(name) = git_profile {
            self.config().machine.git_profiles.profile(name)?;
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
        let name = given_name.unwrap_or_else(|| SpaceName::derive(SpaceKind::Workspace, &name));
        let path = self.new_workspace_path(repo, &name, path).await?;
        let workspace = Workspace {
            setup_skipped: options.skip_setup,
            disposable: options.disposable,
            ..Workspace::new_record(
                repo.id.clone(),
                name,
                path,
                Some(branch),
                WorkspaceState::Preparing,
            )
        };
        let converted_mode = converts.as_ref().map(|converting| converting.mode.clone());
        let placement = match converts {
            Some(converting) => Placement::Replacing(Box::new(converting)),
            None => Placement::Below(parent),
        };
        let workspace = self
            .insert_workspace_as(workspace, base_branch, placement)
            .await?;
        let mut result = self
            .materialize_worktree(repo, &workspace, source, git_profile)
            .await;
        // Before the workspace is ready, so nothing uses one that goes.
        if let (Ok(_), Some(mode)) = (&result, &converted_mode)
            && let Err(error) = self.ensure_worktree_mode(&workspace, mode).await
        {
            result = Err(error);
        }
        self.settle_creation(&workspace, result).await
    }

    /// Record how materializing the recorded `workspace` went: ready, or
    /// preparing while it needs setup, or failed.
    pub(super) async fn settle_creation(
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
                bail!(
                    "workspace {} failed; inspect or remove it with Shoal: {error:#}",
                    workspace.name
                );
            }
        }
        self.workspace_by_id(&workspace.id).await
    }

    /// Record a new workspace, stacked on the workspace that owns `base_branch`
    /// and below `parent`, refusing a parent chat being removed, which takes
    /// no new workspace. Returns the record with the number it took.
    pub(super) async fn insert_workspace(
        &self,
        record: Workspace,
        base_branch: Option<String>,
        parent: Option<SpaceId>,
    ) -> Result<Workspace> {
        self.insert_workspace_as(record, base_branch, Placement::Below(parent))
            .await
    }

    /// Record a new workspace as [`Self::insert_workspace`] does, below a
    /// parent or in place of the chat it converts, as `placement` says.
    async fn insert_workspace_as(
        &self,
        record: Workspace,
        base_branch: Option<String>,
        placement: Placement,
    ) -> Result<Workspace> {
        let record = match &placement {
            Placement::Replacing(converting) => converting.workspace(record),
            Placement::Below(_) => record,
        };
        let marker = match record.git_dir.clone() {
            Some(git_dir) => off_runtime::run(move || identity::marked_owner(&git_dir)).await?,
            None => None,
        };
        let removing = self.chat_removals.clone();
        self.store
            .write(move |tx| {
                let replaced = match &placement {
                    Placement::Replacing(_) => Some(&record.id),
                    Placement::Below(_) => None,
                };
                store::ensure_name_and_branch_available(
                    tx,
                    &record.repository_id,
                    &record.name,
                    record.branch.as_deref(),
                    replaced,
                )?;
                store::ensure_path_unrecorded(tx, &record.path)?;
                store::ensure_path_outside_moves(tx, &record.path)?;
                let workspaces = tx.prepare(&format!("SELECT {} FROM workspaces", store::WORKSPACE_COLUMNS))?
                    .query_map([], store::workspace)?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                for workspace in workspaces {
                    let owns = match record.git_dir.as_ref() {
                        Some(git_dir) => owns_git_dir(
                            &workspace,
                            git_dir,
                            record.git_dir_id.as_deref(),
                            marker.as_deref(),
                        )?,
                        None => false,
                    };
                    ensure!(!owns,
                        "Git worktree is already owned by {}; restore its recorded path", workspace.name);
                }
                let base_workspace = match &base_branch {
                    Some(branch) => stack::owner(tx, &record.repository_id, branch)?,
                    None => None,
                };
                // Recorded first, so the workspace's creation names it.
                if let Placement::Below(Some(parent)) = &placement {
                    ensure!(
                        !removing.reserved(parent),
                        "the chat the workspace would go below is being removed"
                    );
                    tree::ensure_parent(tx, Some(&record.repository_id), &record.id, parent)?;
                    tree::set_parent(tx, &record.id, Some(parent))?;
                }
                tx.execute(
                    "INSERT INTO workspaces (id,repository_id,name,path,branch,state,git_dir,git_dir_id,base_commit,base_ref,setup_finished,observed_branch,base_workspace_id,start_commit,made_for,setup_skipped,disposable,number,mode,narrowing) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?5,?12,?13,?14,?15,?16,NULLIF(?17,0),?18,?19)",
                    params![
                        record.id,
                        record.repository_id,
                        record.name,
                        record.path.to_str(),
                        record.branch,
                        record.state,
                        record.git_dir.as_ref().and_then(|p| p.to_str().map(str::to_owned)),
                        record.git_dir_id,
                        record.base_commit,
                        record.base_ref,
                        record.state == WorkspaceState::Ready,
                        base_workspace,
                        record.at.as_ref().map(|at| &at.commit),
                        record.at.as_ref().and_then(|at| at.made_for.as_ref()).map(serde_json::to_string).transpose()?,
                        record.setup_skipped,
                        record.disposable,
                        record.number,
                        record.mode,
                        store::narrowing_column(&record.narrowing)?,
                    ],
                )?;
                if let Placement::Replacing(converting) = &placement {
                    converting.replace_chat(tx)?;
                }
                tx.execute(
                    "UPDATE repositories SET last_used=(SELECT COALESCE(MAX(last_used),0)+1 FROM repositories) WHERE id=?1",
                    [&record.repository_id],
                )?;
                let number = tx.query_row(
                    "SELECT number FROM workspaces WHERE id=?1",
                    [&record.id],
                    |row| row.get(0),
                )?;
                Ok(Workspace { number, ..record })
            })
            .await
    }

    /// Create and configure the worktree, returning whether setup is still
    /// needed. A restore's files go in before the workspace can be used.
    async fn materialize_worktree(
        &self,
        repo: &crate::model::Repository,
        workspace: &Workspace,
        source: WorkspaceSource,
        git_profile: Option<&str>,
    ) -> Result<bool> {
        if let WorkspaceSource::Existing { branch, .. } = &source {
            self.materialize_branch(repo, branch).await?;
        }
        let base = self.resolve_worktree_base(repo, workspace, &source).await?;
        let commit = self.record_worktree_base(repo, workspace, &base).await?;
        worktrunk::create(
            &repo.path,
            &self.paths.worktrunk_config(),
            &workspace.path,
            workspace.require_branch("creating a branch worktree")?,
            matches!(&source, WorkspaceSource::New { .. }).then_some(commit.as_str()),
        )
        .await?;
        let files = match &source {
            WorkspaceSource::Existing { files, .. } => files.as_ref(),
            WorkspaceSource::New { .. } => None,
        };
        self.configure_worktree(repo, workspace, files, git_profile)
            .await
    }

    /// Record the new worktree's identity, write a restore's files into it,
    /// then apply its Git profile and the seed build, returning whether setup
    /// is still needed.
    pub(super) async fn configure_worktree(
        &self,
        repo: &Repository,
        workspace: &Workspace,
        files: Option<&restore::RestoredFiles>,
        git_profile: Option<&str>,
    ) -> Result<bool> {
        self.record_worktree_identity(workspace).await?;
        if let Some(files) = files {
            restore::write_files(&workspace.path, files).await?;
        }
        let config = self.config();
        let settings = self.workspace_settings_from(&config, workspace).await?;
        self.apply_workspace_git_profile(workspace, &config, &settings, git_profile)
            .await?;
        self.apply_seed(repo, workspace, &settings).await;
        Ok(settings.setup.command.is_some()
            || settings.hooks.pre_setup.is_some()
            || settings.setup.copy_ignored)
    }

    /// Select the effective base for the source branch.
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
        branch: &str,
        base: Option<&str>,
    ) -> Result<String> {
        match base {
            Some(base) => Ok(base.to_owned()),
            None => existing_base(repo, branch).await,
        }
    }

    /// Select a new branch's base: the local default branch as the
    /// background sync left it, or a named base, fetching a remote branch it
    /// names.
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
        // An explicit ref remains an escape hatch when remote default-branch
        // discovery is unavailable; only a remote branch it names is fetched.
        if let Ok(default) =
            crate::git::default_branch::resolve(&repo.path, DefaultBranchLookup::Cached).await
        {
            let default_ref = git::local_ref(&default);
            if default == base || default_ref == base {
                return Ok(default_ref);
            }
        }
        if let Some((remote, branch)) = git::remote_branch(&repo.path, base).await? {
            return git::fetch::configured_branch(&repo.path, &remote, &branch)
                .await
                .with_context(|| {
                    format!("could not fetch {remote}/{branch} before creating workspace; pass a commit to start from the local state")
                });
        }
        Ok(base.to_owned())
    }

    /// Record the base before Worktrunk creates the worktree.
    async fn record_worktree_base(
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
            .run(move |db| {
                db.execute(
                    "UPDATE workspaces SET state=?2,error=NULL,setup_finished=?3 AND NOT setup_skipped WHERE id=?1",
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

/// Where a new workspace's record goes.
enum Placement {
    /// Below the space it names, or at the top.
    Below(Option<SpaceId>),
    /// In place of the chat it converts.
    Replacing(Box<crate::daemon::conversions::Converting>),
}

/// Refuse a disposable workspace on the `existing` branch, which its removal
/// would delete although the user had it before.
fn ensure_disposable_new(disposable: bool, existing: Option<&str>) -> Result<()> {
    match (disposable, existing) {
        (true, Some(branch)) => {
            bail!("--disposable applies only to a new branch; {branch} already exists")
        }
        _ => Ok(()),
    }
}

/// The commit and full ref name `shoal diff` records for a workspace base;
/// a base that names no ref, such as a commit, records no ref.
pub(super) async fn recorded_base(
    repo: &crate::model::Repository,
    base: &str,
) -> Result<(String, Option<String>)> {
    let commit = git::resolve_commit(&repo.path, base, git::run_isolated).await?;
    let reference = git::symbolic_revision(&repo.path, base, git::run_isolated).await?;
    let reference = reference.trim_end_matches('\n');
    Ok((
        commit,
        reference.starts_with("refs/").then(|| reference.to_owned()),
    ))
}

/// The branch a creation request's base belongs to.
enum BaseSource<'a> {
    New,
    Opened(&'a git::existing_branch::Branch),
    /// Not chosen yet: either may follow, and the settled request is checked
    /// again.
    Unsettled,
}

/// Refuse a base creation could not use for `source`. Opening a remote branch
/// fetches and creates it before resolving the base, which may name it in any
/// form, so only creation can check that base.
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
/// have; any other base must resolve here.
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

/// Existing worktrees use a live local default ref, or their opening commit.
pub(super) async fn existing_base(repo: &crate::model::Repository, branch: &str) -> Result<String> {
    if let Ok(name) =
        crate::git::default_branch::resolve(&repo.path, DefaultBranchLookup::Cached).await
    {
        let default = git::local_ref(&name);
        if name != branch && git::ref_exists(&repo.path, &default, git::isolated_command).await? {
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

#[cfg(test)]
mod tests;
