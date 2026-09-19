//! Shared daemon state, workspace lookup, and worktree creation.
mod executions;
mod lifecycle;
mod ownership;
mod registry;
mod repo_configuration;
mod repo_removal;
mod status;

pub use executions::ExecutionKind;

use crate::{
    config::Config,
    git,
    model::{Inspection, Workspace},
    paths::Paths,
    scope::Caller,
    state::WorkspaceState,
    store::{self, Store},
    worktrunk,
};
use anyhow::{Context, Result, bail, ensure};
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use std::{collections::HashMap, fs, sync::Arc};
use tokio::sync::{Mutex, watch};
use uuid::Uuid;

pub(crate) enum WorkspaceSource {
    New(Option<String>),
    Existing(crate::existing_branch::Branch),
}

pub struct Manager {
    pub store: Store,
    pub(crate) pr_gate: Mutex<()>,
    pub cleanup_notify: tokio::sync::Notify,
    pub config: Config,
    paths: Paths,
    /// Serializes every simctl transition.
    pub(crate) simulator_gate: Mutex<()>,
    /// Serializes repository registration, removal, and local config changes.
    registry_gate: Mutex<()>,
    /// Per-repository gate for ref updates and worktree creation/removal.
    git_gates: Mutex<HashMap<String, Arc<Mutex<()>>>>,
    /// Scope token → the execution it was issued to.
    scopes: Mutex<HashMap<String, Caller>>,
    /// Connected executions and the channel that asks their wrapper to stop.
    connections: Mutex<HashMap<String, watch::Sender<bool>>>,
    /// Per-workspace activity counters folded into cleanup fingerprints.
    activity: Mutex<HashMap<String, u64>>,
    /// The newest recorded notification ID; wakes `shoal notifications --follow`.
    pub(crate) notifications_changed: watch::Sender<i64>,
}

impl Manager {
    pub async fn open(paths: Paths) -> Result<Arc<Self>> {
        paths.prepare()?;
        // Avoid inheriting personal Worktrunk hooks and layout preferences.
        fs::write(paths.worktrunk_config(), "# Managed by Shoal.\n")?;
        Ok(Arc::new(Self {
            config: Config::load(&paths)?,
            pr_gate: Mutex::new(()),
            cleanup_notify: tokio::sync::Notify::new(),
            store: Store::open(paths.database()).await?,
            paths,
            simulator_gate: Mutex::new(()),
            registry_gate: Mutex::new(()),
            git_gates: Mutex::new(HashMap::new()),
            scopes: Mutex::new(HashMap::new()),
            connections: Mutex::new(HashMap::new()),
            activity: Mutex::new(HashMap::new()),
            notifications_changed: watch::channel(0).0,
        }))
    }

    pub(crate) async fn git_gate(&self, repository: &str) -> Arc<Mutex<()>> {
        self.git_gates
            .lock()
            .await
            .entry(repository.to_owned())
            .or_default()
            .clone()
    }

    pub(crate) async fn caller(&self, token: &str) -> Option<Caller> {
        self.scopes.lock().await.get(token).cloned()
    }

    /// Bind a scope token to the execution it was issued to.
    pub(crate) async fn issue_scope(&self, token: String, caller: Caller) {
        self.scopes.lock().await.insert(token, caller);
    }

    pub async fn list_workspaces(&self) -> Result<Vec<Workspace>> {
        self.store
            .run(|db| {
                Ok(db
                    .prepare("SELECT * FROM workspaces ORDER BY name")?
                    .query_map([], store::workspace)?
                    .collect::<rusqlite::Result<Vec<_>>>()?)
            })
            .await
    }

    /// Look a workspace up by ID or name.
    pub async fn workspace(&self, selector: &str) -> Result<Workspace> {
        let selector = selector.to_owned();
        self.store
            .run(move |db| {
                db.query_row(
                    "SELECT * FROM workspaces WHERE id=?1 OR name=?1",
                    [&selector],
                    store::workspace,
                )
                .optional()?
                .with_context(|| format!("unknown workspace: {selector}"))
            })
            .await
    }

