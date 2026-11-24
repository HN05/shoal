//! A stand-in for the rest of the daemon, which the tests of reviews build
//! the service over: one workspace, a worktree of a real repository on its
//! own branch, recorded in a temporary store, under a global config a test
//! gives, whose one-shot runs the agent queue admits at once, except a run
//! protection stopped, which waits until a test says load recovered. The
//! approval requests the gate decides are kept in memory and change as the
//! daemon's records do; approving one records it as approved.
use shoal_daemon_base::forge_access::ForgeAccess;
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, bail};
use futures_util::future::BoxFuture;
use rusqlite::{Connection, Transaction};
use shoal_daemon_base::{callers::Caller, gates::Gates, notifications::About};
use shoal_forge::transport::account::Account;
use shoal_ids::repository_ids::RepositoryId;
use shoal_ids::space_ids::SpaceId;
use shoal_ids::space_ids::SpaceNumber;
use shoal_protocol::alerts::AlertKind;
use shoal_protocol::approvals::{ApprovalRequest, DecisionStatus, GateStep};
use shoal_protocol::code::{CodeAt, CodeRef, CodeState};
use shoal_protocol::config::permissions::Permissions;
use shoal_protocol::item_numbers::ItemNumber;
use shoal_protocol::paths::Paths;
use shoal_protocol::repository::Selector;
use shoal_protocol::reviews::{ReviewId, Severity};
use shoal_protocol::scope_tokens::ScopeToken;
use shoal_protocol::space_names::{SpaceName, SpaceSelector};
use shoal_protocol::threads::{
    Finding, Holder, NewPost, Outcome, Post, Resolution, Sender, Side, Thread, ThreadKind,
    ThreadSpace,
};
use shoal_protocol::time::UnixTime;
use shoal_protocol::views::Viewed;
use shoal_refs::CommitId;
use shoal_store::Store;
use tokio::sync::watch;

use crate::{
    Reviews,
    config::{Config, Effective, repo::ConfigLayers},
    daemon::{
        Admission, Agents, Approvals, AwaitedPost, FindingCode, FindingThread, Forge, Records,
        Repositories, Requesters, Requests, Settings, Spaces, Threads,
    },
    forge::{ForgeRepo, items::reference::ItemRef, items::review::NewReview},
    git,
    model::{Repository, Space, Workspace},
    shoal_dirs::{self, Contents, ShoalDir},
    test_support,
};

/// The service over a stand-in daemon with one workspace.
pub(crate) struct Fixture {
    pub(crate) root: tempfile::TempDir,
    pub(crate) reviews: Arc<Reviews>,
    pub(crate) records: Arc<StandInRecords>,
    pub(crate) daemon: Arc<StandIn>,
    pub(crate) requests: Arc<StandInRequests>,
    pub(crate) workspace: Workspace,
    pub(crate) repository: Repository,
    /// The repository's directory of review runs.
    pub(crate) runs: ShoalDir,
}

