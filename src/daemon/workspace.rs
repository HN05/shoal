//! Shared daemon state, workspace lookup, and worktree creation.
mod adoption;
pub(crate) mod agent_queue;
pub(crate) mod agent_state;
mod agent_turns;
mod agents;
mod branches;
pub(crate) mod capacity;
mod checkpoints;
mod commits;
mod config_state;
pub(crate) mod conflicts;
mod contributions;
mod daemon_exit;
mod demos;
mod diff;
pub(crate) mod done;
mod environment;
mod executions;
mod fast_forward;
mod gates;
mod holds;
pub(super) mod identity;
mod kept_branches;
mod lifecycle;
mod location;
mod merge;
mod overload_recovery;
mod ownership;
pub(crate) mod paths;
pub(crate) mod places;
mod pr_gate;
pub(crate) mod proxy_retries;
mod registry;
mod rename;
mod repo_configuration;
mod repo_move;
mod repo_removal;
mod restore;
pub(crate) mod review;
pub(crate) mod runner;
mod sessions;
mod shared_scans;
mod shutdown_stop;
mod signals;
pub(crate) mod spotlight;
mod stack;
mod status;
mod stopped_work;
mod title;
mod tree;
pub(crate) mod turn_retry;
mod usage;
mod xcode;

pub(crate) use agent_queue::Grant;
pub use agents::RestartWaits;
pub(crate) use agents::{Pause, Protection};
pub(crate) use branches::NewWorktreeOptions;
pub(crate) use daemon_exit::Ending;
pub(crate) use executions::{Completing, ExecutionRequest, ReattachedExecution, StartedExecution};
pub use executions::{ExecutionKind, StopRequest, UnprovenStop};
pub(crate) use gates::{GuardMode, PrGuard, ResourceGuard};
pub(crate) use holds::place_approved as place_approved_hold;
pub(crate) use ownership::VerifiedWorktree;
pub(crate) use restore::removal_notice;
pub(crate) use stack::record_base_changed;
pub use stopped_work::Discarded;
pub(crate) use tree::{
    Source, TrackedAgents, children, deciders, delegated, ensure_parent, parent, parent_id,
    parent_in_repository, receiving_paths, set_parent, spaces_above,
};

use crate::daemon::refusal::{Reason, Refusal};
use crate::{
    config::Config,
    daemon::store::{self, Store},
    fsutil::off_runtime,
    git::{self, default_branch::DefaultBranchLookup, worktrunk},
    model::{Execution, Inspection, Repository, Workspace},
    paths::Paths,
    protocol::CreationOptions,
    state::WorkspaceState,
};
use anyhow::{Context, Result, bail, ensure};
use futures_util::StreamExt as _;
use ownership::owns_git_dir;
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use std::{collections::HashSet, fs, sync::Arc};
use tokio::sync::OwnedMutexGuard;

/// Review checks a listing runs at once, each reading HEAD with one Git
/// process: enough to overlap their waits without one listing taking a
/// process per workspace.
const REVIEW_CHECK_CONCURRENCY: usize = 8;

/// Where a new workspace goes, its Git profile, and what it leaves out.
#[derive(Debug, Default)]
pub(crate) struct NewWorkspace<'a> {
    pub git_profile: Option<&'a str>,
    pub path: Option<std::path::PathBuf>,
    pub options: CreationOptions,
}

enum WorkspaceSource {
    /// A new branch from an optional base ref, split from an optional parent
    /// workspace.
    New {
        base: Option<String>,
        parent: Option<String>,
    },
    /// An existing branch, with an optional base ref overriding the default
    /// branch, and the unsaved files a restore writes into it.
    Existing {
        branch: crate::git::existing_branch::Branch,
        base: Option<String>,
        files: Option<restore::RestoredFiles>,
    },
}