    /// Resolve an optional selector to a workspace ID filter.
    pub async fn workspace_filter(&self, selector: Option<&str>) -> Result<Option<String>> {
        match selector {
            Some(selector) => Ok(Some(self.workspace(selector).await?.id)),
            None => Ok(None),
        }
    }

    pub async fn inspect_workspace(&self, selector: &str) -> Result<Inspection> {
        let workspace = self.workspace(selector).await?;
        let simulators = self.list_simulators(Some(&workspace.id)).await?;
        let pr_cleanup = self.pr_registration(&workspace.id).await?;
        self.store
            .run(move |db| {
                Ok(Inspection {
                    pr_cleanup,
                    executions: store::executions(db, &workspace.id)?,
                    ports: store::ports(db, Some(&workspace.id))?,
                    resources: crate::resources::leases(db, Some(&workspace.id))?,
                    simulators,
                    workspace,
                })
            })
            .await
    }

    pub async fn create_workspace(
        &self,
        repository: &str,
        name: String,
        base: Option<String>,
        git_profile: Option<&str>,
    ) -> Result<Workspace> {
        let repo = self.repository(repository).await?;
        git::check_branch_name(Some(&repo.path), &name).await?;
        let gate = self.git_gate(&repo.id).await;
        let _guard = gate.lock().await;
        // Removal may have completed or failed while this request waited.
        self.repository(&repo.id).await?;
        self.ensure_repository_available(&repo.id).await?;
        let branch = self.available_branch(&repo, &name).await?;
        self.create_branch_workspace(&repo, name, branch, WorkspaceSource::New(base), git_profile)
            .await
    }