impl Drop for Fixture {
    /// Delete the root with the read-only checkouts of the reviews still
    /// running, which the temporary directory cannot delete on its own.
    fn drop(&mut self) {
        let _ = shoal_dirs::remove_dir_opening(self.root.path());
    }
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
            threads: Mutex::default(),
        });
        let state = root.path().join("state");
        let paths = Paths {
            home: root.path().join("home"),
            socket: state.join("daemon.sock"),
            state,
        };
        let requests = Arc::new(StandInRequests::default());
        let gates = Arc::new(Gates::default());
        let global = test_support::global_config(config);
        let global = Arc::new(shoal_daemon_base::config_state::ConfigState::new(global));
        let forges = Arc::new(ForgeAccess::new(store.clone(), gates.clone(), global));
        let reviews = Reviews::new(
            store,
            gates,
            forges,
            records.clone(),
            requests.clone(),
            paths,
        );
        let daemon = StandIn {
            requests: requests.clone(),
            notified: Mutex::default(),
            announced: Mutex::default(),
            approve_fails: AtomicBool::new(false),
            space_gone: AtomicBool::new(false),
            scopes: Mutex::default(),
            config: Mutex::new(test_support::global_config(config)),
            repository: repository.clone(),
            workspace: workspace.clone(),
            stops: Mutex::default(),
            recovered: watch::channel(false).0,
            admissions: watch::channel(Vec::new()).0,
            held: Mutex::default(),
            messages: Mutex::default(),
            notices: Mutex::default(),
            records: records.clone(),
            answered: watch::channel(Vec::new()).0,
            answer_fails: AtomicBool::new(false),
        };
        Self {
            root,
            records,
            reviews: Arc::new(reviews),
            daemon: Arc::new(daemon),
            requests,
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

    /// Run the gate on the requests it decides in the background.
    pub(crate) fn run_gate(&self) {
        let daemon: Arc<dyn crate::daemon::GateDaemon> = self.daemon.clone();
        tokio::spawn(crate::gate::run(self.reviews.clone(), daemon));
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
        shared_url: None,
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
        "checkout": {"state": "created", "base": {"commit": "abc", "reference": "refs/heads/main"},
                     "git_dir": "/admin"},
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
    requests: Arc<StandInRequests>,
    /// What the user was told, by kind.
    pub(crate) notified: Mutex<Vec<(AlertKind, String)>>,
    /// The keys of the requests announced to whoever decides them.
    pub(crate) announced: Mutex<Vec<String>>,
    /// Whether approving a request fails, as when what it does cannot run.
    pub(crate) approve_fails: AtomicBool,
    /// Whether the workspace is gone, as a request that outlives it finds.
    pub(crate) space_gone: AtomicBool,
    /// The scope tokens issued, each with whether it is still live.
    pub(crate) scopes: Mutex<Vec<(ScopeToken, Caller, bool)>>,
    config: Mutex<Config>,
    repository: Repository,
    workspace: Workspace,
    /// What takes back each admission a run holds, as protection does.
    pub(crate) stops: Mutex<Vec<Box<dyn FnOnce() + Send + Sync>>>,
    /// Whether load recovered, which admits the runs protection stopped.
    pub(crate) recovered: watch::Sender<bool>,
    /// Whether each admission asked for waited for healthy readings, in
    /// the order they were asked for.
    pub(crate) admissions: watch::Sender<Vec<bool>>,
    /// The repositories of the admissions kept until a restart.
    pub(crate) held: Mutex<Vec<RepositoryId>>,
    /// The messages queued for the workspace's agents.
    pub(crate) messages: Mutex<Vec<String>>,
    /// What its coordinator was told.
    pub(crate) notices: Mutex<Vec<String>>,
    /// The records the reviewer's answers are posted in.
    records: Arc<StandInRecords>,
    /// The answers posted, by thread record.
    pub(crate) answered: watch::Sender<Vec<(i64, String)>>,
    /// Whether posting an answer fails, as in a thread too long for it.
    pub(crate) answer_fails: AtomicBool,
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

    fn diff_base<'a>(&'a self, _id: &'a SpaceId) -> BoxFuture<'a, Result<CommitId>> {
        let base = ["merge-base", "main", "HEAD"];
        Box::pin(async move { revision(&self.workspace.path, &base).await })
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
        self.admissions
            .send_modify(|admissions| admissions.push(after_stop));
        let waits = after_stop.then(|| self.recovered.subscribe());
        Box::pin(async { Ok(Box::new(Admitted(waits)) as Box<dyn Admission>) })
    }

    fn hold_admission(&self, repository: &RepositoryId) {
        self.held.lock().unwrap().push(repository.clone());
    }

    fn message<'a>(
        &'a self,
        _id: &'a SpaceId,
        _kind: AlertKind,
        message: String,
    ) -> BoxFuture<'a, Result<()>> {
        self.messages.lock().unwrap().push(message);
        Box::pin(async { Ok(()) })
    }

    fn tell_coordinator<'a>(
        &'a self,
        _name: &'a SpaceName,
        _id: &'a SpaceId,
        _kind: AlertKind,
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

/// The records of one workspace of the repository `repo`, numbered 1, and
/// of the threads its reviews open, kept in memory.
pub(crate) struct StandInRecords {
    pub(crate) workspace: &'static str,
    pub(crate) name: &'static str,
    pub(crate) threads: Mutex<Vec<Thread>>,
}

impl StandInRecords {
    /// Resolve the thread `number` with `outcome`, or reopen it without one,
    /// as its holder.
    pub(crate) fn resolve(&self, number: ItemNumber, outcome: Option<Outcome>) {
        let mut threads = self.threads.lock().unwrap();
        let thread = (threads.iter_mut())
            .find(|thread| thread.number == number)
            .unwrap();
        thread.resolution = outcome.map(|outcome| Resolution {
            outcome,
            by: Sender::Space,
            by_space: Some(self.workspace.into()),
            resolved_at: UnixTime::from_seconds(1),
        });
    }

