//! A stand-in for the rest of the daemon, which the tests of reviews build
//! the service over: one workspace, a worktree of a real repository on its
//! own branch, recorded in a temporary store, under a global config a test
//! gives, whose one-shot runs the agent queue admits at once, except a run
//! protection stopped, which waits until a test says load recovered.
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, bail};
use futures_util::future::BoxFuture;
use rusqlite::Connection;
use shoal_daemon_base::gates::Gates;
use shoal_forge::account::Account;
use shoal_ids::repository_ids::RepositoryId;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::repository::Selector;
use shoal_protocol::space_names::{SpaceName, SpaceSelector};
use shoal_refs::CommitId;
use shoal_store::Store;
use tokio::sync::watch;

use crate::{
    Reviews,
    config::{Config, Effective, repo::ConfigLayers},
    daemon::{Admission, Agents, Forge, Records, Repositories, Settings, Spaces},
    forge::{ForgeRepo, items::reference::ItemRef, items::review::NewReview},
    git,
    model::{Repository, Workspace},
    shoal_dirs::{Contents, ShoalDir},
    test_support,
};

/// The service over a stand-in daemon with one workspace.
pub(crate) struct Fixture {
    pub(crate) root: tempfile::TempDir,
    pub(crate) reviews: Arc<Reviews>,
    pub(crate) daemon: Arc<StandIn>,
    pub(crate) workspace: Workspace,
    pub(crate) repository: Repository,
    /// The repository's directory of review runs.
    pub(crate) runs: ShoalDir,
}

impl Fixture {
    /// A workspace on the branch `fix-login` in `root` under the global
    /// `config`.
    pub(crate) async fn new(root: tempfile::TempDir, config: &str) -> Self {
        let path = test_support::repository(root.path(), "repo");
        let workspaces_dir = root.path().join("workspaces");
        let worktree = workspaces_dir.join("fix-login");
        let worktree_arg = worktree.to_str().unwrap();
        test_support::git(
            &path,
            &["worktree", "add", "-q", "-b", "fix-login", worktree_arg],
        );
        let repository = repository(path, workspaces_dir);
        let workspace = workspace(worktree);
        let store = store(root.path().join("state.sqlite3"), "workspace").await;
        let records = Arc::new(StandInRecords {
            workspace: "workspace",
            name: "fix-login",
        });
        let home = root.path().join("home");
        let reviews = Reviews::new(store, Arc::new(Gates::default()), records, home);
        let daemon = StandIn {
            config: Mutex::new(test_support::global_config(config)),
            repository: repository.clone(),
            workspace: workspace.clone(),
            stops: Mutex::default(),
            recovered: watch::channel(false).0,
            held: Mutex::default(),
            messages: Mutex::default(),
            notices: Mutex::default(),
        };
        Self {
            root,
            reviews: Arc::new(reviews),
            daemon: Arc::new(daemon),
            workspace,
            runs: ShoalDir::of(&repository, Contents::ReviewRuns),
            repository,
        }
    }

    /// Run queued reviews in the background.
    pub(crate) fn run_reviews(&self) {
        let daemon: Arc<dyn crate::daemon::Daemon> = self.daemon.clone();
        tokio::spawn(crate::run(self.reviews.clone(), daemon));
    }

    /// Make `config` the global config later operations read.
    pub(crate) fn configure(&self, config: &str) {
        *self.daemon.config.lock().unwrap() = test_support::global_config(config);
    }

    pub(crate) fn selector(&self) -> SpaceSelector {
        SpaceSelector::id(&self.workspace.id)
    }
}

/// The registered repository at `path`, whose directory is `workspaces_dir`.
fn repository(path: PathBuf, workspaces_dir: PathBuf) -> Repository {
    Repository {
        id: "repo".into(),
        source: path.to_string_lossy().into_owned(),
        path,
        last_used: 1,
        name: None,
        workspaces_dir,
        contribution: false,
    }
}

/// A ready workspace on `fix-login` with its worktree at `path`.
fn workspace(path: PathBuf) -> Workspace {
    serde_json::from_value(serde_json::json!({
        "id": "workspace",
        "number": 1,
        "name": "fix-login",
        "repository_id": "repo",
        "workdir": {
            "kind": "worktree",
            "workspace": {"id": "workspace", "name": "fix-login", "branch": "fix-login"},
            "path": path,
        },
        "kind": "workspace",
        "path": path,
        "branch": "fix-login",
        "state": "ready",
        "error": null,
        "base_commit": null,
        "base_ref": "refs/heads/main",
        "git_dir": null,
    }))
    .unwrap()
}