    // The caller holds the per-repository Git gate through materialization.
    pub(crate) async fn create_branch_workspace(
        &self,
        repo: &crate::model::Repository,
        name: String,
        branch: String,
        source: WorkspaceSource,
        git_profile: Option<&str>,
    ) -> Result<Workspace> {
        if let Some(name) = git_profile {
            self.config.git.profile(name)?;
        }
        let (base, existing) = match source {
            WorkspaceSource::New(base) => (base, None),
            WorkspaceSource::Existing(branch) => (None, Some(branch)),
        };
        let name = derive_workspace_name(&name);
        let workspace = Workspace {
            id: Uuid::new_v4().to_string(),
            repository_id: repo.id.clone(),
            path: self.workspaces_dir(repo).await?.join(&name),
            name,
            branch,
            state: WorkspaceState::Preparing,
            error: None,
            base_commit: None,
            base_ref: None,
            git_dir: None,
            git_dir_id: None,
        };
        ensure!(
            !workspace.path.exists(),
            "workspace path already exists: {}",
            workspace.path.display()
        );
        self.insert_workspace(workspace.clone()).await?;
        let result = self
            .materialize_worktree(repo, &workspace, base, existing, git_profile)
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

    async fn insert_workspace(&self, record: Workspace) -> Result<()> {
        self.store
            .run(move |db| {
                let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
                let taken: bool = tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM workspaces WHERE name=?1)",
                    [&record.name],
                    |r| r.get(0),
                )?;
                ensure!(!taken, "workspace name already exists: {}", record.name);
                tx.execute(
                    "INSERT INTO workspaces (id,repository_id,name,path,branch,state) VALUES (?1,?2,?3,?4,?5,?6)",
                    params![
                        record.id,
                        record.repository_id,
                        record.name,
                        record.path.to_str(),
                        record.branch,
                        record.state
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

    /// Resolve the base ref, record it, and create the worktree. Returns
    /// whether a setup command still has to run.
    async fn materialize_worktree(
        &self,
        repo: &crate::model::Repository,
        workspace: &Workspace,
        base: Option<String>,
        existing: Option<crate::existing_branch::Branch>,
        git_profile: Option<&str>,
    ) -> Result<bool> {
        if let Some(branch) = &existing {
            self.materialize_branch(repo, branch).await?;
        }
        let base = if existing.is_some() {
            Some(
                match crate::default_branch::resolve(&repo.path, false).await {
                    Ok(name)
                        if name != workspace.branch
                            && git::run_isolated(
                                &repo.path,
                                &["show-ref", "--verify", "--", &format!("refs/heads/{name}")],
                            )
                            .await
                            .is_ok() =>
                    {
                        format!("refs/heads/{name}")
                    }
                    _ => git::run_isolated(
                        &repo.path,
                        &[
                            "rev-parse",
                            "--verify",
                            &format!("refs/heads/{}", workspace.branch),
                        ],
                    )
                    .await?
                    .trim()
                    .to_owned(),
                },
            )
        } else {
            base
        };
        let default = crate::default_branch::resolve(&repo.path, base.is_none()).await;
        // An explicit ref remains an escape hatch when remote default-branch
        // discovery is unavailable. It does not implicitly refresh another ref.
        let default = if base.is_none() {
            Some(default?)
        } else {
            default.ok()
        };
        let default_ref = default.as_ref().map(|name| format!("refs/heads/{name}"));
        let base = base
            .as_deref()
            .or(default_ref.as_deref())
            .context("workspace base is unknown")?;
        let refresh = existing.is_none()
            && (default.as_deref() == Some(base) || default_ref.as_deref() == Some(base));
        if refresh {
            self.refresh_branch(repo, default.as_deref().unwrap(), true)
                .await
                .context("could not refresh the default branch before creating workspace")?;
        }
        let base = if refresh {
            default_ref.as_deref().unwrap()
        } else {
            base
        };
        let commit = git::run(
            &repo.path,
            &["rev-parse", "--verify", &format!("{base}^{{commit}}")],
        )
        .await?
        .trim()
        .to_owned();
        let reference = git::run(&repo.path, &["rev-parse", "--symbolic-full-name", base]).await?;
        let reference = reference.trim_end_matches('\n');
        let reference = reference.starts_with("refs/").then(|| reference.to_owned());
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
        worktrunk::create(
            &repo.path,
            &self.paths.worktrunk_config(),
            &workspace.path,
            &workspace.branch,
            existing.is_none().then_some(commit.as_str()),
        )
        .await?;
        self.record_worktree_identity(workspace).await?;
        let config = self.workspace_config(workspace).await?;
        if let Some(name) = git_profile
            .or(config.git_profile.as_deref())
            .or(self.config.git_profile.as_deref())
        {
            crate::git_profile::apply(&workspace.path, self.config.git.profile(name)?)
                .await
                .with_context(|| format!("apply git profile {name}"))?;
        }
        Ok(config.setup_cmd.is_some())
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
    pub(crate) async fn reserve_lifecycle(&self, id: &str, state: WorkspaceState) -> Result<()> {
        let id = id.to_owned();
        self.store
            .run(move |db| {
                let changed = db.execute(
                    "UPDATE workspaces SET state=?2 WHERE id=?1 AND state IN (?3,?4)",
                    params![id, state, WorkspaceState::Ready, WorkspaceState::Failed],
                )?;
                if changed == 1 {
                    return Ok(());
                }
                // Name the operation that holds the workspace; automatic
                // cleanup may be removing it at the same moment as a user.
                let current: Option<WorkspaceState> = db
                    .query_row("SELECT state FROM workspaces WHERE id=?1", [&id], |row| {
                        row.get(0)
                    })
                    .optional()?;
                match current {
                    None => bail!("workspace no longer exists"),
                    Some(WorkspaceState::Removing) => {
                        bail!("workspace is already being removed; it will disappear shortly")
                    }
                    Some(current) => bail!(
                        "workspace is {current}; wait for that operation to finish before another lifecycle operation"
                    ),
                }
            })
            .await
    }

    /// Record activity so automatic cleanup restarts its idle timer.
    pub async fn touch(&self, id: &str) {
        *self.activity.lock().await.entry(id.to_owned()).or_default() += 1;
    }

    pub(crate) async fn activity(&self, id: &str) -> u64 {
        self.activity.lock().await.get(id).copied().unwrap_or(0)
    }

    /// Drop in-memory bookkeeping for a workspace that no longer exists.
    pub(crate) async fn forget_workspace(&self, id: &str) {
        self.activity.lock().await.remove(id);
        self.scopes
            .lock()
            .await
            .retain(|_, caller| caller.workspace_id != id);
    }
}

/// Keep directory/selector names portable without restricting Git branch syntax.
fn derive_workspace_name(branch: &str) -> String {
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