    /// Post `text` in the thread `number` from its holder's side, as its
    /// workspace's agent does.
    pub(crate) fn post_as_holder(&self, number: ItemNumber, text: &str) {
        let mut threads = self.threads.lock().unwrap();
        let thread = (threads.iter_mut())
            .find(|thread| thread.number == number)
            .unwrap();
        let id = thread.id * 100 + i64::try_from(thread.posts.len()).unwrap();
        let mut post = Self::shoal_post(id, text, Vec::new());
        (post.side, post.sender) = (Side::Holder, Sender::Space);
        thread.posts.push(post);
    }

    /// Open the user's comment `name` of the pending review `review`,
    /// whose posts are held; its number.
    pub(crate) fn comment(&self, review: ReviewId, name: &str) -> ItemNumber {
        let mut threads = self.threads.lock().unwrap();
        let item = u32::try_from(threads.len()).unwrap() + 1;
        let id = i64::from(item);
        let mut post = Self::shoal_post(id, name, Vec::new());
        post.sender = Sender::User;
        let number = ItemNumber {
            space: SpaceNumber::new(1),
            item,
        };
        threads.push(Thread {
            id,
            number,
            space_id: self.workspace.into(),
            space: self.name.into(),
            kind: ThreadKind::Review,
            finding: Some(Finding {
                severity: None,
                review: Some(review),
            }),
            name: name.to_owned(),
            opener: Sender::User,
            opener_space: None,
            holder: Holder::Space(ThreadSpace::here(self.workspace.into())),
            created_at: UnixTime::from_seconds(id),
            posts: vec![post],
            resolution: None,
            working: false,
            held: true,
            changes: Vec::new(),
        });
        number
    }

    /// A post from Shoal saying `text`, pointing at `code`.
    fn shoal_post(id: i64, text: &str, code: Vec<CodeRef>) -> Post {
        Post {
            id,
            created_at: UnixTime::from_seconds(id),
            side: Side::Opener,
            sender: Sender::Shoal,
            space: None,
            post: NewPost {
                text: text.to_owned(),
                ..NewPost::default()
            },
            files: Vec::new(),
            code,
            changes: Vec::new(),
            in_chat: false,
            forwarded_to: None,
            alert_id: None,
        }
    }
}

impl Records for StandInRecords {
    /// The workspace's name, or the name of the space `parent` above it.
    fn space_name(&self, _db: &Connection, id: &SpaceId) -> Result<Option<SpaceName>> {
        Ok(match id.as_str() {
            id if id == self.workspace => Some(self.name.into()),
            "parent" => Some("coordinator".into()),
            _ => None,
        })
    }

    fn thread_space(&self, db: &Connection, id: &SpaceId) -> Result<ThreadSpace> {
        Ok(ThreadSpace {
            name: self.space_name(db, id)?,
            ..ThreadSpace::here(id.clone())
        })
    }

    fn workspaces(
        &self,
        _db: &Connection,
        repository: Option<&RepositoryId>,
    ) -> Result<Vec<SpaceId>> {
        let listed = repository.is_none_or(|repository| repository.as_str() == "repo");
        Ok(listed.then(|| self.workspace.into()).into_iter().collect())
    }

    fn open_finding(&self, tx: &Transaction<'_>, finding: &FindingThread<'_>) -> Result<()> {
        let mut threads = self.threads.lock().unwrap();
        let item = u32::try_from(threads.len()).unwrap() + 1;
        let thread = finding_thread(item, self.name, finding);
        // Its row, which what refers to a thread names.
        tx.execute(
            "INSERT INTO threads(id,space_id,number,kind,name,created_at,opener)
                VALUES (?1,?2,?3,'review',?4,0,'shoal')",
            rusqlite::params![thread.id, thread.space_id, item, thread.name],
        )?;
        threads.push(thread);
        Ok(())
    }

    fn reopen_finding(
        &self,
        _tx: &Transaction<'_>,
        workspace: &SpaceId,
        number: ItemNumber,
        why: &str,
    ) -> Result<Option<i64>> {
        let mut threads = self.threads.lock().unwrap();
        let Some(thread) = threads.iter_mut().find(|thread| thread.number == number) else {
            return Ok(None);
        };
        let fixed = (thread.resolution.as_ref())
            .is_some_and(|resolution| resolution.outcome == Outcome::Fixed);
        if !fixed || thread.space_id != *workspace {
            return Ok(None);
        }
        thread.resolution = None;
        let post = Self::shoal_post(thread.id * 100, why, Vec::new());
        thread.posts.push(post);
        Ok(Some(thread.id))
    }

