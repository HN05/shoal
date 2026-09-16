use std::{collections::HashMap, fs, path::PathBuf, sync::Arc, time::Duration};

use crate::state::{ExecutionState, WorkspaceState};
use anyhow::{Context, Result, bail, ensure};
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use tokio::{
    process::Command,
    sync::{Mutex, watch},
    time::{Instant, sleep},
};
use uuid::Uuid;

use crate::{
    model::{ExecutionPlan, Inspection, Repository, Workspace},
    paths::Paths,
    store::{self, Store},
    worktrunk,
};

pub struct Manager {
    pub store: Store,
    pub config: crate::config::Config,
    paths: Paths,
    pub sim_gate: Mutex<()>,
    repositories: Mutex<()>,
    git_gates: Mutex<HashMap<String, Arc<Mutex<()>>>>,
    pub scopes: Mutex<HashMap<String, (String, String)>>,
    pub(crate) active: Mutex<HashMap<String, watch::Sender<bool>>>,
    activity: Mutex<HashMap<String, u64>>,
}

impl Manager {
    pub(crate) async fn git_gate(&self, repository: &str) -> Arc<Mutex<()>> {
        self.git_gates
            .lock()
            .await
            .entry(repository.to_owned())
            .or_default()
            .clone()
    }

    pub async fn open(paths: Paths) -> Result<Arc<Self>> {
        fs::create_dir_all(paths.state.join("workspaces"))?;
        fs::create_dir_all(paths.state.join("repositories"))?;
        // Avoid inheriting personal Worktrunk hooks and layout preferences.
        fs::write(paths.state.join("worktrunk.toml"), "# Managed by Shoal.\n")?;
        Ok(Arc::new(Self {
            config: crate::config::Config::load(&paths)?,
            store: Store::open(paths.state.join("state.db")).await?,
            paths,
            sim_gate: Mutex::new(()),
            repositories: Mutex::new(()),
            git_gates: Mutex::new(HashMap::new()),
            scopes: Mutex::new(HashMap::new()),
            active: Mutex::new(HashMap::new()),
            activity: Mutex::new(HashMap::new()),
        }))
    }

    pub async fn repositories(&self) -> Result<Vec<Repository>> {
        self.store
            .run(|db| {
                Ok(db
                    .prepare("SELECT * FROM repositories ORDER BY last_used DESC")?
                    .query_map([], store::repository)?
                    .collect::<rusqlite::Result<Vec<_>>>()?)
            })
            .await
    }

    pub async fn register(&self, source: String, name: Option<String>) -> Result<Repository> {
        if let Some(name) = &name {
            validate_name(name)?;
        }
        let _guard = self.repositories.lock().await;
        let repositories = self.repositories().await?;
        if let Some(repo) = repositories.iter().find(|r| r.source == source) {
            return if let Some(name) = name {
                self.rename_repository(repo.id.clone(), name).await
            } else {
                Ok(repo.clone())
            };
        }
        if let Some(identity) = crate::repository::identity(&source).await? {
            for repo in &repositories {
                if crate::repository::identity(&repo.source).await?.as_ref() == Some(&identity) {
                    return if let Some(name) = name {
                        self.rename_repository(repo.id.clone(), name).await
                    } else {
                        Ok(repo.clone())
                    };
                }
            }
        }
        let id = Uuid::new_v4().to_string();
        let path = if PathBuf::from(&source).exists() {
            let root =
                worktrunk::git(&PathBuf::from(&source), &["rev-parse", "--show-toplevel"]).await?;
            fs::canonicalize(root.trim())?
        } else {
            ensure!(
                name.as_ref().is_none_or(|name| !repositories
                    .iter()
                    .any(|repo| repo.name.as_ref() == Some(name))),
                "repository name is already in use"
            );
            ensure!(
                source.contains("://") || source.contains('@'),
                "repository path does not exist: {source}"
            );
            let path = self.paths.state.join("repositories").join(&id);
            let mut command = Command::new("git");
            command.args(["clone", "--"]).arg(&source).arg(&path);
            if let Err(error) = worktrunk::run(command).await {
                // This fresh UUID directory belongs exclusively to this attempt.
                if path.exists() {
                    fs::remove_dir_all(&path).context("clean up failed repository clone")?;
                }
                return Err(error);
            }
            fs::canonicalize(path)?
        };
        let path_string = path
            .to_str()
            .context("repository path is not UTF-8")?
            .to_owned();
        let repo = self.store.run(move |db| {
            db.execute("INSERT INTO repositories (id,path,source,last_used) VALUES (?1, ?2, ?3, (SELECT COALESCE(MAX(last_used), 0)+1 FROM repositories)) ON CONFLICT(path) DO NOTHING", params![id, path_string, source])?;
            Ok(db.query_row("SELECT * FROM repositories WHERE path=?1", [path_string], store::repository)?)
        }).await?;
        if let Some(name) = name {
            self.rename_repository(repo.id, name).await
        } else {
            Ok(repo)
        }
    }