/// A migrated store at `path` with the workspace `id` of the repository
/// `repo` recorded, which reviews belong to.
pub(crate) async fn store(path: PathBuf, id: &'static str) -> Store {
    let store = shoal_store::test_support::unsynced_store(path)
        .await
        .unwrap();
    store
        .write(move |tx| {
            tx.execute(
                "INSERT INTO repositories(id,path,source,last_used) VALUES ('repo','/repo','/repo',1)",
                [],
            )?;
            tx.execute(
                "INSERT INTO spaces(id,kind,repository_id,name) VALUES (?1,'workspace','repo',?1)",
                [id],
            )?;
            tx.execute(
                "INSERT INTO workspaces(id,path,branch,state,base_commit,git_dir) VALUES (?1,'/work',?1,'ready','abc','/admin')",
                [id],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    store
}

/// The rest of the daemon as the fixture's workspace sees it.
pub(crate) struct StandIn {
    config: Mutex<Config>,
    repository: Repository,
    workspace: Workspace,
    /// What takes back each admission a run holds, as protection does.
    pub(crate) stops: Mutex<Vec<Box<dyn FnOnce() + Send + Sync>>>,
    /// Whether load recovered, which admits the runs protection stopped.
    pub(crate) recovered: watch::Sender<bool>,
    /// The repositories of the admissions kept until a restart.
    pub(crate) held: Mutex<Vec<RepositoryId>>,
    /// The messages queued for the workspace's agents.
    pub(crate) messages: Mutex<Vec<String>>,
    /// What its coordinator was told.
    pub(crate) notices: Mutex<Vec<String>>,
}

impl StandIn {
    fn settings_now(&self) -> Result<Effective> {
        let config = self.config.lock().unwrap();
        config.resolve(&ConfigLayers::default())
    }
}

impl Spaces for StandIn {
    fn workspace<'a>(&'a self, selector: &'a SpaceSelector) -> BoxFuture<'a, Result<Workspace>> {
        Box::pin(async move {
            let selected = *selector == SpaceSelector::id(&self.workspace.id);
            selected
                .then(|| self.workspace.clone())
                .with_context(|| format!("no workspace {selector}"))
        })
    }

    fn workspace_by_id<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<Workspace>> {
        Box::pin(async move { Spaces::workspace(self, &SpaceSelector::id(id)).await })
    }

    fn verify_worktree<'a>(&'a self, _workspace: &'a Workspace) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn head<'a>(&'a self, workspace: &'a Workspace) -> BoxFuture<'a, Result<CommitId>> {
        Box::pin(revision(&workspace.path, &["rev-parse", "HEAD"]))
    }

    fn diff_base<'a>(&'a self, workspace: &'a Workspace) -> BoxFuture<'a, Result<CommitId>> {
        let base = ["merge-base", "main", "HEAD"];
        Box::pin(async move { revision(&workspace.path, &base).await })
    }
}

/// The commit Git prints for `args` in `path`.
async fn revision(path: &Path, args: &[&str]) -> Result<CommitId> {
    Ok(git::run(path, args).await?.trim().into())
}

impl Repositories for StandIn {
    fn repository<'a>(&'a self, id: &'a RepositoryId) -> BoxFuture<'a, Result<Repository>> {
        Box::pin(async move {
            (*id == self.repository.id)
                .then(|| self.repository.clone())
                .with_context(|| format!("no repository {id}"))
        })
    }

    fn select_repository(&self, _selector: Selector) -> BoxFuture<'_, Result<Repository>> {
        Box::pin(async { Ok(self.repository.clone()) })
    }

    fn repositories(&self) -> BoxFuture<'_, Result<Vec<Repository>>> {
        Box::pin(async { Ok(vec![self.repository.clone()]) })
    }

    fn false_positives_path<'a>(
        &'a self,
        _id: &'a RepositoryId,
    ) -> BoxFuture<'a, Result<Option<PathBuf>>> {
        let path = crate::config::repo::false_positives_path(&self.repository.workspaces_dir);
        Box::pin(async move { Ok(Some(path)) })
    }

    fn writable_false_positives_path<'a>(
        &'a self,
        _id: &'a RepositoryId,
    ) -> BoxFuture<'a, Result<PathBuf>> {
        let path = crate::config::repo::false_positives_path(&self.repository.workspaces_dir);
        Box::pin(async move { Ok(path) })
    }
}

