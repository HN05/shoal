//! Shared daemon state, workspace lookup, and worktree creation.
mod adoption;
pub(crate) mod agent_queue;
pub(crate) mod agent_state;
mod agents;
mod branches;
pub(crate) mod capacity;
mod checkpoints;
mod conflicts;
mod contributions;
mod diff;
mod done;
mod environment;
mod executions;
mod holds;
pub(super) mod identity;
mod kept_branches;
mod land;
mod lifecycle;
mod location;
mod ownership;
pub(crate) mod paths;
mod registry;
mod rename;
mod repo_configuration;
mod repo_move;
mod repo_removal;
mod restore;
pub(crate) mod review;
mod stack;
mod status;
mod tree;
mod xcode;

pub(crate) use agents::{Pause, Protection};
pub(crate) use branches::NewWorktreeOptions;
pub use executions::{ExecutionKind, Save, StopRequest};
pub(crate) use executions::{ReattachedExecution, StartedExecution};
pub(crate) use ownership::VerifiedWorktree;
pub(crate) use restore::removal_notice;
pub(crate) use stack::record_base_changed;
pub(crate) use tree::{
    Source, TrackedAgents, ancestors_with_agents, coordinators, delegated, receiving_paths,
};

use crate::{
    config::Config,
    daemon::{
        scope::Caller,
        store::{self, Store},
    },
    git::{self, default_branch::DefaultBranchLookup, worktrunk},
    model::{Execution, Inspection, Repository, Workspace},
    paths::Paths,
    state::WorkspaceState,
};
use anyhow::{Context, Result, bail, ensure};
use branches::UpstreamPolicy;
use ownership::owns_git_dir;
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use std::{
    collections::{HashMap, HashSet},
    fs,
    sync::Arc,
};
use tokio::sync::{Mutex, OwnedMutexGuard, RwLock, watch};

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

#[derive(Debug, Clone, Copy)]
pub(crate) enum GuardMode {
    Shared,
    Exclusive,
}

pub(crate) enum ResourceGuard {
    Operation {
        _guard: tokio::sync::OwnedRwLockReadGuard<()>,
    },
    Hook {
        _guard: tokio::sync::OwnedRwLockWriteGuard<()>,
    },
}