    pub async fn rename_repository(&self, selector: String, name: String) -> Result<Repository> {
        validate_name(&name)?;
        let repo = self.repository(&selector).await?;
        self.store
            .run(move |db| {
                ensure!(
                    !db.query_row(
                        "SELECT EXISTS(SELECT 1 FROM repositories WHERE name=?1 AND id<>?2)",
                        params![name, repo.id],
                        |row| row.get::<_, bool>(0)
                    )?,
                    "repository name is already in use"
                );
                db.execute(
                    "UPDATE repositories SET name=?2 WHERE id=?1",
                    params![repo.id, name],
                )?;
                Ok(db.query_row(
                    "SELECT * FROM repositories WHERE id=?1",
                    [repo.id],
                    store::repository,
                )?)
            })
            .await
    }

    pub(crate) async fn repository(&self, selector: &str) -> Result<Repository> {
        let repositories = self.repositories().await?;
        let canonical = fs::canonicalize(selector).ok();
        if let Some(repo) = repositories.iter().find(|repo| {
            repo.id == selector
                || repo.source == selector
                || repo.path.to_str() == Some(selector)
                || canonical.as_ref() == Some(&repo.path)
        }) {
            return Ok(repo.clone());
        }
        if let Some(repo) = repositories
            .iter()
            .find(|repo| repo.name.as_deref() == Some(selector))
        {
            return Ok(repo.clone());
        }
        if let Some(identity) = crate::repository::identity(selector).await? {
            for repo in repositories {
                if crate::repository::identity(&repo.source).await?.as_ref() == Some(&identity) {
                    return Ok(repo);
                }
            }
        }
        bail!("repository is not registered: {selector}; run `shoal repo add <path-or-url>`")
    }

    pub async fn list(&self) -> Result<Vec<Workspace>> {
        self.store
            .run(|db| {
                Ok(db
                    .prepare("SELECT * FROM workspaces ORDER BY name")?
                    .query_map([], store::workspace)?
                    .collect::<rusqlite::Result<Vec<_>>>()?)
            })
            .await
    }