pub struct Manager {
    pub store: Store,
    pub(crate) session_hosts: Vec<Arc<dyn crate::sessions::Host>>,
    /// Held while the owner of a recorded place is read and its place
    /// closed, or its ownership moves to another workspace.
    pub(crate) place_ownership: tokio::sync::Mutex<()>,
    pub(crate) gates: gates::Gates,
    pub(crate) item_waits: crate::daemon::links::wait::ActiveWaits,
    pub(crate) chat_waits: crate::daemon::links::wait::ActiveWaits,
    pub(super) cleanup_health: super::cleanup::CleanupHealth,
    pub(crate) syncs: super::checkout_sync::Syncs,
    pub(crate) config_state: config_state::ConfigState,
    pub(crate) paths: Paths,
    /// Arrival order of queued resource and simulator requests.
    pub(crate) queues: super::queue::Queues,
    pub(crate) executions: executions::Executions,
    pub(crate) overload_recovery: overload_recovery::OverloadRecovery,
    pub(crate) agent_queue: agent_queue::AgentQueue,
    pub(crate) operations: Arc<crate::daemon::operations::Operations>,
    /// Held through a stop before a shutdown; true once one finished, until
    /// a system shutdown is cancelled, so later ones leave the work alone.
    pub(crate) shutdown_stopped: tokio::sync::Mutex<bool>,
    pub(crate) signals: signals::Signals,
    /// Process scans shared by the checks for agents' running work.
    pub(crate) work_scans: shared_scans::SharedScans,
    pub(crate) usage: usage::Usage,
    pub(crate) leftovers: crate::daemon::leftovers::Leftovers,
    /// Held shared from reading the tracked processes until those found
    /// untracked are stopped, and exclusively to hand a chat to another
    /// process, so a chat's new process is never found untracked: its
    /// launcher waits for the handoff and keeps it a tracked descendant.
    pub(crate) chat_handoffs: tokio::sync::RwLock<()>,
    /// Held while chats are saved and their processes stopped, and to
    /// restore or remove a stopped chat, so neither overlaps a stop.
    pub(crate) chat_saves: tokio::sync::Mutex<()>,
    pub(crate) chat_stops: crate::daemon::chats::Stopping,
    /// The chats a removal holds, which no restore runs again.
    pub(crate) chat_removals: crate::daemon::chats::Removing,
    /// The chats whose workspaces a removal under their completion takes,
    /// which keep the workspaces below them meanwhile.
    pub(crate) chat_owners: crate::daemon::chats::Removing,
    pub(crate) seeds: super::seed::Seeds,
    pub(crate) forge_memories: crate::daemon::links::ForgeMemories,
    pub(crate) reviews: crate::daemon::reviews::Reviews,
    pub(crate) runners: crate::daemon::runner::Runners,
    /// One-shot agent runs whose processes may still be running.
    pub(crate) oneshot_runs: crate::daemon::oneshot::Runs,
    /// The retry each proxied execution's proxy waits to make.
    proxy_retries: std::sync::Mutex<std::collections::HashMap<String, proxy_retries::ProxyRetry>>,
    /// Directories whose file access stalls, shared with startup's status answer.
    pub(crate) file_access: Arc<crate::daemon::file_access::FileAccess>,
}