pub struct Manager {
    pub store: Store,
    /// Readers cover background operations, so a writer can quiesce them
    /// without cancelling hooks or mutations already in progress.
    pub(super) background_operations: Arc<RwLock<()>>,
    pub(crate) pr_gate: Mutex<()>,
    /// Orders announcing a question or approval request against withdrawing
    /// it, so a withdrawn one is never announced afterwards.
    pub(crate) announcement_gate: Mutex<()>,
    pub(crate) item_waits: crate::daemon::links::wait::ActiveWaits,
    pub(crate) chat_waits: crate::daemon::links::wait::ActiveWaits,
    pub cleanup_notify: tokio::sync::Notify,
    pub(super) cleanup_health: std::sync::Mutex<super::cleanup::Health>,
    /// The global file as last loaded; a reload replaces it for later reads
    /// and wakes subscribers.
    config: watch::Sender<Arc<Config>>,
    /// Orders reloads so the last file read is the one published.
    config_reload: Mutex<()>,
    pub(crate) paths: Paths,
    /// Serializes every simctl transition.
    pub(crate) simulator_gate: Mutex<()>,
    /// Arrival order of queued resource and simulator requests.
    pub(crate) queues: super::queue::Queues,
    /// Serializes repository registration, removal, and local config changes.
    registry_gate: Mutex<()>,
    /// Orders checkpoint records and refs against expiry, so a ref is never
    /// deleted between its record and its write. Held only for those steps,
    /// since removal takes checkpoints under the repository's Git gate.
    checkpoint_gate: Mutex<()>,
    /// Per-repository gate for ref updates and worktree creation/removal.
    git_gates: Mutex<HashMap<String, Arc<Mutex<()>>>>,
    /// Per-host gate, so concurrent first lookups ask a forge host once.
    pub(super) forge_host_gates: Mutex<HashMap<String, Arc<Mutex<()>>>>,
    /// Excludes lifecycle transitions and overlapping permit hooks per workspace.
    resource_gates: Mutex<HashMap<String, Arc<RwLock<()>>>>,
    /// Scope token → the execution it was issued to.
    scopes: Mutex<HashMap<String, Caller>>,
    /// Connected executions and the channel that asks their wrapper to stop.
    pub(super) connections: Mutex<HashMap<String, watch::Sender<bool>>>,
    /// Connected executions whose wrapper stops instead of reattaching when
    /// the daemon shuts down.
    unattachable: std::sync::Mutex<HashSet<String>>,
    agents: Mutex<HashMap<String, agents::Agent>>,
    /// Why the daemon asked connected executions to stop.
    stop_requests: Mutex<HashMap<String, executions::StopRequest>>,
    /// Set under the connections lock once shutdown stops executions.
    shutting_down: std::sync::atomic::AtomicBool,
    pub(super) recovery_ready: watch::Sender<Option<u64>>,
    pub(super) recovery_epoch: std::sync::atomic::AtomicU64,
    /// Counts published configurations, so a reading taken under an earlier
    /// one cannot pass for a current one.
    pub(super) config_generation: std::sync::atomic::AtomicU64,
    /// One more than the configuration generation under which the disk
    /// monitor's latest reading found the cleanup threshold free on every
    /// filesystem, so restored agents can write; zero for none.
    pub(super) disk_recovered_in: std::sync::atomic::AtomicU64,
    /// Per-workspace activity counters folded into cleanup fingerprints.
    activity: Mutex<HashMap<String, u64>>,
    /// The newest recorded notification ID; wakes `shoal notifications --follow`.
    pub(crate) notifications_changed: watch::Sender<i64>,
    /// Counts queued agent messages; wakes watches waiting for them.
    pub(crate) agent_messages_changed: watch::Sender<u64>,
    /// Counts queued chat messages; wakes chats waiting for them.
    pub(crate) chat_messages_changed: watch::Sender<u64>,
    /// Counts answered and closed questions; wakes waits for an answer.
    pub(crate) questions_changed: watch::Sender<u64>,
    /// Counts background polls that queued item updates; wakes watches.
    pub(crate) item_updates_queued: watch::Sender<u64>,
    /// Counts agent state changes and deliveries to agents; wakes waits for
    /// an idle agent's updates.
    pub(crate) agent_wake_changed: watch::Sender<u64>,
    /// Serializes agent queue admission, so two passes cannot fill one slot.
    agent_queue_gate: Mutex<()>,
    /// Open holds per starting queue entry.
    agent_queue_holds: std::sync::Mutex<HashMap<String, usize>>,
    /// Executions whose stopped agent waits for its queued restore with
    /// every process stopped, with the recovery epoch its admission holds.
    restore_waiters: std::sync::Mutex<HashMap<String, Option<u64>>>,
    /// Wakes the queue dispatcher when a slot may have freed.
    pub(crate) agent_queue_wake: tokio::sync::Notify,
    /// Wakes stopped-agent reconciliation when an execution becomes unknown.
    pub(crate) execution_unknown: tokio::sync::Notify,
    /// Recent CPU readings for dynamic agent admission.
    pub(crate) cpu_usage: std::sync::Mutex<crate::daemon::cpu_usage::CpuUsage>,
    /// The commit and settings whose seed build last failed, per repository.
    pub(super) seed_failures: std::sync::Mutex<HashMap<String, (String, String)>>,
    /// Running seed builds, which repository removal cancels and waits for.
    pub(super) seed_builds: std::sync::Mutex<HashMap<String, Arc<super::seed::SeedSlot>>>,
    /// What forge responses taught, per account the daemon acts as.
    pub(crate) forge_memories:
        std::sync::Mutex<HashMap<String, crate::forge::transport::memory::Memory>>,
}