    pub async fn get(&self, selector: String) -> Result<Workspace> {
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

    pub async fn inspect(&self, selector: String) -> Result<Inspection> {
        let workspace = self.get(selector).await?;
        let simulators = self.simulators(Some(workspace.id.clone())).await?;
        self.store
            .run(move |db| {
                Ok(Inspection {
                    resources: crate::resources::leases(db, Some(&workspace.id))?,
                    simulators,
                    executions: store::executions(db, &workspace.id)?,
                    ports: store::ports(db, Some(&workspace.id))?,
                    workspace,
                })
            })
            .await
    }

    pub async fn add(
        &self,
        repository: String,
        name: String,
        base: Option<String>,
    ) -> Result<Workspace> {
        validate_name(&name)?;
        let repo = self.repository(&repository).await?;
        let gate = self.git_gate(&repo.id).await;
        let _guard = gate.lock().await;
        let branch = self.available_branch(&repo, &name).await?;
        let id = Uuid::new_v4().to_string();
        let workspace = Workspace {
            branch,
            id,
            repository_id: repo.id.clone(),
            path: self.paths.state.join("workspaces").join(&name),
            name,
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
        let record = workspace.clone();
        self.store.run(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            ensure!(!tx.query_row("SELECT EXISTS(SELECT 1 FROM workspaces WHERE name=?1)", [&record.name], |r| r.get::<_, bool>(0))?, "workspace name already exists: {}", record.name);
            tx.execute("INSERT INTO workspaces (id,repository_id,name,path,branch,state) VALUES (?1,?2,?3,?4,?5,?6)", params![record.id, record.repository_id, record.name, record.path.to_str(), record.branch, record.state])?;
            tx.execute("UPDATE repositories SET last_used=(SELECT COALESCE(MAX(last_used),0)+1 FROM repositories) WHERE id=?1", [record.repository_id])?;
            tx.commit()?;
            Ok(())
        }).await?;
        let result = async {
            let base = base.as_deref().unwrap_or("HEAD");
            let commit = worktrunk::git(
                &repo.path,
                &["rev-parse", "--verify", &format!("{base}^{{commit}}")],
            )
            .await?
            .trim()
            .to_owned();
            let reference =
                worktrunk::git(&repo.path, &["rev-parse", "--symbolic-full-name", base]).await?;
            let reference = reference.trim();
            let reference = reference.starts_with("refs/").then(|| reference.to_owned());
            let (record_id, record_commit) = (workspace.id.clone(), commit.clone());
            self.store
                .run(move |db| {
                    db.execute(
                        "UPDATE workspaces SET base_commit=?2,base_ref=?3 WHERE id=?1",
                        params![record_id, record_commit, reference],
                    )?;
                    Ok(())
                })
                .await?;
            worktrunk::create(
                &repo.path,
                &self.paths.state.join("worktrunk.toml"),
                &workspace.path,
                &workspace.branch,
                &commit,
            )
            .await?;
            self.record_worktree_identity(&workspace).await
        }
        .await;
        match result {
            Ok(()) => {
                self.set_state(&workspace.id, WorkspaceState::Ready, None)
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
        self.get(workspace.id).await
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

    pub(crate) async fn verify_worktree(&self, workspace: &Workspace) -> Result<()> {
        let repo = self.repository(&workspace.repository_id).await?;
        // Verify this is still the checkout Shoal created before any deletion.
        let root = worktrunk::git(&workspace.path, &["rev-parse", "--show-toplevel"]).await?;
        ensure!(
            fs::canonicalize(root.trim())? == fs::canonicalize(&workspace.path)?,
            "workspace path no longer points to its worktree root"
        );
        let expected = worktrunk::git(
            &repo.path,
            &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        )
        .await?;
        let actual = worktrunk::git(
            &workspace.path,
            &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        )
        .await?;
        ensure!(
            fs::canonicalize(expected.trim())? == fs::canonicalize(actual.trim())?,
            "workspace now belongs to a different repository"
        );
        let actual_git_dir = worktrunk::git(
            &workspace.path,
            &["rev-parse", "--path-format=absolute", "--git-dir"],
        )
        .await?;
        let actual_git_dir = fs::canonicalize(actual_git_dir.trim())?;
        ensure!(
            actual_git_dir != fs::canonicalize(actual.trim())?,
            "workspace was replaced by a main repository checkout"
        );
        if let Some(identity) = &workspace.git_dir_id {
            ensure!(
                crate::recovery::directory_identity(&actual_git_dir)? == *identity,
                "Git worktree metadata was replaced; ownership cannot be verified"
            );
        }
        if let Some(expected) = &workspace.git_dir {
            ensure!(
                fs::canonicalize(expected)? == actual_git_dir,
                "workspace path now refers to a different Git worktree"
            );
        }
        Ok(())
    }

    pub(crate) async fn record_worktree_identity(&self, workspace: &Workspace) -> Result<()> {
        let git_dir = worktrunk::git(
            &workspace.path,
            &["rev-parse", "--path-format=absolute", "--git-dir"],
        )
        .await?;
        let git_dir = fs::canonicalize(git_dir.trim())?;
        let identity = crate::recovery::directory_identity(&git_dir)?;
        let id = workspace.id.clone();
        self.store
            .run(move |db| {
                db.execute(
                    "UPDATE workspaces SET git_dir=?2,git_dir_id=?3 WHERE id=?1",
                    params![id, git_dir.to_str(), identity],
                )?;
                Ok(())
            })
            .await
    }

    pub async fn check_removal(
        &self,
        selector: String,
        caller_pid: u32,
    ) -> Result<crate::removal::RemovalCheck> {
        let inspection = self.inspect(selector).await?;
        if inspection.workspace.path.exists() {
            self.verify_worktree(&inspection.workspace).await?;
        }
        crate::removal::check(
            inspection.workspace,
            inspection.executions.len(),
            caller_pid,
        )
        .await
    }

    pub async fn remove(
        &self,
        selector: String,
        choice: crate::removal::Choice,
        caller_pid: u32,
    ) -> Result<crate::removal::RemovalResult> {
        self.remove_with_guard(selector, choice, caller_pid, None)
            .await
    }

    pub async fn cleanup_snapshot(&self, id: &str) -> Result<Option<u64>> {
        if !self.list_resources(Some(id.into())).await?.is_empty() {
            return Ok(None);
        }
        if self
            .simulators(Some(id.into()))
            .await?
            .iter()
            .any(|s| s.workspace_id.is_some())
        {
            return Ok(None);
        }
        let check = self.check_removal(id.to_owned(), 0).await?;
        if !check.safe() || !check.workspace.path.is_dir() {
            return Ok(None);
        }
        let head = worktrunk::git(&check.workspace.path, &["rev-parse", "HEAD"]).await?;
        let activity = self.activity.lock().await.get(id).copied().unwrap_or(0);
        let path = check.workspace.path;
        Ok(Some(
            tokio::task::spawn_blocking(move || {
                crate::cleanup::fingerprint(&path, &head, activity)
            })
            .await??,
        ))
    }

    pub async fn remove_idle(&self, selector: String, snapshot: u64) -> Result<()> {
        self.remove_with_guard(selector, crate::removal::Choice::Auto, 0, Some(snapshot))
            .await
            .map(|_| ())
    }

    async fn remove_with_guard(
        &self,
        selector: String,
        choice: crate::removal::Choice,
        caller_pid: u32,
        expected_snapshot: Option<u64>,
    ) -> Result<crate::removal::RemovalResult> {
        use crate::removal::{Choice, RemovalResult};
        let automatic = expected_snapshot.is_some();
        let workspace = self.get(selector).await?;
        let id = workspace.id.clone();
        self.store
            .run(move |db| {
                let changed = db.execute(
                    "UPDATE workspaces SET state=?2 WHERE id=?1 AND state IN (?3,?4)",
                    params![
                        id,
                        WorkspaceState::Removing,
                        WorkspaceState::Ready,
                        WorkspaceState::Failed
                    ],
                )?;
                ensure!(changed == 1, "workspace is busy");
                Ok(())
            })
            .await?;
        let result = async {
            let repo = self.repository(&workspace.repository_id).await?;
            let outcome = if workspace.path.exists() {
                let check = self.check_removal(workspace.id.clone(), caller_pid).await?;
                ensure!(!automatic || check.safe(), "workspace is no longer idle, clean and fully pushed");
                ensure!(automatic || !matches!(choice, Choice::Auto) || !check.needs_choice(),
                    "removal requires a branch choice: {}; use --yes with --keep-branch or --delete-branch", check.warnings().join("; "));
                self.stop_executions(&workspace.id, !automatic).await?;
                if let Some(expected) = expected_snapshot {
                    ensure!(
                        self.cleanup_snapshot(&workspace.id).await? == Some(expected),
                        "workspace changed before automatic removal"
                    );
                }
                let check = self.check_removal(workspace.id.clone(), caller_pid).await?;
                ensure!(!automatic || check.safe(), "workspace changed while stopping commands");
                ensure!(automatic || !matches!(choice, Choice::Auto) || !check.needs_choice(),
                    "workspace changed while stopping commands; choose whether to keep or delete the branch");
                let delete_branch = match choice {
                    Choice::Auto => check.can_delete_branch(),
                    Choice::KeepBranch => false,
                    Choice::DeleteBranch => true,
                };
                ensure!(!matches!(choice, Choice::KeepBranch) || check.branch.is_some() || check.unpushed_commits == 0,
                    "detached HEAD has unpushed commits; create a branch before choosing to keep it");
                // Live resources are removed before the directory; failed cleanup
                // retains their ownership records so removal can be retried.
                self.remove_simulators(&workspace.id).await?;
                worktrunk::remove(
                    &repo.path,
                    &self.paths.state.join("worktrunk.toml"),
                    &workspace.path,
                    !matches!(choice, Choice::Auto),
                    delete_branch,
                )
                .await?
            } else {
                ensure!(
                    workspace.state == WorkspaceState::Failed,
                    "workspace directory disappeared; manual reconciliation required"
                );
                ensure!(self.missing_worktree(&workspace).await?.is_none(),
                    "worktree was moved; restore its recorded path before removing it");
                self.stop_executions(&workspace.id, !automatic).await?;
                self.remove_simulators(&workspace.id).await?;
                if self.missing_registration(&workspace).await? {
                    // Prune only this owned registration, through Worktrunk, and
                    // retain its branch because the contents cannot be inspected.
                    worktrunk::remove(&repo.path, &self.paths.state.join("worktrunk.toml"),
                        &workspace.path, true, false).await?
                } else {
                    RemovalResult { removed: true, branch: Some(workspace.branch.clone()), branch_deleted: false, branch_outcome: "retained".into() }
                }
            };
            self.remove_simulators(&workspace.id).await?;
            let id = workspace.id.clone();
            self.store
                .run(move |db| {
                    let tx = db.transaction()?;
                    tx.execute("DELETE FROM executions WHERE workspace_id=?1", [&id])?;
                    tx.execute("DELETE FROM workspaces WHERE id=?1", [id])?;
                    tx.commit()?;
                    Ok(outcome)
                })
                .await
        }
        .await;
        match result {
            Ok(outcome) => {
                self.activity.lock().await.remove(&workspace.id);
                self.scopes
                    .lock()
                    .await
                    .retain(|_, (_, owner)| owner != &workspace.id);
                Ok(outcome)
            }
            Err(error) => {
                self.set_state(&workspace.id, workspace.state, Some(format!("{error:#}")))
                    .await?;
                Err(error)
            }
        }
    }

    pub async fn begin(
        &self,
        selector: String,
        wrapper: Option<crate::process_identity::Identity>,
    ) -> Result<(ExecutionPlan, watch::Receiver<bool>)> {
        if let Some(wrapper) = &wrapper {
            ensure!(
                crate::process_identity::alive(wrapper)?,
                "execution wrapper is no longer alive"
            );
        }
        // Coordinate registration and stop notification without holding the map
        // during any external command or lifetime of the agent.
        let mut active = self.active.lock().await;
        let workspace = self.get(selector).await?;
        ensure!(workspace.path.is_dir(), "workspace directory is missing");
        self.verify_worktree(&workspace).await?;
        *self
            .activity
            .lock()
            .await
            .entry(workspace.id.clone())
            .or_default() += 1;
        let id = Uuid::new_v4().to_string();
        let (execution_id, workspace_id) = (id.clone(), workspace.id.clone());
        let ports = self
            .store
            .run(move |db| {
                let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
                let ready: bool = tx.query_row(
                    "SELECT state='ready' FROM workspaces WHERE id=?1",
                    [&workspace_id],
                    |r| r.get(0),
                )?;
                ensure!(ready, "workspace is not ready");
                tx.execute(
                    "INSERT INTO executions(id,workspace_id,state,wrapper) VALUES (?1,?2,?3,?4)",
                    params![
                        execution_id,
                        workspace_id,
                        ExecutionState::Running,
                        wrapper.map(|w| serde_json::to_string(&w)).transpose()?
                    ],
                )?;
                let ports = store::ports(&tx, Some(&workspace_id))?;
                tx.commit()?;
                Ok(ports)
            })
            .await?;
        let (sender, receiver) = watch::channel(false);
        active.insert(id.clone(), sender);
        let scope_token = Uuid::new_v4().to_string();
        self.scopes
            .lock()
            .await
            .insert(scope_token.clone(), (id.clone(), workspace.id.clone()));
        Ok((
            ExecutionPlan {
                scope_token,
                id,
                workspace,
                ports,
            },
            receiver,
        ))
    }

    pub async fn touch(&self, id: &str) {
        *self.activity.lock().await.entry(id.to_owned()).or_default() += 1;
    }

    pub async fn record_execution_child(
        &self,
        id: String,
        child: Option<crate::process_identity::Identity>,
        group_id: u32,
    ) -> Result<()> {
        ensure!(
            group_id > 1 && group_id <= i32::MAX as u32,
            "invalid execution process group"
        );
        ensure!(
            child.as_ref().is_none_or(|child| child.pid == group_id),
            "child must lead its execution process group"
        );
        self.store.run(move |db| {
            ensure!(db.execute("UPDATE executions SET child=?2,group_id=?3 WHERE id=?1 AND group_id IS NULL",
                params![id, child.map(|c| serde_json::to_string(&c)).transpose()?, group_id])? == 1,
                "execution already registered or no longer exists");
            Ok(())
        }).await
    }

    pub async fn finish(&self, id: String, complete: bool) -> Result<bool> {
        let complete = if complete {
            match crate::process_identity::scan(std::collections::HashSet::from([id.clone()])).await
            {
                Ok(scan) => scan.processes.is_empty(),
                Err(_) => false,
            }
        } else {
            false
        };
        let mut active = self.active.lock().await;
        let record_id = id.clone();
        self.store
            .run(move |db| {
                if complete {
                    db.execute("DELETE FROM executions WHERE id=?1", [record_id])?;
                } else {
                    db.execute(
                        "UPDATE executions SET state=?2 WHERE id=?1",
                        params![record_id, ExecutionState::Unknown],
                    )?;
                }
                Ok(())
            })
            .await?;
        active.remove(&id);
        self.scopes
            .lock()
            .await
            .retain(|_, (execution, _)| execution != &id);
        Ok(complete)
    }

    pub async fn stop(&self, selector: String) -> Result<()> {
        let workspace = self.get(selector).await?;
        let id = workspace.id.clone();
        self.store
            .run(move |db| {
                ensure!(
                    db.execute(
                        "UPDATE workspaces SET state=?2 WHERE id=?1 AND state IN (?3,?4)",
                        params![
                            id,
                            WorkspaceState::Stopping,
                            WorkspaceState::Ready,
                            WorkspaceState::Failed
                        ]
                    )? == 1,
                    "workspace is busy or not ready"
                );
                Ok(())
            })
            .await?;
        let result = self.stop_executions(&workspace.id, false).await;
        self.set_state(
            &workspace.id,
            workspace.state,
            result
                .as_ref()
                .err()
                .map(|e| format!("{e:#}"))
                .or(workspace.error),
        )
        .await?;
        result
    }

    async fn stop_disconnected(
        &self,
        execution: &crate::model::Execution,
        manual_removal: bool,
    ) -> Result<()> {
        use crate::process_identity as process;
        let scan = process::scan(std::collections::HashSet::from([execution.id.clone()])).await?;
        let (related, unverified) =
            process::related(execution.child.as_ref(), execution.group_id).await?;
        let mut targets: Vec<_> = scan.processes.into_iter().map(|p| p.identity).collect();
        targets.extend(related);
        for identity in [&execution.wrapper, &execution.child].into_iter().flatten() {
            if process::alive(identity)? {
                targets.push(identity.clone());
            }
        }
        process::stop_verified(&targets).await?;
        let after = process::scan(std::collections::HashSet::from([execution.id.clone()])).await?;
        ensure!(
            after.processes.is_empty(),
            "owned processes survived stopping; retry after shoal reconcile"
        );
        // Manual removal retains its policy: unrelated/unverifiable processes do
        // not block deletion. A plain stop must not claim those processes stopped.
        if !manual_removal {
            ensure!(
                execution.wrapper.is_some()
                    && execution.group_id.is_some()
                    && unverified.is_empty(),
                "execution ownership is incomplete; use shoal reconcile to inspect it"
            );
            let (related, candidates) =
                process::related(execution.child.as_ref(), execution.group_id).await?;
            ensure!(
                related.is_empty()
                    && candidates.is_empty()
                    && after
                        .unreadable
                        .iter()
                        .all(|p| !p.not_older_than(execution.wrapper.as_ref().unwrap())),
                "process state remains uncertain; use shoal reconcile to inspect it"
            );
            let id = execution.id.clone();
            self.store
                .run(move |db| {
                    db.execute("DELETE FROM executions WHERE id=?1", [id])?;
                    Ok(())
                })
                .await?;
        }
        Ok(())
    }

    async fn stop_executions(&self, id: &str, allow_disconnected: bool) -> Result<()> {
        let id = id.to_owned();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let active = self.active.lock().await;
            let query_id = id.clone();
            let executions = self
                .store
                .run(move |db| store::executions(db, &query_id))
                .await?;
            if executions.is_empty() {
                return Ok(());
            }
            let mut connected = 0;
            for execution in executions {
                match active
                    .get(&execution.id)
                    .filter(|sender| !sender.is_closed())
                {
                    Some(sender) => {
                        sender
                            .send(true)
                            .context("execution disconnected during stop")?;
                        connected += 1;
                    }
                    None => {
                        self.stop_disconnected(&execution, allow_disconnected)
                            .await?
                    }
                }
            }
            if connected == 0 {
                return Ok(());
            }
            drop(active);
            ensure!(
                Instant::now() < deadline,
                "timed out waiting for workspace processes to stop"
            );
            sleep(Duration::from_millis(50)).await;
        }
    }
}

pub(crate) fn validate_name(name: &str) -> Result<()> {
    ensure!(
        !name.is_empty()
            && name.len() <= 64
            && name.as_bytes()[0].is_ascii_alphanumeric()
            && name
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_'),
        "workspace names must be 1–64 ASCII letters, digits, hyphens or underscores, starting with a letter or digit"
    );
    Ok(())
}
