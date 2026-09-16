//! Shared daemon state, workspace lookup, and worktree creation.
mod executions;
mod lifecycle;
mod ownership;
mod registry;

use crate::{
    model::{Inspection, Workspace},
    paths::Paths,
    state::WorkspaceState,
    store::{self, Store},
    worktrunk,
};
use anyhow::{Context, Result, bail, ensure};
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use std::{collections::HashMap, fs, sync::Arc};
use tokio::sync::{Mutex, watch};
use uuid::Uuid;

pub struct Manager {
    pub store: Store,
    pub config: crate::config::Config,
    paths: Paths,
    pub sim_gate: Mutex<()>,
    repositories: Mutex<()>,
    git_gates: Mutex<HashMap<String, Arc<Mutex<()>>>>,
    pub scopes: Mutex<HashMap<String, (String, String)>>,
    active: Mutex<HashMap<String, watch::Sender<bool>>>,
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
            let base = match base.as_deref() {
                None | Some("main") => "refs/heads/main",
                Some(base) => base,
            };
            if base == "refs/heads/main" {
                self.refresh_main(&repo, true)
                    .await
                    .context("could not refresh main before creating workspace")?;
            }
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

    pub async fn touch(&self, id: &str) {
        *self.activity.lock().await.entry(id.to_owned()).or_default() += 1;
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