impl Manager {
    pub async fn open(paths: Paths) -> Result<Arc<Self>> {
        paths.prepare()?;
        // Avoid inheriting personal Worktrunk hooks and layout preferences.
        fs::write(paths.worktrunk_config(), "# Managed by Shoal.\n")?;
        let config = Config::load(&paths)?;
        super::forge_hosts::learn_forge_pins(&Default::default(), &config.forges);
        Ok(Arc::new(Self {
            config: watch::channel(Arc::new(config)).0,
            config_reload: Mutex::new(()),
            pr_gate: Mutex::new(()),
            announcement_gate: Mutex::new(()),
            item_waits: Default::default(),
            chat_waits: Default::default(),
            cleanup_notify: tokio::sync::Notify::new(),
            cleanup_health: Default::default(),
            store: Store::open(paths.database()).await?,
            background_operations: Arc::new(RwLock::new(())),
            paths,
            simulator_gate: Mutex::new(()),
            queues: Default::default(),
            registry_gate: Mutex::new(()),
            checkpoint_gate: Mutex::new(()),
            git_gates: Mutex::new(HashMap::new()),
            forge_host_gates: Mutex::new(HashMap::new()),
            resource_gates: Mutex::new(HashMap::new()),
            scopes: Mutex::new(HashMap::new()),
            connections: Mutex::new(HashMap::new()),
            unattachable: Default::default(),
            agents: Mutex::new(HashMap::new()),
            stop_requests: Mutex::new(HashMap::new()),
            shutting_down: std::sync::atomic::AtomicBool::new(false),
            recovery_ready: watch::channel(None).0,
            recovery_epoch: std::sync::atomic::AtomicU64::new(0),
            config_generation: std::sync::atomic::AtomicU64::new(0),
            disk_recovered_in: std::sync::atomic::AtomicU64::new(0),
            activity: Mutex::new(HashMap::new()),
            notifications_changed: watch::channel(0).0,
            agent_messages_changed: watch::channel(0).0,
            chat_messages_changed: watch::channel(0).0,
            questions_changed: watch::channel(0).0,
            item_updates_queued: watch::channel(0).0,
            agent_wake_changed: watch::channel(0).0,
            agent_queue_gate: Mutex::new(()),
            agent_queue_holds: Default::default(),
            restore_waiters: Default::default(),
            agent_queue_wake: tokio::sync::Notify::new(),
            execution_unknown: tokio::sync::Notify::new(),
            cpu_usage: Default::default(),
            seed_failures: Default::default(),
            seed_builds: Default::default(),
            forge_memories: Default::default(),
        }))
    }

    /// The global config as last loaded. Take one snapshot per operation so a
    /// concurrent reload cannot mix two versions.
    pub fn config(&self) -> Arc<Config> {
        self.config.borrow().clone()
    }

    /// Observe reloads, for waits that a policy change must end.
    pub(crate) fn watch_config(&self) -> watch::Receiver<Arc<Config>> {
        self.config.subscribe()
    }

    /// Make `config` the one later operations read. Changed overload
    /// settings withdraw recovery readiness before anything can read them, so
    /// a waiting agent needs a fresh healthy interval under the new thresholds.
    pub(crate) fn publish_config(&self, config: Config) {
        if config.overload != self.config().overload {
            self.reset_overload_recovery();
        }
        super::forge_hosts::learn_forge_pins(&self.config().forges, &config.forges);
        self.config.send_replace(Arc::new(config));
        // After the swap: a reader that sees the new generation sees this config.
        self.config_generation
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }

    /// Replace the global config with the file's current contents for later
    /// operations. An invalid file leaves the running config in place.
    pub async fn reload_config(&self) -> Result<()> {
        let _reload = self.config_reload.lock().await;
        let paths = self.paths.clone();
        let config = tokio::task::spawn_blocking(move || Config::load_settled(&paths)).await??;
        self.publish_config(config);
        Ok(())
    }