impl Manager {
    pub async fn open(paths: Paths) -> Result<Arc<Self>> {
        paths.prepare()?;
        // Avoid inheriting personal Worktrunk hooks and layout preferences.
        fs::write(paths.worktrunk_config(), "# Managed by Shoal.\n")?;
        let config = Config::load(&paths)?;
        super::forge_hosts::learn_forge_pins(&Default::default(), &config.machine.forges);
        Ok(Arc::new(Self {
            config_state: config_state::ConfigState::new(config),
            item_waits: Default::default(),
            chat_waits: Default::default(),
            cleanup_health: Default::default(),
            syncs: Default::default(),
            store: Store::open(paths.database()).await?,
            session_hosts: crate::sessions::hosts(),
            place_ownership: Default::default(),
            gates: Default::default(),
            paths,
            queues: Default::default(),
            executions: Default::default(),
            overload_recovery: Default::default(),
            agent_queue: Default::default(),
            operations: Default::default(),
            shutdown_stopped: Default::default(),
            signals: Default::default(),
            work_scans: Default::default(),
            usage: Default::default(),
            leftovers: Default::default(),
            chat_handoffs: Default::default(),
            chat_saves: Default::default(),
            chat_stops: Default::default(),
            chat_removals: Default::default(),
            chat_owners: Default::default(),
            seeds: Default::default(),
            forge_memories: Default::default(),
            reviews: Default::default(),
            runners: Default::default(),
            oneshot_runs: Default::default(),
            proxy_retries: Default::default(),
            file_access: Default::default(),
        }))
    }

    /// The global config as last loaded; see [`config_state::ConfigState::current`].
    pub fn config(&self) -> Arc<Config> {
        self.config_state.current()
    }

    /// Make `config` the one later operations read. Changed overload
    /// settings withdraw recovery readiness before anything can read them, so
    /// a waiting agent needs a fresh healthy interval under the new thresholds.
    pub(crate) fn publish_config(&self, config: Config) {
        if config.machine.overload != self.config().machine.overload {
            self.overload_recovery.reset();
        }
        super::forge_hosts::learn_forge_pins(&self.config().machine.forges, &config.machine.forges);
        self.config_state.publish(config);
    }

    /// Replace the global config with the file's current contents for later
    /// operations. An invalid file leaves the running config in place.
    pub async fn reload_config(&self) -> Result<()> {
        let _reload = self.config_state.lock_reload().await;
        let paths = self.paths.clone();
        let config = tokio::task::spawn_blocking(move || Config::load_settled(&paths)).await??;
        self.publish_config(config);
        Ok(())
    }

    pub(crate) async fn lock_repository(
        &self,
        selector: impl Into<crate::repository::Selector>,
    ) -> Result<(Repository, OwnedMutexGuard<()>)> {
        let repo = self.repository(selector).await?;
        let guard = self.gates.lock_git(&repo.id).await;
        // Removal may have completed or failed while this request waited.
        let repo = self.repository(&repo.id).await?;
        self.ensure_repository_available(&repo.id).await?;
        Ok((repo, guard))
    }

    pub async fn list_workspaces(&self) -> Result<Vec<Workspace>> {
        self.list_workspaces_within(None).await
    }

    /// `root` and every workspace below it, or every workspace without
    /// `root`, by name. Only the selected workspaces load their details.
    pub async fn list_workspaces_within(&self, root: Option<&str>) -> Result<Vec<Workspace>> {
        let root = root.map(str::to_owned);
        self.store
            .run(move |db| {
                let mut workspaces = db
                    .prepare(&tree::subtree_query(store::WORKSPACE_COLUMNS))?
                    .query_map([root], store::workspace)?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                for workspace in &mut workspaces {
                    load_details(db, workspace)?;
                }
                Ok(workspaces)
            })
            .await
    }

    pub async fn workspace_ids(&self) -> Result<HashSet<String>> {
        self.store
            .run(|db| {
                Ok(db
                    .prepare("SELECT id FROM workspaces")?
                    .query_map([], |row| row.get(0))?
                    .collect::<rusqlite::Result<_>>()?)
            })
            .await
    }

    /// What the workspace's processes use, from the latest sample.
    pub(crate) fn workspace_usage(&self, id: &str) -> Option<crate::model::WorkspaceUsage> {
        let now = tokio::time::Instant::now();
        self.usage.with_workspaces(|usages| usages.get(id, now))
    }

