use std::{collections::HashMap, fs, path::PathBuf, sync::Arc, time::Duration};

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
    paths: Paths,
    repositories: Mutex<()>,
    active: Mutex<HashMap<String, watch::Sender<bool>>>,
    activity: Mutex<HashMap<String, u64>>,
}

impl Manager {
    pub async fn open(paths: Paths) -> Result<Arc<Self>> {
        fs::create_dir_all(paths.state.join("workspaces"))?;
        fs::create_dir_all(paths.state.join("repositories"))?;
        // Avoid inheriting personal Worktrunk hooks and layout preferences.
        fs::write(paths.state.join("worktrunk.toml"), "# Managed by Shoal.\n")?;
        Ok(Arc::new(Self {
            store: Store::open(paths.state.join("state.db")).await?,
            paths,
            repositories: Mutex::new(()),
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

    pub async fn register(&self, source: String) -> Result<Repository> {
        let _guard = self.repositories.lock().await;
        let repositories = self.repositories().await?;
        if let Some(repo) = repositories.iter().find(|r| r.source == source) {
            return Ok(repo.clone());
        }
        if let Some(identity) = crate::repository::identity(&source).await? {
            for repo in &repositories {
                if crate::repository::identity(&repo.source).await?.as_ref() == Some(&identity) {
                    return Ok(repo.clone());
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
        self.store.run(move |db| {
            db.execute("INSERT INTO repositories VALUES (?1, ?2, ?3, (SELECT COALESCE(MAX(last_used), 0)+1 FROM repositories)) ON CONFLICT(path) DO NOTHING", params![id, path_string, source])?;
            Ok(db.query_row("SELECT * FROM repositories WHERE path=?1", [path_string], store::repository)?)
        }).await
    }

    async fn repository(&self, selector: &str) -> Result<Repository> {
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
        self.store
            .run(move |db| {
                Ok(Inspection {
                    executions: store::executions(db, &workspace.id)?,
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
        let id = Uuid::new_v4().to_string();
        let workspace = Workspace {
            branch: format!("shoal/{name}-{}", &id[..8]),
            id,
            repository_id: repo.id.clone(),
            path: self.paths.state.join("workspaces").join(&name),
            name,
            state: "preparing".into(),
            error: None,
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
            tx.execute("INSERT INTO workspaces VALUES (?1,?2,?3,?4,?5,?6,NULL)", params![record.id, record.repository_id, record.name, record.path.to_str(), record.branch, record.state])?;
            tx.execute("UPDATE repositories SET last_used=(SELECT COALESCE(MAX(last_used),0)+1 FROM repositories) WHERE id=?1", [record.repository_id])?;
            tx.commit()?;
            Ok(())
        }).await?;
        let result = worktrunk::create(
            &repo.path,
            &self.paths.state.join("worktrunk.toml"),
            &workspace.path,
            &workspace.branch,
            base.as_deref().unwrap_or("HEAD"),
        )
        .await;
        match result {
            Ok(()) => self.set_state(&workspace.id, "ready", None).await?,
            Err(error) => {
                self.set_state(&workspace.id, "failed", Some(format!("{error:#}")))
                    .await?;
                bail!(
                    "workspace {} failed; inspect or remove it with Shoal: {error:#}",
                    workspace.name
                );
            }
        }
        self.get(workspace.id).await
    }

    async fn set_state(&self, id: &str, state: &str, error: Option<String>) -> Result<()> {
        let (id, state) = (id.to_owned(), state.to_owned());
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

    async fn verify_worktree(&self, workspace: &Workspace) -> Result<()> {
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
        Ok(())
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
        self.store.run(move |db| {
            let changed = db.execute("UPDATE workspaces SET state='removing' WHERE id=?1 AND state IN ('ready','failed')", [id])?;
            ensure!(changed == 1, "workspace is busy");
            Ok(())
        }).await?;
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
                // Future ports/simulator leases belong to the worktree. Release or
                // reset them here, before deleting its directory and ownership record.
                // Manual and automatic removal share this exact path.
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
                    workspace.state == "failed",
                    "workspace directory disappeared; manual reconciliation required"
                );
                RemovalResult { removed: true, branch: None, branch_deleted: false, branch_outcome: "not_attempted".into() }
            };
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
                Ok(outcome)
            }
            Err(error) => {
                self.set_state(&workspace.id, &workspace.state, Some(format!("{error:#}")))
                    .await?;
                Err(error)
            }
        }
    }

    pub async fn begin(&self, selector: String) -> Result<(ExecutionPlan, watch::Receiver<bool>)> {
        // Coordinate registration and stop notification without holding the map
        // during any external command or lifetime of the agent.
        let mut active = self.active.lock().await;
        let workspace = self.get(selector).await?;
        ensure!(workspace.path.is_dir(), "workspace directory is missing");
        *self
            .activity
            .lock()
            .await
            .entry(workspace.id.clone())
            .or_default() += 1;
        let id = Uuid::new_v4().to_string();
        let (execution_id, workspace_id) = (id.clone(), workspace.id.clone());
        self.store
            .run(move |db| {
                let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
                let ready: bool = tx.query_row(
                    "SELECT state='ready' FROM workspaces WHERE id=?1",
                    [&workspace_id],
                    |r| r.get(0),
                )?;
                ensure!(ready, "workspace is not ready");
                tx.execute(
                    "INSERT INTO executions VALUES (?1,?2,'running')",
                    params![execution_id, workspace_id],
                )?;
                tx.commit()?;
                Ok(())
            })
            .await?;
        let (sender, receiver) = watch::channel(false);
        active.insert(id.clone(), sender);
        Ok((ExecutionPlan { id, workspace }, receiver))
    }

    pub async fn finish(&self, id: String, complete: bool) -> Result<()> {
        let mut active = self.active.lock().await;
        let record_id = id.clone();
        self.store
            .run(move |db| {
                if complete {
                    db.execute("DELETE FROM executions WHERE id=?1", [record_id])?;
                } else {
                    db.execute(
                        "UPDATE executions SET state='unknown' WHERE id=?1",
                        [record_id],
                    )?;
                }
                Ok(())
            })
            .await?;
        active.remove(&id);
        Ok(())
    }

    pub async fn stop(&self, selector: String) -> Result<()> {
        let workspace = self.get(selector).await?;
        let id = workspace.id.clone();
        self.store
            .run(move |db| {
                ensure!(
                    db.execute(
                        "UPDATE workspaces SET state='stopping' WHERE id=?1 AND state='ready'",
                        [id]
                    )? == 1,
                    "workspace is busy or not ready"
                );
                Ok(())
            })
            .await?;
        let result = self.stop_executions(&workspace.id, false).await;
        self.set_state(
            &workspace.id,
            "ready",
            result.as_ref().err().map(|e| format!("{e:#}")),
        )
        .await?;
        result
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
                    None if allow_disconnected => {}
                    None => {
                        bail!("execution is no longer connected; processes require reconciliation")
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

pub fn validate_name(name: &str) -> Result<()> {
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