    fn drop_review_threads(&self, _tx: &Transaction<'_>, review: ReviewId) -> Result<()> {
        let mut threads = self.threads.lock().unwrap();
        threads.retain(|thread| thread.finding.and_then(|finding| finding.review) != Some(review));
        Ok(())
    }

    fn release_review_threads(&self, _tx: &Transaction<'_>, review: ReviewId) -> Result<()> {
        let mut threads = self.threads.lock().unwrap();
        let of_review = (threads.iter_mut())
            .filter(|thread| thread.finding.and_then(|finding| finding.review) == Some(review));
        of_review.for_each(|thread| thread.held = false);
        Ok(())
    }

    fn review_threads(&self, _db: &Connection, review: ReviewId) -> Result<Vec<Thread>> {
        let threads = self.threads.lock().unwrap();
        Ok((threads.iter())
            .filter(|thread| thread.finding.and_then(|finding| finding.review) == Some(review))
            .cloned()
            .collect())
    }

    fn threads(&self, _db: &Connection, ids: &[i64]) -> Result<Vec<Thread>> {
        let threads = self.threads.lock().unwrap();
        Ok((ids.iter())
            .filter_map(|id| threads.iter().find(|thread| thread.id == *id).cloned())
            .collect())
    }

    fn awaiting_answers(&self, _db: &Connection) -> Result<Vec<AwaitedPost>> {
        let threads = self.threads.lock().unwrap();
        let waiting = threads
            .iter()
            .filter(|thread| thread.awaits() == Some(Side::Opener));
        Ok((waiting.filter_map(|thread| {
            let post = thread.last_post()?.id;
            Some(AwaitedPost {
                thread: thread.id,
                post,
            })
        }))
        .collect())
    }
}

/// The thread `1.item` the stand-in's records open for `finding` in the
/// workspace named `name`.
fn finding_thread(item: u32, name: &str, finding: &FindingThread<'_>) -> Thread {
    let id = i64::from(item);
    let space = ThreadSpace {
        id: finding.workspace.clone(),
        name: Some(name.into()),
        number: Some(SpaceNumber::new(1)),
    };
    let code = CodeRef {
        id,
        space: space.clone(),
        commit: Some(finding.code.commit.clone()),
        original: finding.code.at.clone(),
        snippet: finding.code.snippet.clone(),
        latest: finding.code.at.clone(),
        state: CodeState::Unchanged,
    };
    Thread {
        id,
        number: ItemNumber {
            space: SpaceNumber::new(1),
            item,
        },
        space_id: finding.workspace.clone(),
        space: name.into(),
        kind: ThreadKind::Review,
        finding: Some(Finding {
            severity: Some(finding.severity),
            review: Some(finding.review),
        }),
        name: finding.title.to_owned(),
        opener: Sender::Shoal,
        opener_space: None,
        holder: Holder::Space(space),
        created_at: UnixTime::from_seconds(id),
        posts: vec![StandInRecords::shoal_post(id, finding.body, vec![code])],
        resolution: None,
        working: false,
        held: false,
        changes: Vec::new(),
    }
}

/// The thread `1.item` of a finding of review 9 of the workspace `work`
/// with `severity` on line 7 of `path`, titled `Problem <item>`, saying
/// `body`.
pub(crate) fn finding(item: u32, path: &str, severity: Severity, body: &str) -> Thread {
    let code = FindingCode {
        commit: "0123456789abcdef".into(),
        at: CodeAt {
            path: path.into(),
            lines: Some(shoal_protocol::code::Lines { start: 6, end: 7 }),
            blob: None,
        },
        snippet: None,
    };
    let title = format!("Problem {item}");
    let finding = FindingThread {
        review: ReviewId(9),
        workspace: &"work".into(),
        severity,
        title: &title,
        body,
        code: &code,
    };
    finding_thread(item, "fix-login", &finding)
}

/// The reviewer's answers go into the stand-in's records.
impl Threads for StandIn {
    fn review_threads_changed<'a>(&'a self, _workspace: &'a Workspace) -> BoxFuture<'a, ()> {
        Box::pin(async {})
    }