    /// The workspaces a caller lists, with review marks checked against
    /// HEAD. A workspace process sees its own subtree with its own workspace
    /// first, since scoped commands without a target act on the first one.
    pub async fn visible_workspaces(&self, owner: Option<&str>) -> Result<Vec<Workspace>> {
        let mut workspaces = self.list_workspaces_within(owner).await?;
        if let Some(owner) = owner {
            workspaces.sort_by_key(|w| w.id != owner);
        }
        let deadline = review::check_deadline();
        futures_util::stream::iter(&mut workspaces)
            .for_each_concurrent(REVIEW_CHECK_CONCURRENCY, |workspace| {
                self.annotate_review(workspace, deadline)
            })
            .await;
        for workspace in &mut workspaces {
            workspace.usage = self.workspace_usage(&workspace.id);
        }
        Ok(workspaces)
    }

    /// Whether the workspace has any recorded execution, in any state.
    pub(super) async fn workspace_has_executions(&self, id: &str) -> Result<bool> {
        let id = id.to_owned();
        self.store
            .run(move |db| {
                store::exists(db, "SELECT 1 FROM executions WHERE workspace_id=?1", [id])
            })
            .await
    }

    /// Read recorded executions in every state for a known workspace ID.
    pub(super) async fn workspace_executions(&self, id: &str) -> Result<Vec<Execution>> {
        let id = id.to_owned();
        self.store.run(move |db| store::executions(db, &id)).await
    }

    /// Count recorded executions in every state without decoding their details.
    pub(super) async fn workspace_execution_count(&self, id: &str) -> Result<usize> {
        let id = id.to_owned();
        self.store
            .run(move |db| {
                let count: i64 = db.query_row(
                    "SELECT COUNT(*) FROM executions WHERE workspace_id=?1",
                    [id],
                    |row| row.get(0),
                )?;
                Ok(usize::try_from(count)?)
            })
            .await
    }

    /// Look a workspace up by ID or name.
    pub async fn workspace(&self, selector: &str) -> Result<Workspace> {
        let selector = selector.to_owned();
        self.store
            .run(move |db| {
                let mut workspace = db
                    .query_row(
                        &format!(
                            "SELECT {} FROM workspaces WHERE id=?1 OR name=?1",
                            store::WORKSPACE_COLUMNS
                        ),
                        [&selector],
                        store::workspace,
                    )
                    .optional()?
                    .ok_or_else(|| {
                        Refusal::new(Reason::NotFound, format!("unknown workspace: {selector}"))
                    })?;
                load_details(db, &mut workspace)?;
                Ok(workspace)
            })
            .await
    }

    /// The workspace with this ID, unless it was removed.
    pub async fn find_workspace(&self, id: &str) -> Result<Option<Workspace>> {
        let owned = id.to_owned();
        let exists = self
            .store
            .run(move |db| store::exists(db, "SELECT 1 FROM workspaces WHERE id=?1", [owned]))
            .await?;
        Ok(match exists {
            true => Some(self.workspace(id).await?),
            false => None,
        })
    }

    /// Resolve an optional selector to a workspace ID filter.
    pub async fn workspace_filter(&self, selector: Option<&str>) -> Result<Option<String>> {
        match selector {
            Some(selector) => Ok(Some(self.workspace(selector).await?.id)),
            None => Ok(None),
        }
    }

    pub async fn inspect_workspace(&self, selector: &str) -> Result<Inspection> {
        let mut workspace = self.workspace(selector).await?;
        self.annotate_review(&mut workspace, review::check_deadline())
            .await;
        let simulators = self.list_simulators(Some(&workspace.id)).await?;
        let issue = self.issue_registration(&workspace.id).await?;
        let pr_cleanup = self.pr_registration(&workspace.id).await?;
        let completion = self.completion(&workspace.id).await?;
        let mut inspection = self
            .store
            .run(move |db| {
                Ok(Inspection {
                    issue,
                    completion,
                    pr_cleanup,
                    executions: store::executions(db, &workspace.id)?,
                    places: places::owned(db, Some(&workspace.id))?
                        .into_iter()
                        .map(|(_, place)| place)
                        .collect(),
                    ports: store::ports(db, Some(&workspace.id))?,
                    resources: crate::daemon::resources::leases(db, Some(&workspace.id))?,
                    simulators,
                    workspace,
                })
            })
            .await?;
        self.annotate_stops(&mut inspection.executions).await;
        Ok(inspection)
    }