impl Settings for StandIn {
    fn settings<'a>(&'a self, _workspace: &'a Workspace) -> BoxFuture<'a, Result<Effective>> {
        let settings = self.settings_now();
        Box::pin(async move { settings })
    }

    fn settings_with_templates<'a>(
        &'a self,
        _workspace: &'a Workspace,
    ) -> BoxFuture<'a, Result<Effective>> {
        let settings = self.settings_now();
        Box::pin(async move { settings })
    }

    fn repository_settings<'a>(
        &'a self,
        _id: &'a RepositoryId,
    ) -> BoxFuture<'a, Result<Effective>> {
        let settings = self.settings_now();
        Box::pin(async move { settings })
    }

    fn environment<'a>(
        &'a self,
        _workspace: &'a Workspace,
        _settings: &'a Effective,
    ) -> BoxFuture<'a, Result<BTreeMap<String, OsString>>> {
        Box::pin(async { Ok(BTreeMap::new()) })
    }
}

/// A run is admitted at once, or one protection stopped once load
/// recovered.
impl Agents for StandIn {
    fn admit<'a>(
        &'a self,
        _repository: &'a RepositoryId,
        _space: &'a SpaceId,
        after_stop: bool,
        stop: Box<dyn FnOnce() + Send + Sync>,
    ) -> BoxFuture<'a, Result<Box<dyn Admission + 'a>>> {
        self.stops.lock().unwrap().push(stop);
        let waits = after_stop.then(|| self.recovered.subscribe());
        Box::pin(async { Ok(Box::new(Admitted(waits)) as Box<dyn Admission>) })
    }

    fn hold_admission(&self, repository: &RepositoryId) {
        self.held.lock().unwrap().push(repository.clone());
    }

    fn message<'a>(&'a self, _id: &'a SpaceId, message: String) -> BoxFuture<'a, Result<()>> {
        self.messages.lock().unwrap().push(message);
        Box::pin(async { Ok(()) })
    }

    fn tell_coordinator<'a>(
        &'a self,
        _name: &'a SpaceName,
        _id: &'a SpaceId,
        message: String,
    ) -> BoxFuture<'a, ()> {
        self.notices.lock().unwrap().push(message);
        Box::pin(async {})
    }
}

/// An admission granted at once, or once load recovered.
struct Admitted(Option<watch::Receiver<bool>>);

impl Admission for Admitted {
    fn admitted(&mut self) -> BoxFuture<'_, ()> {
        Box::pin(async {
            if let Some(recovered) = &mut self.0 {
                let _ = recovered.wait_for(|recovered| *recovered).await;
            }
        })
    }

    fn keep(self: Box<Self>) {}
}

/// The workspace's repository has no forge.
impl Forge for StandIn {
    fn forge_account<'a>(
        &'a self,
        _workspace: &'a Workspace,
    ) -> BoxFuture<'a, Result<(ForgeRepo, Account)>> {
        Box::pin(async { bail!("the repository has no forge") })
    }

    fn post<'a>(
        &'a self,
        _workspace: &'a Workspace,
        _pull: &'a ItemRef,
        _review: NewReview,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { bail!("the repository has no forge") })
    }
}

/// The records of one workspace of the repository `repo`.
pub(crate) struct StandInRecords {
    pub(crate) workspace: &'static str,
    pub(crate) name: &'static str,
}

impl Records for StandInRecords {
    fn space_name(&self, _db: &Connection, id: &SpaceId) -> Result<Option<SpaceName>> {
        Ok((id.as_str() == self.workspace).then(|| self.name.into()))
    }

    fn workspaces(
        &self,
        _db: &Connection,
        repository: Option<&RepositoryId>,
    ) -> Result<Vec<SpaceId>> {
        let listed = repository.is_none_or(|repository| repository.as_str() == "repo");
        Ok(listed.then(|| self.workspace.into()).into_iter().collect())
    }
}