    /// Acquire the Git gate by recorded ID without checking repository availability.
    /// Callers must not already hold this repository's gate.
    pub(crate) async fn lock_repository_git(&self, repository_id: &str) -> OwnedMutexGuard<()> {
        let gate = self
            .git_gates
            .lock()
            .await
            .entry(repository_id.to_owned())
            .or_default()
            .clone();
        gate.lock_owned().await
    }

    /// [`Self::lock_repository_git`] when nothing holds the gate.
    pub(crate) async fn try_lock_repository_git(
        &self,
        repository_id: &str,
    ) -> Option<OwnedMutexGuard<()>> {
        let gate = self
            .git_gates
            .lock()
            .await
            .entry(repository_id.to_owned())
            .or_default()
            .clone();
        gate.try_lock_owned().ok()
    }

    pub(crate) async fn lock_repository(
        &self,
        selector: impl Into<crate::repository::Selector>,
    ) -> Result<(Repository, OwnedMutexGuard<()>)> {
        let repo = self.repository(selector).await?;
        let guard = self.lock_repository_git(&repo.id).await;
        // Removal may have completed or failed while this request waited.
        let repo = self.repository(&repo.id).await?;
        self.ensure_repository_available(&repo.id).await?;
        Ok((repo, guard))
    }

    pub(crate) async fn resource_guard(&self, id: &str, mode: GuardMode) -> Result<ResourceGuard> {
        let gate = self
            .resource_gates
            .lock()
            .await
            .entry(id.to_owned())
            .or_default()
            .clone();
        let guard = match mode {
            GuardMode::Exclusive => gate
                .try_write_owned()
                .map(|guard| ResourceGuard::Hook { _guard: guard }),
            GuardMode::Shared => gate
                .try_read_owned()
                .map(|guard| ResourceGuard::Operation { _guard: guard }),
        };
        guard.context("workspace resource operation is in progress; retry when its hook finishes")
    }

    /// Bind a scope token to the execution it was issued to.
    pub(crate) async fn issue_scope(&self, token: String, caller: Caller) {
        self.scopes.lock().await.insert(token, caller);
    }

    pub async fn list_workspaces(&self) -> Result<Vec<Workspace>> {
        self.store
            .run(|db| {
                let mut workspaces = db
                    .prepare(&format!(
                        "SELECT {} FROM workspaces ORDER BY name",
                        store::WORKSPACE_COLUMNS
                    ))?
                    .query_map([], store::workspace)?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                for workspace in &mut workspaces {
                    load_details(db, workspace)?;
                }
                Ok(workspaces)
            })
            .await
    }

    /// The workspaces a caller lists, with review marks checked against
    /// HEAD. A workspace process sees its own subtree with its own workspace
    /// first, since scoped commands without a target act on the first one.
    pub async fn visible_workspaces(&self, owner: Option<&str>) -> Result<Vec<Workspace>> {
        let mut workspaces = self.list_workspaces().await?;
        if let Some(owner) = owner {
            let subtree = self.subtree(owner).await?;
            workspaces.retain(|w| subtree.contains(&w.id));
            workspaces.sort_by_key(|w| w.id != owner);
        }
        for workspace in &mut workspaces {
            self.annotate_review(workspace).await;
        }
        Ok(workspaces)
    }

    /// Record that a restart would stop this connected execution.
    pub(super) fn mark_unattachable(&self, id: &str) {
        self.unattachable
            .lock()
            .expect("unattachable executions lock")
            .insert(id.to_owned());
    }