    #[cfg(test)]
    pub async fn create_workspace(
        &self,
        repository: impl Into<crate::repository::Selector>,
        name: String,
        base: Option<String>,
        git_profile: Option<&str>,
        path: Option<std::path::PathBuf>,
    ) -> Result<Workspace> {
        let new = NewWorkspace {
            git_profile,
            path,
            options: CreationOptions::default(),
        };
        self.create_workspace_with(repository, name, base, new)
            .await
    }

    /// [`Self::create_workspace`] where and as `new` says.
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
        let name = name.map(|name| derive_workspace_name(&name));
        if let Some(name) = name.clone() {
            self.store
                .run(move |db| store::ensure_name_available(db, &name, None))
                .await?;
        }
        self.check_new_workspace_path(&repo, name.as_deref(), options.path)
            .await
    }

    // The caller holds the per-repository Git gate through materialization.
    async fn create_branch_workspace(
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
        };
        let name = derive_workspace_name(&name);
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
        self.insert_workspace(workspace.clone(), base_branch, parent)
            .await?;
        let result = self
            .materialize_worktree(repo, &workspace, source, git_profile)
            .await;
        self.settle_creation(&workspace, result).await
    }

    /// Record how materializing the recorded `workspace` went: ready, or
    /// preparing while it needs setup, or failed.
    async fn settle_creation(
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
        self.workspace(&workspace.id).await
    }

    /// Record a new workspace, stacked on the workspace that owns `base_branch`
    /// and split from `parent`.
    async fn insert_workspace(
        &self,
        record: Workspace,
        base_branch: Option<String>,
        parent: Option<String>,
    ) -> Result<()> {
        let marker = match record.git_dir.clone() {
            Some(git_dir) => off_runtime::run(move || identity::marked_owner(&git_dir)).await?,
            None => None,
        };
        self.store
            .run(move |db| {
                let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
                store::ensure_name_and_branch_available(
                    &tx,
                    &record.repository_id,
                    &record.name,
                    record.branch.as_deref(),
                    None,
                )?;
                store::ensure_path_unrecorded(&tx, &record.path)?;
                store::ensure_path_outside_moves(&tx, &record.path)?;
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
                    Some(branch) => stack::owner(&tx, &record.repository_id, branch)?,
                    None => None,
                };
                // Recorded first, so the workspace's creation names it.
                if let Some(parent) = &parent {
                    tree::ensure_parent(&tx, &record.repository_id, &record.id, parent)?;
                    tree::set_parent(&tx, &record.id, Some(parent))?;
                }
                tx.execute(
                    "INSERT INTO workspaces (id,repository_id,name,path,branch,state,git_dir,git_dir_id,base_commit,base_ref,setup_finished,observed_branch,base_workspace_id,start_commit,made_for,setup_skipped,disposable) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?5,?12,?13,?14,?15,?16)",
                    params![
                        record.id,
                        record.repository_id,
                        record.name,
                        record.path.to_str(),
                        record.branch,
                        record.state,
                        record.git_dir.and_then(|p| p.to_str().map(str::to_owned)),
                        record.git_dir_id,
                        record.base_commit,
                        record.base_ref,
                        record.state == WorkspaceState::Ready,
                        base_workspace,
                        record.at.as_ref().map(|at| &at.commit),
                        record.at.as_ref().and_then(|at| at.made_for.as_ref()).map(serde_json::to_string).transpose()?,
                        record.setup_skipped,
                        record.disposable,
                    ],
                )?;
                tx.execute(
                    "UPDATE repositories SET last_used=(SELECT COALESCE(MAX(last_used),0)+1 FROM repositories) WHERE id=?1",
                    [record.repository_id],
                )?;
                tx.commit()?;
                Ok(())
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
    async fn configure_worktree(
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
    async fn new_branch_base(&self, repo: &Repository, base: Option<&str>) -> Result<String> {
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

    pub(crate) async fn set_state(
        &self,
        id: &str,
        state: WorkspaceState,
        error: Option<String>,
    ) -> Result<()> {
        let id = id.to_owned();
        self.store
            .run(move |db| {
                db.execute(
                    "UPDATE workspaces SET state=?2, error=?3 WHERE id=?1",
                    params![id, state, error],
                )?;
                Ok(())
            })
            .await
    }

    async fn finish_materialization(&self, id: &str, needs_setup: bool) -> Result<()> {
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

    /// Move a ready or failed workspace into a transient lifecycle state,
    /// excluding every other lifecycle operation until it is restored.
    pub(crate) async fn reserve_lifecycle(
        &self,
        id: &str,
        state: WorkspaceState,
    ) -> Result<WorkspaceState> {
        self.reserve_lifecycle_inner(id, Some(state), RemovalRules::default())
            .await
    }

    /// Refuse what [`Self::reserve_lifecycle`] would refuse now, without
    /// reserving, so a batch can check every target before acting on any.
    pub(crate) async fn check_lifecycle(&self, id: &str) -> Result<()> {
        self.reserve_lifecycle_inner(id, None, RemovalRules::default())
            .await?;
        Ok(())
    }

    async fn reserve_lifecycle_for_removal(
        &self,
        id: &str,
        rules: RemovalRules,
    ) -> Result<WorkspaceState> {
        self.reserve_lifecycle_inner(id, Some(WorkspaceState::Removing), rules)
            .await
    }

    /// Reserve the workspace in `state`, or only check that it could be
    /// without `state`.
    async fn reserve_lifecycle_inner(
        &self,
        id: &str,
        state: Option<WorkspaceState>,
        rules: RemovalRules,
    ) -> Result<WorkspaceState> {
        let _resources = self.gates.resource_guard(id, GuardMode::Shared)?;
        let id = id.to_owned();
        self.store
            .run(move |db| {
                let tx = db.transaction()?;
                // Removal checks pending intent after reserving the lifecycle,
                // so a confirmed-deleted worktree can release its ownership.
                if state != Some(WorkspaceState::Reconciling) && !rules.allow_pending_rename {
                    store::ensure_no_pending_rename(&tx, &id)?;
                }
                store::ensure_no_pending_move(&tx, &id)?;
                let current: Option<WorkspaceState> = tx
                    .query_row("SELECT state FROM workspaces WHERE id=?1", [&id], |row| {
                        row.get(0)
                    })
                    .optional()?;
                let current = match current {
                    None => bail!("workspace no longer exists"),
                    Some(current @ (WorkspaceState::Ready | WorkspaceState::Failed)) => current,
                    // Setup registers its execution as it reserves the
                    // workspace, so none recorded means none started.
                    Some(current @ WorkspaceState::Preparing)
                        if rules.awaiting_setup
                            && !store::exists(
                                &tx,
                                "SELECT 1 FROM executions WHERE workspace_id=?1",
                                [&id],
                            )? =>
                    {
                        current
                    }
                    Some(WorkspaceState::Removing) => {
                        bail!("workspace is already being removed; it will disappear shortly")
                    }
                    Some(current) => bail!(
                        "workspace is {current}; wait for that operation to finish before another lifecycle operation"
                    ),
                };
                // Holds need a ready workspace, so none can arrive once it is
                // reserved.
                ensure!(
                    !rules.refuse_holds
                        || !store::exists(
                            &tx,
                            "SELECT 1 FROM workspace_holds WHERE workspace_id=?1 AND placed_by=?2",
                            params![id, crate::model::HoldPlacer::User],
                        )?,
                    "workspace has holds, which are the user's; ask the user to remove it"
                );
                if let Some(state) = state {
                    tx.execute(
                        "UPDATE workspaces SET state=?2 WHERE id=?1",
                        params![id, state],
                    )?;
                    tx.commit()?;
                }
                Ok(current)
            })
            .await
    }

    /// Record activity so automatic cleanup restarts its idle timer, and its
    /// time for idle periods that must survive a restart.
    pub async fn touch(&self, id: &str) {
        self.usage.record_activity(id);
        let id = id.to_owned();
        let recorded = self
            .store
            .run(move |db| {
                db.execute(
                    "UPDATE workspaces SET active_at=unixepoch() WHERE id=?1",
                    [id],
                )?;
                Ok(())
            })
            .await;
        if let Err(error) = recorded {
            crate::daemon::log!("activity time not recorded: {error:#}");
        }
    }

    /// The Unix time of the workspace's last recorded activity; one without
    /// any starts its idle period now.
    pub(crate) async fn active_at(&self, id: &str) -> Result<u64> {
        let id = id.to_owned();
        self.store
            .run(move |db| {
                db.execute(
                    "UPDATE workspaces SET active_at=unixepoch() WHERE id=?1 AND active_at IS NULL",
                    [&id],
                )?;
                let active_at: i64 = db.query_row(
                    "SELECT active_at FROM workspaces WHERE id=?1",
                    [&id],
                    |row| row.get(0),
                )?;
                Ok(u64::try_from(active_at)?)
            })
            .await
    }

    /// Drop in-memory bookkeeping for a workspace that no longer exists.
    pub(crate) async fn forget_workspace(&self, id: &str) {
        self.usage.forget_workspace(id);
        self.executions.forget_workspace(id).await;
    }
}

/// Keep directory/selector names portable without restricting Git branch syntax.
pub(crate) fn derive_workspace_name(branch: &str) -> String {
    let name: String = branch
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_') {
                c
            } else {
                '-'
            }
        })
        .collect();
    let name = name.trim_start_matches(['-', '_']);
    if name.is_empty() {
        "workspace".into()
    } else {
        name.chars().take(crate::validate::MAX_NAME_LEN).collect()
    }
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
async fn recorded_base(
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
async fn existing_base(repo: &crate::model::Repository, branch: &str) -> Result<String> {
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

/// What reserving a workspace for removal accepts besides a ready or failed
/// workspace, and what it refuses.
#[derive(Clone, Copy, Default)]
struct RemovalRules {
    /// A pending rename, which removing a deleted worktree releases.
    allow_pending_rename: bool,
    /// A preparing workspace whose setup never started; the caller excludes
    /// its creation by holding the repository's Git gate.
    awaiting_setup: bool,
    /// A workspace with holds the user placed, whose removal would release
    /// them.
    refuse_holds: bool,
}

/// Fill in what a workspace record reads from its related tables.
fn load_details(db: &rusqlite::Connection, workspace: &mut Workspace) -> Result<()> {
    workspace.holds = holds::list(db, &workspace.id)?;
    workspace.review = review::list(db, &workspace.id)?;
    workspace.latest_review = crate::daemon::reviews::store::latest_status(db, &workspace.id)?;
    workspace.agent_state = agent_state::current(db, &workspace.id)?;
    stack::load(db, workspace)?;
    tree::load(db, workspace)?;
    workspace.links = store::space_links(db, &workspace.id)?;
    workspace.running = store::exists(
        db,
        "SELECT 1 FROM executions WHERE workspace_id=?1 AND state=?2",
        rusqlite::params![workspace.id, crate::state::ExecutionState::Running],
    )?;
    workspace.done = store::exists(
        db,
        "SELECT 1 FROM workspace_completion WHERE workspace_id=?1",
        [&workspace.id],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests;