    fn answer_finding(&self, thread: i64, text: String) -> BoxFuture<'_, Result<()>> {
        if self.answer_fails.load(Ordering::SeqCst) {
            self.answered
                .send_modify(|answered| answered.push((thread, text)));
            return Box::pin(async { bail!("the thread is too long") });
        }
        let mut threads = self.records.threads.lock().unwrap();
        let answered = threads.iter_mut().find(|answered| answered.id == thread);
        if let Some(answered) = answered {
            let id = thread * 100 + i64::try_from(answered.posts.len()).unwrap();
            let post = StandInRecords::shoal_post(id, &text, Vec::new());
            answered.posts.push(post);
        }
        self.answered
            .send_modify(|answered| answered.push((thread, text)));
        Box::pin(async { Ok(()) })
    }
}

/// The workspace asks the gate, while it exists.
impl Requesters for StandIn {
    fn space<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<Space>> {
        Box::pin(async move {
            let gone = self.space_gone.load(Ordering::SeqCst);
            let found = (*id == self.workspace.id && !gone).then(|| self.workspace.clone().into());
            found.with_context(|| format!("no space {id}"))
        })
    }

    fn view_items<'a>(&'a self, _id: &'a SpaceId) -> BoxFuture<'a, Result<Viewed>> {
        Box::pin(async {
            Ok(Viewed {
                items: Vec::new(),
                views: Vec::new(),
            })
        })
    }

    fn permissions<'a>(
        &'a self,
        _id: &'a SpaceId,
        _repository: Option<&'a RepositoryId>,
    ) -> BoxFuture<'a, Result<Permissions>> {
        let permissions = (self.settings_now())
            .and_then(|settings| crate::config::permissions::resolve(&settings, None));
        Box::pin(async move { permissions })
    }

    fn tell_space<'a>(
        &'a self,
        _id: &'a SpaceId,
        _kind: AlertKind,
        message: String,
    ) -> BoxFuture<'a, bool> {
        self.messages.lock().unwrap().push(message);
        self.requests.changed();
        Box::pin(async { true })
    }

    fn notify(&self, _about: About, kind: AlertKind, message: String) -> BoxFuture<'_, ()> {
        self.notified.lock().unwrap().push((kind, message));
        Box::pin(async {})
    }

    fn issue_scope(&self, caller: &Caller) -> ScopeToken {
        let mut scopes = self.scopes.lock().unwrap();
        let token = ScopeToken::from(format!("scope-{}", scopes.len()));
        scopes.push((token.clone(), caller.clone(), true));
        token
    }

    fn revoke_scope(&self, token: &ScopeToken) {
        let mut scopes = self.scopes.lock().unwrap();
        for (issued, _, live) in scopes.iter_mut() {
            *live &= issued != token;
        }
    }
}

/// Approving a request records it as approved, unless approvals fail.
impl Approvals for StandIn {
    fn approve<'a>(&'a self, allowed: &'a ApprovalRequest) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            if self.approve_fails.load(Ordering::SeqCst) {
                bail!("the push cannot run");
            }
            (self.requests).change(&allowed.key, |request| {
                request.status = DecisionStatus::Approved;
                Ok(())
            })?;
            Ok(())
        })
    }

    fn announce<'a>(&'a self, request: &'a ApprovalRequest) -> BoxFuture<'a, ()> {
        self.announced.lock().unwrap().push(request.key.clone());
        self.requests.changed();
        Box::pin(async {})
    }

    fn prune_proposal_files<'a>(&'a self, _denied: &'a ApprovalRequest) -> BoxFuture<'a, ()> {
        Box::pin(async {})
    }
}

/// Approval requests in memory, in the order they were made.
pub(crate) struct StandInRequests {
    requests: Mutex<Vec<ApprovalRequest>>,
    /// Counts what changed a request or told someone of one.
    changes: watch::Sender<u64>,
}

impl Default for StandInRequests {
    fn default() -> Self {
        Self {
            requests: Mutex::default(),
            changes: watch::channel(0).0,
        }
    }
}

impl StandInRequests {
    /// Wait for the next change of a request, or a message or notice of
    /// one, after subscribing.
    pub(crate) fn changes(&self) -> watch::Receiver<u64> {
        self.changes.subscribe()
    }

    fn changed(&self) {
        self.changes.send_modify(|changes| *changes += 1);
    }