    /// Whether a restart would stop a connected execution. Disconnected
    /// records keep their state across it.
    pub(super) fn has_unattachable_connections(&self) -> bool {
        !self
            .unattachable
            .lock()
            .expect("unattachable executions lock")
            .is_empty()
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
                    .with_context(|| format!("unknown workspace: {selector}"))?;
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
        self.annotate_review(&mut workspace).await;
        let simulators = self.list_simulators(Some(&workspace.id)).await?;
        let issue = self.issue_registration(&workspace.id).await?;
        let pr_cleanup = self.pr_registration(&workspace.id).await?;
        let completion = self.completion(&workspace.id).await?;
        self.store
            .run(move |db| {
                Ok(Inspection {
                    issue,
                    completion,
                    pr_cleanup,
                    executions: store::executions(db, &workspace.id)?,
                    ports: store::ports(db, Some(&workspace.id))?,
                    resources: crate::daemon::resources::leases(db, Some(&workspace.id))?,
                    simulators,
                    workspace,
                })
            })
            .await
    }

    pub async fn create_workspace(
        &self,
        repository: impl Into<crate::repository::Selector>,
        name: String,
        base: Option<String>,
        git_profile: Option<&str>,
        path: Option<std::path::PathBuf>,
    ) -> Result<Workspace> {
        let (repo, _guard) = self.lock_repository(repository).await?;
        git::check_branch_name(Some(&repo.path), &name).await?;
        let branch = self.available_branch(&repo, &name).await?;
        self.create_branch_workspace(
            &repo,
            name,
            branch,
            WorkspaceSource::New { base, parent: None },
            git_profile,
            path,
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
        let (name, opened) = match (branch, existing) {
            (Some(name), _) => {
                git::check_branch_name(Some(&repo.path), name).await?;
                (Some(name.to_owned()), None)
            }
            (None, Some(selector)) => match self.open_target(&repo, selector, options).await? {
                branches::OpenTarget::Reopen(workspace) => {
                    return self.check_reopen(&workspace, &workspace.branch).await;
                }
                branches::OpenTarget::Create(branch) => (Some(branch.name.clone()), Some(branch)),
            },
            (None, None) => (None, None),
        };
        if let Some(profile) = options.git_profile {
            self.config().git.profile(profile)?;
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
        git_profile: Option<&str>,
        path: Option<std::path::PathBuf>,
    ) -> Result<Workspace> {
        if let Some(name) = git_profile {
            self.config().git.profile(name)?;
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
        let workspace = Workspace::new_record(
            repo.id.clone(),
            name,
            path,
            branch,
            WorkspaceState::Preparing,
        );
        self.insert_workspace(workspace.clone(), base_branch, parent)
            .await?;
        let result = self
            .materialize_worktree(repo, &workspace, source, git_profile)
            .await;
        match result {
            Ok(needs_setup) => {
                self.finish_materialization(&workspace.id, needs_setup)
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
        let marker = record
            .git_dir
            .as_ref()
            .map(|git_dir| identity::marked_owner(git_dir))
            .transpose()?
            .flatten();
        self.store
            .run(move |db| {
                let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
                store::ensure_name_and_branch_available(
                    &tx,
                    &record.repository_id,
                    &record.name,
                    &record.branch,
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
                if let Some(parent) = &parent {
                    tree::ensure_parent(&tx, &record.repository_id, parent)?;
                }
                tx.execute(
                    "INSERT INTO workspaces (id,repository_id,name,path,branch,state,git_dir,git_dir_id,base_commit,base_ref,setup_finished,observed_branch,base_workspace_id,parent_workspace_id) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?5,?12,?13)",
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
                        parent,
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
            &workspace.branch,
            matches!(&source, WorkspaceSource::New { .. }).then_some(commit.as_str()),
        )
        .await?;
        self.record_worktree_identity(workspace).await?;
        if let WorkspaceSource::Existing {
            files: Some(files), ..
        } = &source
        {
            restore::write_files(&workspace.path, files).await?;
        }
        let config = self.config();
        let settings = self.workspace_settings_from(&config, workspace).await?;
        self.apply_workspace_git_profile(workspace, &config, &settings, git_profile)
            .await?;
        self.apply_seed(repo, workspace, &settings).await;
        Ok(settings.setup_cmd.is_some()
            || settings.pre_setup_cmd.is_some()
            || settings.copy_ignored)
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
                self.existing_branch_base(repo, &workspace.branch, base.as_deref())
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

    /// Select a new branch's base, refreshing the default or fetching a named
    /// remote branch when needed.
    async fn new_branch_base(&self, repo: &Repository, base: Option<&str>) -> Result<String> {
        let lookup = if base.is_none() {
            DefaultBranchLookup::Discover
        } else {
            DefaultBranchLookup::Cached
        };
        let default = crate::git::default_branch::resolve(&repo.path, lookup).await;
        match base {
            None => {
                let default = default?;
                self.refresh_branch(repo, &default, UpstreamPolicy::AllowLocalOnly)
                    .await
                    .context("could not refresh the default branch before creating workspace")?;
                Ok(git::local_ref(&default))
            }
            Some(base) => {
                // An explicit ref remains an escape hatch when remote
                // default-branch discovery is unavailable; only a remote
                // branch it names is fetched.
                if let Ok(default) = default {
                    let default_ref = git::local_ref(&default);
                    if default == base || default_ref == base {
                        self.refresh_branch(repo, &default, UpstreamPolicy::AllowLocalOnly)
                            .await
                            .context(
                                "could not refresh the default branch before creating workspace",
                            )?;
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
        }
    }

    /// Record the base after refresh and before Worktrunk creates the worktree.
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
            crate::git_profile::apply(&workspace.path, config.git.profile(name)?)
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
                    "UPDATE workspaces SET state=?2,error=NULL,setup_finished=?3 WHERE id=?1",
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
        self.reserve_lifecycle_inner(id, state, false).await
    }

    async fn reserve_lifecycle_for_removal(
        &self,
        id: &str,
        allow_pending_rename: bool,
    ) -> Result<WorkspaceState> {
        self.reserve_lifecycle_inner(id, WorkspaceState::Removing, allow_pending_rename)
            .await
    }

    async fn reserve_lifecycle_inner(
        &self,
        id: &str,
        state: WorkspaceState,
        allow_pending_rename: bool,
    ) -> Result<WorkspaceState> {
        let _resources = self.resource_guard(id, GuardMode::Shared).await?;
        let id = id.to_owned();
        self.store
            .run(move |db| {
                let tx = db.transaction()?;
                // Removal checks pending intent after reserving the lifecycle,
                // so a confirmed-deleted worktree can release its ownership.
                if state != WorkspaceState::Reconciling && !allow_pending_rename {
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
                    Some(WorkspaceState::Removing) => {
                        bail!("workspace is already being removed; it will disappear shortly")
                    }
                    Some(current) => bail!(
                        "workspace is {current}; wait for that operation to finish before another lifecycle operation"
                    ),
                };
                tx.execute(
                    "UPDATE workspaces SET state=?2 WHERE id=?1",
                    params![id, state],
                )?;
                tx.commit()?;
                Ok(current)
            })
            .await
    }

    /// Record activity so automatic cleanup restarts its idle timer, and its
    /// time for idle periods that must survive a restart.
    pub async fn touch(&self, id: &str) {
        *self.activity.lock().await.entry(id.to_owned()).or_default() += 1;
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

    pub(crate) async fn activity(&self, id: &str) -> u64 {
        self.activity.lock().await.get(id).copied().unwrap_or(0)
    }

    /// Drop in-memory bookkeeping for a workspace that no longer exists.
    pub(crate) async fn forget_workspace(&self, id: &str) {
        self.activity.lock().await.remove(id);
        self.resource_gates.lock().await.remove(id);
        self.scopes
            .lock()
            .await
            .retain(|_, caller| caller.workspace_id != id);
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

/// Fill in what a workspace record reads from its related tables.
fn load_details(db: &rusqlite::Connection, workspace: &mut Workspace) -> Result<()> {
    workspace.holds = holds::list(db, &workspace.id)?;
    workspace.review = review::list(db, &workspace.id)?;
    workspace.agent_state = agent_state::get(db, &workspace.id)?;
    stack::load(db, workspace)?;
    tree::load(db, workspace)?;
    workspace.links = store::workspace_links(db, &workspace.id)?;
    workspace.chat = db.query_row(
        "SELECT chat_id FROM workspaces WHERE id=?1",
        [&workspace.id],
        |row| row.get(0),
    )?;
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
mod tests {
    use super::*;

    #[tokio::test]
    async fn reload_replaces_the_global_config_only_when_valid() {
        let (_root, manager) = crate::test_support::manager().await;
        let path = Config::path(&manager.paths);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let before = manager.config();
        fs::write(&path, "[simulators.profiles.phone]\nruntime = 'iOS 27'\n").unwrap();
        assert!(manager.reload_config().await.is_err());
        assert!(Arc::ptr_eq(&manager.config(), &before));
        fs::write(
            &path,
            "[simulators]\ndefault = 'phone'\n\
             [simulators.profiles.phone]\ndevice = 'iPhone 17'\n",
        )
        .unwrap();
        manager.reload_config().await.unwrap();
        assert_eq!(
            manager.config().simulators.default.as_deref(),
            Some("phone")
        );
        // A snapshot taken before the reload keeps the settings it started with.
        assert_eq!(before.simulators.default, None);
    }

    #[tokio::test]
    async fn existing_base_falls_back_only_when_the_default_ref_is_missing() {
        let (root, manager) = crate::test_support::manager().await;
        let path = crate::test_support::repository(root.path(), "repo");
        // The cached remote HEAD names the default branch without reading it.
        crate::test_support::git(&path, &["switch", "-c", "topic"]);
        crate::test_support::git(&path, &["remote", "add", "origin", "/missing"]);
        crate::test_support::git(
            &path,
            &[
                "symbolic-ref",
                "refs/remotes/origin/HEAD",
                "refs/remotes/origin/main",
            ],
        );
        let topic = crate::test_support::git(&path, &["rev-parse", "topic"]);
        let repo = manager
            .register_repository(path.to_str().unwrap().into(), None, None)
            .await
            .unwrap();
        assert_eq!(
            existing_base(&repo, "topic").await.unwrap(),
            "refs/heads/main"
        );
        let main = path.join(".git/refs/heads/main");
        fs::write(&main, "garbage\n").unwrap();
        assert!(existing_base(&repo, "topic").await.is_err());
        fs::remove_file(&main).unwrap();
        assert_eq!(existing_base(&repo, "topic").await.unwrap(), topic.trim());
        manager.store.shutdown().await;
    }

    #[tokio::test]
    async fn records_report_only_running_executions() {
        let (root, manager) = crate::test_support::manager().await;
        let path = crate::test_support::repository(root.path(), "repo");
        let repo = manager
            .register_repository(path.to_str().unwrap().into(), None, None)
            .await
            .unwrap();
        let workspace = manager
            .create_workspace(&repo.id, "busy".into(), None, None, None)
            .await
            .unwrap();
        assert!(!manager.workspace(&workspace.id).await.unwrap().running);
        for (state, running) in [("running", true), ("unknown", false)] {
            let id = workspace.id.clone();
            manager
                .store
                .run(move |db| {
                    db.execute(
                        "INSERT OR REPLACE INTO executions(id,workspace_id,state) VALUES ('run',?1,?2)",
                        [&id, state],
                    )?;
                    Ok(())
                })
                .await
                .unwrap();
            assert_eq!(
                manager.workspace(&workspace.id).await.unwrap().running,
                running
            );
            assert_eq!(manager.list_workspaces().await.unwrap()[0].running, running);
        }
        manager.store.shutdown().await;
    }
}