    /// Record `request` under a key and number of its own.
    pub(crate) fn add(&self, request: ApprovalRequest) -> ApprovalRequest {
        let mut requests = self.requests.lock().unwrap();
        let item = requests.len() as u32 + 1;
        let request = ApprovalRequest {
            key: format!("request-{item}"),
            number: ItemNumber {
                space: 1.into(),
                item,
            },
            ..request
        };
        requests.push(request.clone());
        request
    }

    /// The request stored under `key`.
    pub(crate) fn get(&self, key: &str) -> Option<ApprovalRequest> {
        let requests = self.requests.lock().unwrap();
        requests.iter().find(|request| request.key == key).cloned()
    }

    /// Apply `change` to the request stored under `key`.
    fn change(
        &self,
        key: &str,
        change: impl FnOnce(&mut ApprovalRequest) -> Result<()>,
    ) -> Result<ApprovalRequest> {
        let mut requests = self.requests.lock().unwrap();
        let request = (requests.iter_mut().find(|request| request.key == key))
            .context("the approval request no longer exists")?;
        change(request)?;
        let changed = request.clone();
        drop(requests);
        self.changed();
        Ok(changed)
    }

    /// Apply `change` to the request stored under `key` while the gate
    /// still decides it.
    fn with_gate(
        &self,
        key: &str,
        change: impl FnOnce(&mut ApprovalRequest),
    ) -> Result<ApprovalRequest> {
        self.change(key, |request| {
            anyhow::ensure!(
                request.status == DecisionStatus::Pending
                    && request.gate == Some(GateStep::Deciding),
                "approval request {} is no longer the gate's to decide",
                request.number
            );
            change(request);
            Ok(())
        })
    }

    fn pending_at(&self, step: GateStep) -> Vec<ApprovalRequest> {
        let requests = self.requests.lock().unwrap();
        (requests.iter())
            .filter(|request| request.status == DecisionStatus::Pending)
            .filter(|request| request.gate.as_ref() == Some(&step))
            .cloned()
            .collect()
    }
}

impl Requests for StandInRequests {
    fn request(&self, _db: &Connection, key: &str) -> Result<Option<ApprovalRequest>> {
        Ok(self.get(key))
    }

    fn give(&self, _tx: &Transaction<'_>, key: &str) -> Result<bool> {
        let given = self.change(key, |request| {
            if request.status == DecisionStatus::Pending && request.gate.is_none() {
                request.gate = Some(GateStep::Deciding);
            }
            Ok(())
        })?;
        Ok(given.gate == Some(GateStep::Deciding))
    }

    fn deciding(&self, _db: &Connection) -> Result<Vec<ApprovalRequest>> {
        Ok(self.pending_at(GateStep::Deciding))
    }

    fn stranded(&self, _db: &Connection) -> Result<Vec<ApprovalRequest>> {
        Ok(self.pending_at(GateStep::Allowed))
    }

    fn allow(&self, _tx: &Transaction<'_>, key: &str) -> Result<ApprovalRequest> {
        self.with_gate(key, |request| request.gate = Some(GateStep::Allowed))
    }

    fn deny(&self, _tx: &Transaction<'_>, key: &str) -> Result<ApprovalRequest> {
        self.with_gate(key, |request| request.status = DecisionStatus::Denied)
    }

    fn escalate(
        &self,
        _tx: &Transaction<'_>,
        key: &str,
        reason: &str,
        rule: &str,
    ) -> Result<ApprovalRequest> {
        let (reason, rule) = (reason.to_owned(), rule.to_owned());
        let escalated = GateStep::Escalated { reason, rule };
        self.with_gate(key, |request| request.gate = Some(escalated))
    }

    fn unjudged(&self, _tx: &Transaction<'_>, key: &str, reason: &str) -> Result<ApprovalRequest> {
        let reason = reason.to_owned();
        let unjudged = GateStep::Unjudged { reason };
        self.with_gate(key, |request| request.gate = Some(unjudged))
    }

    fn unapplied(
        &self,
        _tx: &Transaction<'_>,
        key: &str,
        failure: &str,
    ) -> Result<Option<ApprovalRequest>> {
        let error = failure.to_owned();
        let changed = self.change(key, |request| {
            if request.status == DecisionStatus::Pending {
                request.gate = Some(GateStep::Unapplied { error });
            }
            Ok(())
        })?;
        Ok((changed.status == DecisionStatus::Pending).then_some(changed))
    }
}
