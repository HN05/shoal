//! A stand-in for the rest of the daemon, which the spaces' tests build the
//! service over. It has no links, hub, CI jobs, reviews, leases or demos,
//! whose records read as empty.

use shoal_daemon_base::forge_access::ForgeAccess;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::ffi::OsString;
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context as _, Result, bail};
use futures_util::future::BoxFuture;
use rusqlite::{Connection, Transaction};
use shoal_coordination::Coordination;
use shoal_coordination::daemon::{Attaching, KeptFinding, PassedUp};
use shoal_daemon_base::alerts::AlertKind;
use shoal_daemon_base::blockers::Readiness;
use shoal_daemon_base::config_state::ConfigState;
use shoal_daemon_base::file_access::FileAccess;
use shoal_daemon_base::gates::Gates;
use shoal_daemon_base::notifications::{Notifications, Recorded};
use shoal_daemon_base::signals::Signals;
use shoal_daemon_base::stops::{StopPolicy, StopRequest};
use shoal_forge::transport::account::Account;
use shoal_forge_types::activity::status::PrStatus;
use shoal_forge_types::ci::run_action::Run;
use shoal_forge_types::items::action::Action;
use shoal_forge_types::items::issue::IssueDetails;
use shoal_forge_types::items::link::{LinkKind, Selection};
use shoal_forge_types::items::reference::{ItemInput, ItemRef};
use shoal_forge_types::{ForgeRepo, PullRequest};
use shoal_ids::execution_ids::ExecutionId;
use shoal_ids::member_ids::MemberId;
use shoal_ids::move_tickets::MoveTicket;
use shoal_ids::queue_entry_ids::QueueEntryId;
use shoal_ids::repository_ids::RepositoryId;
use shoal_ids::space_ids::{SpaceId, SpaceNumber};
use shoal_protocol::agent::Agent;
use shoal_protocol::approvals::{ApprovalRequest, ProposalDecision};
use shoal_protocol::cleanup::Emptying;
use shoal_protocol::config::permissions::Permissions;
use shoal_protocol::containers::ContainerProject;
use shoal_protocol::executions::{ProxyRetry, TurnRetry};
use shoal_protocol::files::{Attacher, FileSource, NewFile, SentFile};
use shoal_protocol::hub::directory::{HubView, Listed};
use shoal_protocol::hub::packages::KeptPackage;
use shoal_protocol::hub::placement::Decision;
use shoal_protocol::hub::{Machine, Member};
use shoal_protocol::links::{Unlinked, issue, pr};
use shoal_protocol::model::KeptLease;
use shoal_protocol::model::{
    Chat, CheckoutSync, Execution, LinkedItem, Located, MergePath, MergeWarning, QueueAdmission,
    QueueEntry, QueuedJob, Relation, Repository, ReviewMark, RunningAgent, Selected, Space,
    SpaceLinks, SpaceRef, SpaceTarget, StoppedSpace, SyncedRepository, Workspace, WorkspaceRef,
};
use shoal_protocol::paths::Paths;
use shoal_protocol::processes::Identity;
use shoal_protocol::progress::Blocker;
use shoal_protocol::protocol::operations::{LaunchRequest, QueuedLaunch};
use shoal_protocol::protocol::{
    Body, ConfigTarget, ItemContext, Operation, QueuedLaunches, RepoTarget,
};
use shoal_protocol::resources::ResourceLease;
use shoal_protocol::reviews::{CarriedReview, ReviewId};
use shoal_protocol::scope_tokens::ScopeToken;
use shoal_protocol::sessions::Origin;
use shoal_protocol::sim::Simulator;
use shoal_protocol::space_names::{SpaceName, SpaceSelector};
use shoal_protocol::state::BlockerKind;
use shoal_protocol::status::IssueStatus;
use shoal_protocol::threads::{Outcome, Thread};
use shoal_refs::CommitId;
use shoal_schedule::placement::snapshot::Request;
use shoal_store::test_support::seeds;
use shoal_store::tree::TrackedAgents;
use tokio::sync::{MutexGuard, OwnedMutexGuard};

use crate::Spaces;
use crate::alerts::NewAlert;
use crate::config::repo::ConfigLayers;
use crate::config::{Config, Effective};
use crate::daemon::*;
use crate::repository::Selector;
use crate::scope::Caller;
use crate::spaces::Removing;
use crate::store::Store;
use crate::target_checks::TargetCheck;
use shoal_core::launches::SpaceRepository;

pub(crate) use shoal_core::test_support::*;

/// Keeps Git from creating a linked worktree of a repository until it
/// drops: it makes the directory of the worktrees' admin directories
/// read-only, so a creation fails after its record exists.
pub(crate) struct BlockedWorktrees(PathBuf);

pub(crate) fn block_worktrees(checkout: &Path) -> BlockedWorktrees {
    use std::os::unix::fs::PermissionsExt;
    let directory = checkout.join(".git/worktrees");
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o555)).unwrap();
    BlockedWorktrees(directory)
}

impl Drop for BlockedWorktrees {
    fn drop(&mut self) {
        use std::os::unix::fs::PermissionsExt;
        let writable = std::fs::Permissions::from_mode(0o755);
        std::fs::set_permissions(&self.0, writable).unwrap();
    }
}

pub(crate) const NOT_HERE: &str = "the spaces' stand-in has no such subsystem";

pub(crate) struct Fixture {
    pub(crate) root: tempfile::TempDir,
    pub(crate) daemon: Arc<StandIn>,
}

impl Fixture {
    pub(crate) async fn new() -> Self {
        Self::with_config("[cleanup]\ngrace_minutes = 0\n").await
    }

    pub(crate) async fn with_config(text: &str) -> Self {
        let root = tempfile::tempdir_in("/tmp").unwrap();
        let paths = Paths::for_test(root.path());
        std::fs::create_dir_all(&paths.state).unwrap();
        shoal_core::test_support::write_global_config(&paths, text);
        let config = Arc::new(ConfigState::new(shoal_core::test_support::global_config(
            text,
        )));
        let daemon = Arc::new(StandIn::open(paths, config).await);
        Self { root, daemon }
    }

    /// The service a restarted daemon builds over the same state, knowing
    /// only what it recorded there.
    pub(crate) async fn reopen(self) -> Self {
        let (paths, config) = (self.daemon.paths.clone(), self.daemon.config.clone());
        self.daemon.store.shutdown().await;
        drop(self.daemon);
        let daemon = Arc::new(StandIn::open(paths, config).await);
        Self {
            root: self.root,
            daemon,
        }
    }

    /// Another daemon over the same state, as one that starts while this
    /// one still runs would read it.
    pub(crate) async fn open_again(&self) -> Arc<StandIn> {
        let (paths, config) = (self.daemon.paths.clone(), self.daemon.config.clone());
        Arc::new(StandIn::open(paths, config).await)
    }

    pub(crate) fn daemon(&self) -> Arc<dyn Daemon> {
        self.daemon.clone()
    }

    /// Checked out on `main` with one commit.
    pub(crate) async fn repository(&self, name: &str) -> Repository {
        let checkout = repository(self.root.path(), name);
        (self.register_repository(checkout.to_str().unwrap().into(), None, None))
            .await
            .unwrap()
    }
}

impl Deref for Fixture {
    type Target = StandIn;

    fn deref(&self) -> &StandIn {
        &self.daemon
    }
}

/// As editing the global config and reloading does.
pub(crate) async fn configure(fixture: &Fixture, text: &str) {
    shoal_core::test_support::write_global_config(&fixture.paths, text);
    let config = shoal_core::test_support::global_config(text);
    fixture.config.publish(config);
}

pub(crate) struct StandIn {
    pub(crate) spaces: Spaces,
    pub(crate) coordination: Coordination,
    pub(crate) store: Store,
    pub(crate) paths: Paths,
    pub(crate) config: Arc<ConfigState>,
    pub(crate) gates: Arc<Gates>,
    /// Held while an agent is admitted, which the stand-in never does.
    admissions: tokio::sync::Mutex<()>,
    /// In the order the removals closed them.
    pub(crate) closed_places: std::sync::Mutex<Vec<SpaceId>>,
    agents: Arc<RunningAgents>,
}

impl StandIn {
    async fn open(paths: Paths, config: Arc<ConfigState>) -> Self {
        let store = Store::open(paths.state.join("state.db")).await.unwrap();
        let gates = Arc::new(Gates::default());
        let notifications = Arc::new(Notifications::new(store.clone()));
        let signals = Arc::new(Signals::default());
        let removing = Removing::default();
        let agents = Arc::new(RunningAgents::default());
        let records = Arc::new(StandInRecords {
            coordination: CoordinationRecords(removing.clone()),
            agents: agents.clone(),
        });
        // Workers are fakes: no test here drives them.
        let operations =
            shoal_daemon_base::operations::Operations::new(paths.clone(), gates.clone());
        let operations = Arc::new(operations);
        operations.fake_workers();
        let coordination = Coordination::new(
            store.clone(),
            gates.clone(),
            config.clone(),
            notifications.clone(),
            signals.clone(),
            operations.clone(),
            Arc::new(CoordinationRecords(removing.clone())),
        );
        let spaces = Spaces::new(
            store.clone(),
            gates.clone(),
            config.clone(),
            notifications,
            signals,
            paths.clone(),
            Arc::new(FileAccess::default()),
            Arc::new(ForgeAccess::new(
                store.clone(),
                gates.clone(),
                config.clone(),
            )),
            operations,
            removing,
            records,
        );
        Self {
            spaces,
            coordination,
            store,
            paths,
            config,
            gates,
            admissions: Default::default(),
            closed_places: Default::default(),
            agents,
        }
    }

    /// Registers the checkout under its directory's name.
    async fn register(&self, path: &Path) -> Result<Repository> {
        let path = std::fs::canonicalize(path)?;
        let name = path.file_name().context("a named checkout")?;
        let repository = Repository {
            id: RepositoryId::generate(),
            path: path.clone(),
            source: path.display().to_string(),
            last_used: 1,
            name: Some(name.to_string_lossy().into_owned()),
            workspaces_dir: std::fs::canonicalize(&self.paths.home)?
                .join("shoal")
                .join(name),
            contribution: false,
            shared_url: None,
        };
        std::fs::create_dir_all(&repository.workspaces_dir)?;
        let recorded = repository.clone();
        (self.store)
            .write(move |tx| seeds::repository(tx, &recorded))
            .await?;
        Ok(repository)
    }

    pub(crate) async fn find_repository(
        &self,
        selector: impl Into<Selector>,
    ) -> Result<Repository> {
        let repositories = self.store.run(|db| repositories(db)).await?;
        Ok(crate::repository::select(&repositories, selector)
            .await?
            .clone())
    }

    async fn layers(
        &self,
        repository: Option<&RepositoryId>,
        worktree: Option<&Path>,
    ) -> Result<ConfigLayers> {
        let (worktree_file, worktree_template_files) = match worktree {
            Some(path) => crate::config::repo::load(path)?,
            None => Default::default(),
        };
        let (local_config, local_template_files) = match repository {
            Some(id) => {
                let repository = self.find_repository(Selector::from(id)).await?;
                crate::config::repo::load_local(&repository.workspaces_dir)?
            }
            None => Default::default(),
        };
        Ok(ConfigLayers {
            worktree_file,
            local_config,
            worktree_template_files,
            local_template_files,
            ..ConfigLayers::default()
        })
    }

    /// A workspace reads the file in its worktree; any other space reads the
    /// file in its repository's checkout.
    async fn space_layers(
        &self,
        space: &Space,
        repository: Option<&RepositoryId>,
    ) -> Result<ConfigLayers> {
        let repository = repository.or(space.repository_id.as_ref());
        let worktree = match space.worktree() {
            Some(worktree) => Some(worktree.path.clone()),
            None => match repository {
                Some(id) => Some(self.find_repository(Selector::from(id)).await?.path),
                None => None,
            },
        };
        self.layers(repository, worktree.as_deref()).await
    }
}

impl shoal_daemon_base::notifications::Coordinators for StandIn {
    fn tell<'a>(
        &'a self,
        alert: NewAlert,
        for_user: bool,
        source: shoal_daemon_base::notifications::Source,
    ) -> BoxFuture<'a, Option<shoal_daemon_base::alerts::Sent>> {
        Box::pin(
            self.coordination
                .tell_coordinator(self, alert, for_user, source),
        )
    }
}

impl StandIn {
    /// Selects by ID, so it reads a removed space's log too.
    pub(crate) async fn space_log(
        &self,
        selector: &SpaceSelector,
        filter: shoal_protocol::log::LogFilter,
        limit: u32,
    ) -> Result<Vec<shoal_protocol::log::LogEntry>> {
        let id = match selector {
            SpaceSelector::Id(id) => id.clone(),
            other => self.spaces.space(other).await?.id,
        };
        crate::events::space_log::read(&self.store, id, filter, limit).await
    }

    /// Opens a new question to the space's coordinator, or else the user.
    pub(crate) async fn ask(&self, id: &SpaceId, text: &str) -> Thread {
        use shoal_protocol::threads::{NewPost, ThreadKind, ThreadNumber, ThreadOpening};
        let caller = Caller::of_space(id.clone());
        let opening = ThreadOpening::named(text);
        let thread = (self.coordination).open_thread(
            self,
            Some(&caller),
            opening,
            Some(ThreadKind::Question),
            None,
        );
        let thread = thread.await.unwrap();
        let post = NewPost {
            text: text.into(),
            ..NewPost::default()
        };
        let number = ThreadNumber::Full(thread.number);
        (self.coordination.post_to_thread(
            self,
            Some(&caller),
            number,
            post,
            Vec::new(),
            Vec::new(),
            Vec::new(),
        ))
        .await
        .unwrap()
    }

    /// The user replies as the thread's holder.
    pub(crate) async fn answer(
        &self,
        number: shoal_protocol::item_numbers::ItemNumber,
        text: &str,
    ) -> Thread {
        use shoal_protocol::threads::{NewPost, ThreadNumber};
        let post = NewPost {
            text: text.into(),
            ..NewPost::default()
        };
        (self.coordination.post_to_thread(
            self,
            None,
            ThreadNumber::Full(number),
            post,
            Vec::new(),
            Vec::new(),
            Vec::new(),
        ))
        .await
        .unwrap()
    }

    /// One pass of the daemon's cleanup sweep: the spaces' own steps, in the
    /// order the daemon's `cleanup::sweep` runs them. A change there repeats
    /// here.
    pub(crate) async fn sweep(
        &self,
        timers: &mut crate::cleanup::Timers,
    ) -> Result<(), shoal_daemon_base::failures::SweepFailures> {
        use crate::cleanup::{kept_branches, merged, observe_branches, sweep_idle};
        let spaces = &self.spaces;
        let mut failed = shoal_daemon_base::failures::SweepFailures::default();
        let mut step = |name: &str, result: Result<()>| failed.step(name, result);
        step(
            "ownership recheck",
            spaces.recheck_startup_failures(self).await,
        );
        step("grace periods", spaces.begin_grace_pass().await);
        step("branch observation", observe_branches(self).await);
        step("completion cleanup", spaces.sweep_completed(self).await);
        let users = shoal_core::process::DirectoryUsers::default();
        step("idle cleanup", sweep_idle(self, timers, &users).await);
        step(
            "merged cleanup",
            merged::sweep(self, &mut timers.merged, &users).await,
        );
        step(
            "kept branches",
            kept_branches::sweep(self, &mut timers.kept_branches).await,
        );
        step("checkpoint expiry", spaces.expire_checkpoints(self).await);
        step("result expiry", spaces.expire_results(self).await);
        step("fetched copies", spaces.expire_fetched(self).await);
        step(
            "carried commits",
            spaces.release_unclaimed_carried(self).await,
        );
        step("temporary directories", spaces.sweep_tmp_dirs(self).await);
        step("leftovers", spaces.find_leftovers(self).await);
        step("grace periods", spaces.end_unclaimed_grace().await);
        failed.into_result()
    }

    /// Oldest first.
    pub(crate) async fn done_notices(&self) -> Vec<String> {
        let notifications = self.spaces.notifications.list(false, 100).await.unwrap();
        (notifications.into_iter())
            .filter(|notification| notification.kind == AlertKind::SpaceDone)
            .map(|notification| notification.message)
            .collect()
    }

    /// As `shoal repo add` does.
    pub(crate) async fn register_repository(
        &self,
        source: Selector,
        name: Option<String>,
        clone_path: Option<PathBuf>,
    ) -> Result<Repository> {
        Repositories::register_repository(self, source, name, clone_path).await
    }

    /// Deletes it when `text` is `None`.
    pub(crate) async fn write_local_config(
        &self,
        id: &RepositoryId,
        toml: Option<String>,
    ) -> Result<PathBuf> {
        let repository = self.find_repository(Selector::from(id)).await?;
        let path = crate::config::repo::local_config_path(&repository.workspaces_dir);
        match toml {
            Some(text) => {
                crate::config::repo::parse(&text)?;
                std::fs::create_dir_all(path.parent().unwrap())?;
                std::fs::write(&path, text)?;
            }
            None => match std::fs::remove_file(&path) {
                Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                    return Err(error.into());
                }
                _ => {}
            },
        }
        Ok(path)
    }

    /// Oldest first.
    pub(crate) async fn space_events(
        &self,
        after: Option<crate::events::EventCursor>,
        limit: u32,
    ) -> Result<Vec<crate::events::EventItem>> {
        let (items, _) = crate::events::page(&self.store, after, None, limit).await?;
        Ok(items)
    }

    pub(crate) async fn create_workspace(
        &self,
        repository: impl Into<Selector>,
        name: String,
        base: Option<String>,
        git_profile: Option<&str>,
        path: Option<PathBuf>,
    ) -> Result<Workspace> {
        let new = crate::workspace::NewWorkspace {
            git_profile,
            path,
            ..Default::default()
        };
        (self.spaces)
            .create_workspace_with(self, repository, name, base, new)
            .await
    }

    /// Records the chat as a start does before its agent runs.
    pub(crate) async fn chat(
        &self,
        name: &str,
        mode: impl Into<crate::config::modes::ModeName>,
        repository: Option<&Repository>,
        parent: Option<&SpaceId>,
    ) -> Chat {
        let new = crate::chats::NewChat {
            priority: None,
            name: name.into(),
            agent: "claude".into(),
            mode: mode.into(),
            narrowing: Default::default(),
            origin: None,
            parent: parent.cloned(),
        };
        let repository = repository.map(|repository| repository.id.clone().into());
        (self.spaces.start_new_chat(self, repository, None, new))
            .await
            .unwrap()
    }

    pub(crate) async fn run_agent(&self, id: &SpaceId) -> ExecutionId {
        self.run_named_agent(id, "claude").await
    }

    /// Records the agent as the execution the agents would record it as.
    /// The stand-in reports it as the space's agent until
    /// [`Self::end_agent`].
    pub(crate) async fn run_named_agent(&self, id: &SpaceId, agent: &str) -> ExecutionId {
        let execution = ExecutionId::generate();
        let mut run = shoal_protocol::test_support::execution(execution.as_str(), id.as_str());
        run.agent = Some(Agent::from(agent));
        run.mode = Some("worker".parse().unwrap());
        (self.store.write(move |tx| seeds::execution(tx, &run, None)))
            .await
            .unwrap();
        let ran = Ran {
            space: id.clone(),
            agent: agent.into(),
            state: None,
        };
        self.agents.0.lock().unwrap().insert(execution.clone(), ran);
        execution
    }

    /// Deletes the execution's record as the agent's end does.
    pub(crate) async fn end_agent(&self, execution: &ExecutionId) {
        self.agents.0.lock().unwrap().remove(execution);
        let recorded = execution.clone();
        (self.store.write(move |tx| {
            tx.execute("DELETE FROM executions WHERE id=?1", [recorded])?;
            Ok(())
        }))
        .await
        .unwrap();
    }

    /// As an agent's hooks report the session it continues.
    pub(crate) fn report_session(&self, id: &SpaceId, session: &str) {
        let [(_, execution)] = self.agents.executions(Some(id)).try_into().unwrap();
        let session = session.into();
        crate::execution::recovery::save_session(&self.paths, id, &execution, &session).unwrap();
    }

    /// As `shoal move -o` does. `file` must not exist, and `unsaved` is the
    /// work the move's end may delete.
    pub(crate) async fn move_to_file(
        &self,
        selector: &SpaceSelector,
        unsaved: crate::removal::UnsavedWork,
        file: PathBuf,
        carrying: &crate::transfer::Carrying,
    ) -> Result<(
        crate::transfer::ExportedSpace,
        crate::removal::RemovalResult,
    )> {
        let packing = crate::spaces::MovePacking {
            unsaved,
            destination: crate::transfer::MoveDestination::File { path: file },
            carrying: carrying.clone(),
            again: false,
        };
        let exported = match self
            .spaces
            .export_moving_space(self, selector, packing)
            .await
        {
            Ok(exported) => exported,
            Err(error) => {
                let _ = self.spaces.abandon_move(self, selector).await;
                return Err(error);
            }
        };
        let removal = self.spaces.end_moved_space(self, selector).await?;
        Ok((exported, removal))
    }

    /// As `shoal restore --from` does, into `repository` or the one with
    /// the package's origin.
    pub(crate) async fn import_file(
        &self,
        repository: Option<&Repository>,
        bytes: &[u8],
    ) -> Result<crate::transfer::ImportedPackage> {
        let directory = tempfile::tempdir()?;
        let file = directory.path().join("space.shoal");
        std::fs::write(&file, bytes)?;
        let import = crate::transfer::PackageImport {
            package: crate::transfer::PackageSource::File(file.clone()),
            repository: repository.map(|repository| (&repository.id).into()),
            sessions: None,
            claim: crate::transfer::Claim::Plain,
        };
        let input = crate::restore::packages::PackageInput::File(file);
        (self.spaces.import_package(self, &import, input, None)).await
    }

    pub(crate) fn report(&self, id: &SpaceId, state: shoal_protocol::state::AgentState) {
        let mut ran = self.agents.0.lock().unwrap();
        for ran in (ran.values_mut()).filter(|ran| ran.space == *id) {
            ran.state = Some(state);
        }
    }

    /// Records the chat as a start in the workspace does before its agent
    /// runs.
    pub(crate) async fn chat_in(&self, workspace: &SpaceSelector, name: &str) -> Result<Chat> {
        let new = crate::chats::NewChat {
            priority: None,
            name: name.into(),
            agent: "claude".into(),
            mode: crate::model::chat_mode(),
            narrowing: Default::default(),
            origin: None,
            parent: None,
        };
        (self.spaces.start_new_chat(self, None, Some(workspace), new)).await
    }

    /// As `shoal resume` does.
    pub(crate) async fn restore_workspace(
        &self,
        selector: &SpaceSelector,
    ) -> Result<shoal_protocol::model::RestoredWorkspace> {
        match self.spaces.restore_space(self, selector, None).await? {
            shoal_protocol::model::RestoredSpace::Workspace(restored) => Ok(*restored),
            shoal_protocol::model::RestoredSpace::Chat(_) => bail!("restored a chat"),
        }
    }

    /// Finds a workspace, then a chat, then a removed workspace by its ID,
    /// since a name never selects a space. A name no space has selects
    /// nothing.
    pub(crate) async fn named(&self, name: &str) -> SpaceSelector {
        let spaces = self.spaces.present_spaces(self).await.unwrap();
        let removed = self.spaces.removed_workspaces().await.unwrap();
        let id = (spaces.into_iter())
            .find(|space| space.name == name)
            .map(|space| space.id)
            .or_else(|| {
                (removed.into_iter())
                    .find(|removed| removed.name == name)
                    .map(|removed| removed.space_id)
            })
            .unwrap_or_else(SpaceId::generate);
        SpaceSelector::Id(id)
    }
}

fn repositories(db: &Connection) -> Result<Vec<Repository>> {
    Ok(db
        .prepare(&format!(
            "SELECT {} FROM repositories ORDER BY last_used DESC",
            shoal_store::REPOSITORY_COLUMNS
        ))?
        .query_map([], shoal_store::repository)?
        .collect::<rusqlite::Result<_>>()?)
}

/// Records nothing beside the spaces' own records but the removals the
/// service holds and the holds approvals place.
struct StandInRecords {
    coordination: CoordinationRecords,
    agents: Arc<RunningAgents>,
}

/// The agents a test runs, by the execution each runs as. The stand-in
/// reports them where the daemon reports the agents it tracks.
#[derive(Default)]
pub(crate) struct RunningAgents(std::sync::Mutex<BTreeMap<ExecutionId, Ran>>);

#[derive(Clone)]
struct Ran {
    space: SpaceId,
    agent: Agent,
    state: Option<shoal_protocol::state::AgentState>,
}

impl RunningAgents {
    fn executions(&self, space: Option<&SpaceId>) -> Vec<(SpaceId, ExecutionId)> {
        let ran = self.0.lock().unwrap();
        (ran.iter())
            .filter(|(_, ran)| space.is_none_or(|space| ran.space == *space))
            .map(|(execution, ran)| (ran.space.clone(), execution.clone()))
            .collect()
    }

    fn ran(&self, execution: &ExecutionId) -> Option<Ran> {
        self.0.lock().unwrap().get(execution).cloned()
    }

    fn state(&self, space: &SpaceId) -> Option<shoal_protocol::state::AgentState> {
        let ran = self.0.lock().unwrap();
        (ran.values()).find(|ran| ran.space == *space)?.state
    }
}

struct CoordinationRecords(Removing);

impl shoal_coordination::daemon::Records for CoordinationRecords {
    fn require_taking_work(&self, db: &Connection, id: &SpaceId) -> Result<()> {
        self.0.require_taking_work(db, id)
    }

    fn joined_review(
        &self,
        _: &Connection,
        _: &SpaceId,
        _: Option<&SpaceId>,
    ) -> Result<Option<ReviewId>> {
        Ok(None)
    }

    fn place_approved_hold(&self, tx: &Transaction<'_>, request: &ApprovalRequest) -> Result<()> {
        crate::holds::place_approved(tx, request, &self.0)
    }

    fn agent_turn(
        &self,
        _: &Connection,
        _: &SpaceId,
    ) -> Result<(Option<i64>, Option<ExecutionId>)> {
        Ok((None, None))
    }

    fn queue_alert_in(&self, _: &Transaction<'_>, _: &SpaceId, _: &NewAlert) -> Result<Recorded> {
        bail!("{NOT_HERE}: agents' alerts")
    }
}

impl Operations for StandIn {
    fn target_check(&self, _: &Operation) -> Option<TargetCheck> {
        Default::default()
    }

    fn failure(&self, _: &anyhow::Error) -> Body {
        unreachable!("{NOT_HERE}: failure")
    }
}

impl Links for StandIn {
    fn current_head<'a>(&'a self, workspace: &'a Workspace) -> BoxFuture<'a, Result<CommitId>> {
        Box::pin(async move { Ok(crate::git::head_and_branch(&workspace.path).await?.0) })
    }

    fn acting_repository<'a>(&'a self, space: &'a Space) -> BoxFuture<'a, Result<RepositoryId>> {
        let repository = space.repository_id.clone();
        Box::pin(async { repository.context("a chat without a repository acts in none here") })
    }

    fn origin_path<'a>(&'a self, space: &'a Space) -> BoxFuture<'a, Result<PathBuf>> {
        Box::pin(async move {
            match (space.worktree(), &space.repository_id) {
                (Some(worktree), _) => Ok(worktree.path.clone()),
                (None, Some(id)) => Ok(self.find_repository(Selector::from(id)).await?.path),
                (None, None) => bail!("{NOT_HERE}: a chat without a repository"),
            }
        })
    }

    fn read_pull_title<'a>(
        &'a self,
        _: &'a Path,
        _: &'a ItemInput,
    ) -> BoxFuture<'a, Result<String>> {
        Box::pin(async { Ok(Default::default()) })
    }

    fn branch_merge_warning<'a>(
        &'a self,
        _: &'a SpaceSelector,
    ) -> BoxFuture<'a, Result<Option<MergeWarning>>> {
        Box::pin(async { Ok(Default::default()) })
    }

    fn space_forge<'a>(&'a self, _: &'a Space) -> BoxFuture<'a, Result<ForgeRepo>> {
        Box::pin(async { bail!("{NOT_HERE}: space_forge") })
    }

    fn linked_states<'a>(
        &'a self,
        _: &'a Result<Option<String>>,
        _: Option<&'a pr::Registration>,
        _: &'a [issue::Registration],
    ) -> BoxFuture<'a, (Vec<PrStatus>, Vec<IssueStatus>)> {
        Box::pin(async { Default::default() })
    }

    fn acting_settings<'a>(&'a self, space: &'a Space) -> BoxFuture<'a, Result<Effective>> {
        Box::pin(async move {
            self.config
                .current()
                .resolve(&self.space_layers(space, None).await?)
        })
    }

    fn settle_agents(&self, _: &SpaceId) {}

    fn apply_own<'a>(
        &'a self,
        _: &'a SpaceId,
        _: &'a ForgeRepo,
        _: &'a Account,
        _: (shoal_forge_types::items::link::ItemKind, u64, String),
        _: &'a Action,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn check_conventions<'a>(
        &'a self,
        _: &'a Space,
        _: &'a Effective,
        _: &'a mut [ReviewMark],
    ) -> BoxFuture<'a, ()> {
        Box::pin(async {})
    }

    fn explicit_item<'a>(
        &'a self,
        _: &'a Path,
        _: &'a Selection,
    ) -> BoxFuture<'a, Result<Option<ItemRef>>> {
        Box::pin(async { Ok(Default::default()) })
    }

    fn follow_pushed_merge(&self, _: PushedMerge) -> BoxFuture<'_, Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn hand_over_assignees<'a>(&'a self, _: &'a SpaceId) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn issue_blockers<'a>(
        &'a self,
        _: &'a ItemContext,
        _: &'a ItemInput,
        _: Option<&'a Caller>,
    ) -> BoxFuture<'a, Result<Vec<Blocker>>> {
        Box::pin(async { Ok(Default::default()) })
    }

    fn issue_details<'a>(
        &'a self,
        _: &'a ItemContext,
        _: &'a ItemInput,
        _: bool,
        _: Option<&'a Caller>,
    ) -> BoxFuture<'a, Result<IssueDetails>> {
        Box::pin(async { bail!("{NOT_HERE}: issue_details") })
    }

    fn issue_forge<'a>(
        &'a self,
        _: &'a Path,
        _: &'a ItemInput,
    ) -> BoxFuture<'a, Result<(ForgeRepo, ItemRef)>> {
        Box::pin(async { bail!("{NOT_HERE}: issue_forge") })
    }

    fn issue_registrations<'a>(
        &'a self,
        _: &'a SpaceId,
    ) -> BoxFuture<'a, Result<Vec<issue::Registration>>> {
        Box::pin(async { Ok(Default::default()) })
    }

    fn lock_change_gates<'a>(
        &'a self,
        id: &'a SpaceId,
        others: &'a [&'a SpaceId],
    ) -> BoxFuture<'a, Result<shoal_daemon_base::gates::PrGuard>> {
        Box::pin(async move {
            let owned = id.to_owned();
            let subtree = (self.store)
                .read(move |db| shoal_store::tree::subtree(db, &owned))
                .await?;
            let ids = subtree.iter().chain(others.iter().copied());
            Ok(self.gates.lock_prs(ids).await)
        })
    }

    fn awaits_merge_runs<'a>(&'a self, _: &'a SpaceId) -> BoxFuture<'a, Result<bool>> {
        Box::pin(async { Ok(Default::default()) })
    }

    fn merged_unseen<'a>(&'a self, _: &'a SpaceId) -> BoxFuture<'a, Result<bool>> {
        Box::pin(async { Ok(Default::default()) })
    }

    /// Always the local merge: no space here links a PR.
    fn merge_path<'a>(&'a self, _: &'a SpaceSelector) -> BoxFuture<'a, Result<MergePath>> {
        Box::pin(async { Ok(MergePath::Local) })
    }

    fn pr_registration<'a>(
        &'a self,
        _: &'a SpaceId,
    ) -> BoxFuture<'a, Result<Option<pr::Registration>>> {
        Box::pin(async { Ok(Default::default()) })
    }

    fn pull_request<'a>(
        &'a self,
        _: &'a ItemContext,
        _: &'a ItemInput,
        _: Option<&'a Caller>,
    ) -> BoxFuture<'a, Result<PullRequest>> {
        Box::pin(async { bail!("{NOT_HERE}: pull_request") })
    }

    fn launch_run<'a>(
        &'a self,
        _: &'a ItemContext,
        _: &'a ItemInput,
        _: Option<&'a Caller>,
    ) -> BoxFuture<'a, Result<Run>> {
        Box::pin(async { bail!("{NOT_HERE}: launch_run") })
    }

    fn link_related_run<'a>(
        &'a self,
        _: &'a SpaceSelector,
        _: &'a ItemRef,
        _: Option<&'a Caller>,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn read_open_issue<'a>(
        &'a self,
        _: &'a Path,
        _: &'a ItemInput,
    ) -> BoxFuture<'a, Result<OpenIssue>> {
        Box::pin(async { bail!("{NOT_HERE}: read_open_issue") })
    }

    fn release_assignees(&self, _: Vec<Dropped>) -> BoxFuture<'_, Vec<String>> {
        Box::pin(async { Default::default() })
    }

    fn relink_space<'a>(
        &'a self,
        _: &'a SpaceId,
        _: &'a SpaceLinks,
        _: Option<&'a Caller>,
    ) -> BoxFuture<'a, Vec<String>> {
        Box::pin(async { Default::default() })
    }

    fn restack<'a>(&'a self, _: &'a WorkspaceRef, _: &'a Restack<'_>) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn select_context_items<'a>(
        &'a self,
        _: &'a RepoTarget,
        _: &'a Selection,
        _: bool,
        _: Option<&'a Caller>,
    ) -> BoxFuture<'a, Result<Selected>> {
        Box::pin(async { bail!("{NOT_HERE}: select_context_items") })
    }

    fn selected_items<'a>(
        &'a self,
        _: &'a Space,
        _: &'a Selection,
    ) -> BoxFuture<'a, Result<Vec<ItemRef>>> {
        Box::pin(async { Ok(Default::default()) })
    }

    fn set_draft<'a>(
        &'a self,
        _: &'a Space,
        _: &'a [ItemRef],
        _: bool,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn set_issue<'a>(
        &'a self,
        _: &'a SpaceSelector,
        _: &'a ItemInput,
        _: Option<String>,
        _: Option<&'a Caller>,
    ) -> BoxFuture<'a, Result<Vec<String>>> {
        Box::pin(async { Ok(Default::default()) })
    }

    fn set_pr<'a>(&'a self, _: &'a SpaceSelector, _: pr::Action) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn settle_removed_agents<'a>(
        &'a self,
        _: &'a Space,
        _: &'a [ItemRef],
    ) -> BoxFuture<'a, Result<Vec<String>>> {
        Box::pin(async { Ok(Default::default()) })
    }

    fn unlink_items<'a>(
        &'a self,
        _: &'a SpaceSelector,
        _: Relation,
        _: Option<LinkKind>,
        _: Option<&'a ItemInput>,
    ) -> BoxFuture<'a, Result<Unlinked>> {
        Box::pin(async { Ok(Default::default()) })
    }
}

impl Agents for StandIn {
    fn agents_suspended(&self) -> bool {
        Default::default()
    }

    fn opening_readiness<'a>(&'a self, _: &'a SpaceId) -> BoxFuture<'a, Result<Option<Readiness>>> {
        Box::pin(async { Ok(Default::default()) })
    }

    fn cleanup_orphan_sessions(&self) -> BoxFuture<'_, ()> {
        Box::pin(async {})
    }

    fn queue_start_agent<'a>(
        &'a self,
        _: (&'a Space, bool),
        _: QueuedLaunch,
        _: (Option<ItemInput>, Vec<Blocker>),
    ) -> BoxFuture<'a, Result<QueueAdmission>> {
        Box::pin(async { bail!("{NOT_HERE}: queue_start_agent") })
    }

    fn queue_attach<'a>(
        &'a self,
        _: &'a QueueEntryId,
        _: &'a SpaceSelector,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn disk_holds_back_starts(&self) -> BoxFuture<'_, Result<bool>> {
        Box::pin(async { Ok(Default::default()) })
    }

    fn lock_agent_admissions(&self) -> BoxFuture<'_, MutexGuard<'_, ()>> {
        Box::pin(self.admissions.lock())
    }

    fn lock_connections(&self) -> BoxFuture<'_, Box<dyn Send + '_>> {
        Box::pin(async { Box::new(()) as Box<dyn Send> })
    }

    fn execution_connected<'a>(&'a self, _: &'a ExecutionId) -> BoxFuture<'a, bool> {
        Box::pin(async { Default::default() })
    }

    fn forget_workspace_executions<'a>(&'a self, _: &'a SpaceId) -> BoxFuture<'a, ()> {
        Box::pin(async {})
    }

    fn agent_turn<'a>(
        &'a self,
        _: &'a Space,
        _: Option<&'a ExecutionId>,
    ) -> BoxFuture<'a, Result<(ExecutionId, Agent)>> {
        Box::pin(async { bail!("{NOT_HERE}: agent_turn") })
    }

    fn agent_environments<'a>(
        &'a self,
        _: &'a RepositoryId,
    ) -> BoxFuture<'a, Result<Vec<BTreeMap<String, String>>>> {
        Box::pin(async { Ok(Default::default()) })
    }

    fn agent_wrappers(&self) -> BoxFuture<'_, Result<Vec<Identity>>> {
        Box::pin(async { Ok(Default::default()) })
    }

    fn annotate_stops<'a>(&'a self, _: &'a mut [Execution]) -> BoxFuture<'a, ()> {
        Box::pin(async {})
    }

    fn caller_agent<'a>(&'a self, caller: &'a Caller) -> BoxFuture<'a, Option<CallerAgent>> {
        Box::pin(async {
            let execution = caller.execution_id()?;
            let ran = self.agents.ran(execution)?;
            Some(CallerAgent {
                execution: execution.clone(),
                name: ran.agent,
                space_id: ran.space,
            })
        })
    }

    fn cancel_stopped_restores<'a>(&'a self, _: &'a SpaceId) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn chat_environment<'a>(
        &'a self,
        _: Chat,
    ) -> BoxFuture<'a, Result<BTreeMap<String, OsString>>> {
        Box::pin(async { Ok(Default::default()) })
    }

    fn check_blockers<'a>(&'a self, _: &'a Selector, _: Vec<Blocker>) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn cleared_locally<'a>(
        &'a self,
        _: Option<&'a RepositoryId>,
        _: BlockerKind,
        _: &'a str,
    ) -> BoxFuture<'a, Result<Option<String>>> {
        Box::pin(async { Ok(Default::default()) })
    }

    fn close_space_places<'a>(&'a self, id: &'a SpaceId, _: &'a SpaceName) -> BoxFuture<'a, ()> {
        self.closed_places.lock().unwrap().push(id.clone());
        Box::pin(async {})
    }

    fn ensure_may_replace<'a>(
        &'a self,
        _: &'a Caller,
        _: &'a mut SpaceSelector,
        _: bool,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn ensure_replaceable<'a>(&'a self, _: &'a Space) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn discard_replaced_agent<'a>(&'a self, _: &'a Space) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn ensure_takes_worker<'a>(&'a self, _: &'a Space) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn forget_execution<'a>(&'a self, _: &'a ExecutionId) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn forget_last_sessions(&self, _: &SpaceId) {}

    fn forget_reports(&self, _: &SpaceId) {}

    fn has_queued_launch<'a>(&'a self, _: &'a SpaceId) -> BoxFuture<'a, Result<bool>> {
        Box::pin(async { Ok(Default::default()) })
    }

    fn idle_restores<'a>(&'a self, _: &'a SpaceId) -> BoxFuture<'a, Result<HashSet<ExecutionId>>> {
        Box::pin(async { Ok(Default::default()) })
    }

    fn note_interaction<'a>(&'a self, _: &'a SpaceSelector) -> BoxFuture<'a, ()> {
        Box::pin(async {})
    }

    fn record_user_interaction<'a>(
        &'a self,
        _: Option<&'a Caller>,
        _: &'a SpaceSelector,
    ) -> BoxFuture<'a, ()> {
        Box::pin(async {})
    }

    fn owned_places(&self, _: Option<SpaceId>) -> BoxFuture<'_, Result<Vec<(SpaceId, Origin)>>> {
        Box::pin(async { Ok(Default::default()) })
    }

    fn proxy_retry(&self, _: &[Execution]) -> Option<ProxyRetry> {
        Default::default()
    }

    fn queue_agent<'a>(
        &'a self,
        _: &'a SpaceSelector,
        _: LaunchRequest,
        _: Vec<Blocker>,
        _: Option<ItemInput>,
    ) -> BoxFuture<'a, Result<QueueEntry>> {
        Box::pin(async { bail!("{NOT_HERE}: queue_agent") })
    }

    fn queue_agent_message_next_turn<'a>(
        &'a self,
        _: &'a SpaceId,
        _: AlertKind,
        _: String,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn running_agents(&self) -> BoxFuture<'_, Result<Vec<RunningAgent>>> {
        Box::pin(async { Ok(Default::default()) })
    }

    fn saved_executions<'a>(
        &'a self,
        _: &'a SpaceSelector,
    ) -> BoxFuture<'a, Result<Vec<ExecutionId>>> {
        Box::pin(async { Ok(Default::default()) })
    }

    fn start_stopped_restore<'a>(
        &'a self,
        _: &'a SpaceSelector,
        _: &'a ExecutionId,
        _: BTreeMap<String, String>,
    ) -> BoxFuture<'a, Result<QueueEntry>> {
        Box::pin(async { bail!("{NOT_HERE}: start_stopped_restore") })
    }

    fn stop_executions<'a>(
        &'a self,
        _: &'a SpaceId,
        _: StopPolicy,
        _: &'a StopRequest,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn stop_space<'a>(&'a self, _: &'a SpaceSelector, _: StopRequest) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn stop_space_agent_executions<'a>(
        &'a self,
        _: &'a SpaceId,
        _: &'a StopRequest,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn stop_space_manually<'a>(
        &'a self,
        _: &'a SpaceSelector,
        _: bool,
        _: QueuedLaunches,
    ) -> BoxFuture<'a, Result<StoppedSpace>> {
        Box::pin(async { Ok(Default::default()) })
    }

    fn take_chat_place<'a>(&'a self, _: &'a SpaceId, _: Origin) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn turn_ended<'a>(
        &'a self,
        _: &'a SpaceId,
        _: &'a (dyn Fn(&RunningAgent) -> bool + Send + Sync),
    ) -> BoxFuture<'a, Result<bool>> {
        unreachable!("{NOT_HERE}: turn_ended")
    }
}

impl Repositories for StandIn {
    fn apply_seed<'a>(
        &'a self,
        _: &'a Repository,
        _: &'a Workspace,
        _: &'a Effective,
    ) -> BoxFuture<'a, ()> {
        Box::pin(async {})
    }

    fn checkout_sync(&self, _: &RepositoryId) -> Option<CheckoutSync> {
        Default::default()
    }

    fn destroy_seed_outputs<'a>(&'a self, _: &'a Workspace) -> BoxFuture<'a, ()> {
        Box::pin(async {})
    }

    fn ensure_available<'a>(&'a self, _: &'a RepositoryId) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn fetch_default_branch<'a>(&'a self, _: &'a RepositoryId) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }

    /// The stand-in fetches nothing, so a checkout holds what its test put
    /// there.
    fn default_branch_fresh<'a>(&'a self, _: &'a RepositoryId) -> BoxFuture<'a, bool> {
        Box::pin(async { true })
    }

    fn empty_caches<'a>(
        &'a self,
        _: &'a Repository,
        _: bool,
    ) -> BoxFuture<'a, Result<Option<Emptying>>> {
        Box::pin(async { Ok(Default::default()) })
    }

    fn forget_rejected_config(&self, _: &SpaceId) {}

    fn lock_repository(
        &self,
        selector: Selector,
    ) -> BoxFuture<'_, Result<(Repository, OwnedMutexGuard<()>)>> {
        Box::pin(async move {
            let repository = self.find_repository(selector).await?;
            let guard = self.gates.lock_git(&repository.id).await;
            Ok((repository, guard))
        })
    }

    fn verified_seed_worktree<'a>(
        &'a self,
        _: &'a Repository,
        _: &'a Path,
    ) -> BoxFuture<'a, Option<PathBuf>> {
        Box::pin(async { Default::default() })
    }

    /// Creates the configured `root_dir`.
    fn prepare_chat_root(&self) -> BoxFuture<'_, Result<PathBuf>> {
        Box::pin(async {
            let root = self.config.current().root_dir(&self.paths)?;
            std::fs::create_dir_all(&root)?;
            Ok(root)
        })
    }

    fn register_repository(
        &self,
        source: Selector,
        _: Option<String>,
        _: Option<PathBuf>,
    ) -> BoxFuture<'_, Result<Repository>> {
        Box::pin(async move {
            match source {
                Selector::Path { path, .. } => self.register(&path).await,
                Selector::Value(value) if Path::new(&value).is_dir() => {
                    self.register(Path::new(&value)).await
                }
                other => self.find_repository(other).await,
            }
        })
    }

    fn repository_settings<'a>(&'a self, id: &'a RepositoryId) -> BoxFuture<'a, Result<Effective>> {
        Box::pin(async move {
            self.config
                .current()
                .resolve(&self.layers(Some(id), None).await?)
        })
    }

    fn repository_settings_with_templates<'a>(
        &'a self,
        id: &'a RepositoryId,
    ) -> BoxFuture<'a, Result<Effective>> {
        Box::pin(async move {
            let layers = self.layers(Some(id), None).await?;
            self.config
                .current()
                .resolve_with_template_files(&layers, &self.paths)
        })
    }

    fn space_settings<'a>(&'a self, space: &'a Space) -> BoxFuture<'a, Result<Effective>> {
        Box::pin(async move {
            self.config
                .current()
                .resolve(&self.space_layers(space, None).await?)
        })
    }

    fn space_settings_with_templates<'a>(
        &'a self,
        space: &'a Space,
        repository: Option<&'a RepositoryId>,
    ) -> BoxFuture<'a, Result<Effective>> {
        Box::pin(async move {
            let layers = self.space_layers(space, repository).await?;
            self.config
                .current()
                .resolve_with_template_files(&layers, &self.paths)
        })
    }

    fn sync_repository<'a>(&'a self, _: &'a Selector) -> BoxFuture<'a, Result<SyncedRepository>> {
        Box::pin(async { bail!("{NOT_HERE}: sync_repository") })
    }

    fn target_settings_with_templates(
        &self,
        target: ConfigTarget,
    ) -> BoxFuture<'_, Result<Effective>> {
        Box::pin(async move {
            let layers = match target {
                ConfigTarget::Global => ConfigLayers::default(),
                ConfigTarget::Space(selector) => {
                    let space = self.spaces.space(&selector).await?;
                    self.space_layers(&space, None).await?
                }
                ConfigTarget::Repository(selector) => {
                    let repository = self.find_repository(selector).await?;
                    self.layers(Some(&repository.id), Some(&repository.path))
                        .await?
                }
            };
            self.config
                .current()
                .resolve_with_template_files(&layers, &self.paths)
        })
    }

    fn tracked_config<'a>(&'a self, _: &'a Repository) -> BoxFuture<'a, Result<String>> {
        Box::pin(async { Ok(Default::default()) })
    }

    fn tracked_settings(&self, _: &str) -> Result<Effective> {
        bail!("{NOT_HERE}: tracked_settings")
    }

    fn workspace_layers<'a>(
        &'a self,
        workspace: &'a Workspace,
    ) -> BoxFuture<'a, Result<ConfigLayers>> {
        Box::pin(async move {
            self.layers(Some(workspace.repository()), Some(&workspace.path))
                .await
        })
    }

    fn workspace_settings<'a>(
        &'a self,
        workspace: &'a Workspace,
    ) -> BoxFuture<'a, Result<Effective>> {
        Box::pin(async move {
            let layers = self
                .layers(Some(workspace.repository()), Some(&workspace.path))
                .await?;
            self.config.current().resolve(&layers)
        })
    }

    fn workspace_settings_from<'a>(
        &'a self,
        config: &'a Config,
        workspace: &'a Workspace,
    ) -> BoxFuture<'a, Result<Effective>> {
        Box::pin(async move {
            config.resolve(
                &self
                    .layers(Some(workspace.repository()), Some(&workspace.path))
                    .await?,
            )
        })
    }

    fn workspace_settings_with_templates<'a>(
        &'a self,
        workspace: &'a Workspace,
    ) -> BoxFuture<'a, Result<Effective>> {
        Box::pin(async move {
            let layers = self
                .layers(Some(workspace.repository()), Some(&workspace.path))
                .await?;
            self.config
                .current()
                .resolve_with_template_files(&layers, &self.paths)
        })
    }
}

impl Hub for StandIn {
    fn deliver_file<'a>(
        &'a self,
        _: &'a Member,
        _: Operation,
        _: Option<&'a Caller>,
        _: &'a Path,
    ) -> BoxFuture<'a, Result<Body>> {
        Box::pin(async { bail!("{NOT_HERE}: deliver_file") })
    }

    fn claim_moved<'a>(
        &'a self,
        _: &'a SpaceId,
        _: &'a MoveTicket,
    ) -> BoxFuture<'a, Result<(), ClaimFailure>> {
        Box::pin(async { Ok(()) })
    }

    fn claim_space<'a>(&'a self, _: &'a SpaceId) -> BoxFuture<'a, ()> {
        Box::pin(async {})
    }

    fn claim_taken_over<'a>(&'a self, _: &'a SpaceId) -> BoxFuture<'a, Result<(), ClaimFailure>> {
        Box::pin(async { Ok(()) })
    }

    fn coordinator_elsewhere<'a>(&'a self, _: &'a SpaceId) -> BoxFuture<'a, bool> {
        Box::pin(async { Default::default() })
    }

    fn descendants_elsewhere<'a>(&'a self, _: &'a SpaceId) -> BoxFuture<'a, Result<Vec<SpaceRef>>> {
        Box::pin(async { Ok(Default::default()) })
    }

    fn ensure_parent_elsewhere<'a>(
        &'a self,
        _: Option<&'a SpaceId>,
        _: Option<&'a RepositoryId>,
        _: &'a SpaceId,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn fetch_package<'a>(&'a self, _: &'a SpaceId, _: &'a Path) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn hub_view<'a>(&'a self, _: Option<&'a Caller>) -> BoxFuture<'a, Result<Option<HubView>>> {
        Box::pin(async { Ok(Default::default()) })
    }

    fn kept_package<'a>(&'a self, _: &'a SpaceId) -> BoxFuture<'a, Result<Option<KeptPackage>>> {
        Box::pin(async { Ok(Default::default()) })
    }

    fn listed<'a>(&'a self, _: &'a SpaceId) -> BoxFuture<'a, ()> {
        Box::pin(async {})
    }

    fn machine_name(&self) -> BoxFuture<'_, String> {
        Box::pin(async { Default::default() })
    }

    fn other_machines(&self) -> BoxFuture<'_, Vec<Member>> {
        Box::pin(async { Default::default() })
    }

    fn waiting_elsewhere<'a>(&'a self, _: &'a SpaceId) -> BoxFuture<'a, usize> {
        Box::pin(async { Default::default() })
    }

    fn tell_machine<'a>(&'a self, _: &'a Member, _: Operation) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { bail!("{NOT_HERE}: tell_machine") })
    }

    fn machine_of<'a>(
        &'a self,
        _: &'a SpaceSelector,
    ) -> BoxFuture<'a, Result<Option<(Member, Listed)>>> {
        Box::pin(async { Ok(Default::default()) })
    }

    fn record_keeper<'a>(&'a self, _: &'a SpaceSelector) -> BoxFuture<'a, Result<Option<Member>>> {
        Box::pin(async { Ok(None) })
    }

    fn named_machine<'a>(&'a self, _: &'a str) -> BoxFuture<'a, Result<(Member, bool)>> {
        Box::pin(async { bail!("{NOT_HERE}: named_machine") })
    }

    fn joined_machines(&self) -> BoxFuture<'_, Result<Vec<Member>>> {
        Box::pin(async { bail!("{NOT_HERE}: joined_machines") })
    }

    fn files_stay_here(&self, _: &str) -> anyhow::Error {
        unreachable!("{NOT_HERE}: files_stay_here")
    }

    fn never_sent(&self, _: &anyhow::Error) -> bool {
        Default::default()
    }

    fn origin_url_elsewhere<'a>(&'a self, _: &'a SpaceId) -> BoxFuture<'a, Result<Option<String>>> {
        Box::pin(async { Ok(Default::default()) })
    }

    fn parent_elsewhere<'a>(&'a self, _: &'a SpaceId) -> BoxFuture<'a, Result<Option<SpaceRef>>> {
        Box::pin(async { Ok(Default::default()) })
    }

    fn place_start(&self, _: Request) -> BoxFuture<'_, Result<Option<Decision>>> {
        Box::pin(async { Ok(Default::default()) })
    }

    fn refuse_live_elsewhere<'a>(&'a self, _: &'a SpaceId) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn remove_children_elsewhere<'a>(
        &'a self,
        _: &'a SpaceId,
        _: BelowElsewhere<'a>,
    ) -> BoxFuture<'a, Result<Vec<SpaceName>>> {
        Box::pin(async { Ok(Default::default()) })
    }

    fn repository_elsewhere<'a>(
        &'a self,
        _: &'a SpaceId,
    ) -> BoxFuture<'a, Result<SpaceRepository>> {
        Box::pin(async { bail!("{NOT_HERE}: repository_elsewhere") })
    }

    fn settle_move<'a>(
        &'a self,
        _: &'a SpaceId,
        _: &'a MemberId,
        _: &'a MoveTicket,
    ) -> BoxFuture<'a, Result<bool>> {
        Box::pin(async { Ok(Default::default()) })
    }

    fn this_machine(&self) -> BoxFuture<'_, Result<Option<Machine>>> {
        Box::pin(async { Ok(Default::default()) })
    }
}

impl Runner for StandIn {
    fn attach_jobs<'a>(&'a self, _: &'a mut [Space]) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn cancel_job_before_stopping<'a>(
        &'a self,
        _: &'a SpaceId,
        _: shoal_daemon_base::jobs::JobStop,
    ) -> BoxFuture<'a, ()> {
        Box::pin(async {})
    }

    fn linked_job_workspaces<'a>(
        &'a self,
        _: &'a SpaceId,
    ) -> BoxFuture<'a, Result<Vec<Workspace>>> {
        Box::pin(async { Ok(Default::default()) })
    }

    fn linked_waiting_jobs<'a>(&'a self, _: &'a SpaceId) -> BoxFuture<'a, Result<Vec<QueueEntry>>> {
        Box::pin(async { Ok(Default::default()) })
    }

    fn workspace_job<'a>(
        &'a self,
        _: &'a SpaceId,
    ) -> BoxFuture<'a, Result<Option<Box<QueuedJob>>>> {
        Box::pin(async { Ok(Default::default()) })
    }
}

impl Reviews for StandIn {
    fn check_review<'a>(
        &'a self,
        _: &'a Workspace,
        _: ReviewGate,
        _: &'a CommitId,
        _: Option<&'a Caller>,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn require_review<'a>(
        &'a self,
        _: &'a Workspace,
        _: ReviewGate,
        _: &'a CommitId,
        _: Option<&'a Caller>,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn queue_unreviewed(&self, error: anyhow::Error) -> BoxFuture<'_, anyhow::Error> {
        Box::pin(async { error })
    }

    fn ensure_gate_hears<'a>(&'a self, _: &'a ApprovalRequest) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn pending_review<'a>(
        &'a self,
        _: &'a SpaceId,
    ) -> BoxFuture<'a, Result<Option<CarriedReview>>> {
        Box::pin(async { Ok(Default::default()) })
    }

    fn queue_carried_review<'a>(
        &'a self,
        _: &'a SpaceId,
        _: CarriedReview,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn stop_reviews<'a>(&'a self, _: &'a SpaceId) -> BoxFuture<'a, ()> {
        Box::pin(async {})
    }
}

impl Leases for StandIn {
    fn kept_leases<'a>(&'a self, _: &'a SpaceId) -> BoxFuture<'a, Result<Vec<KeptLease>>> {
        Box::pin(async { Ok(Vec::new()) })
    }

    fn container_project<'a>(
        &'a self,
        _: &'a SpaceId,
    ) -> BoxFuture<'a, Result<Option<ContainerProject>>> {
        Box::pin(async { Ok(Default::default()) })
    }

    fn list_resources<'a>(
        &'a self,
        _: Option<&'a SpaceSelector>,
    ) -> BoxFuture<'a, Result<Vec<ResourceLease>>> {
        Box::pin(async { Ok(Default::default()) })
    }

    fn list_simulators<'a>(
        &'a self,
        _: Option<&'a SpaceId>,
    ) -> BoxFuture<'a, Result<Vec<Simulator>>> {
        Box::pin(async { Ok(Default::default()) })
    }

    fn remove_containers<'a>(&'a self, _: &'a Workspace) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn remove_simulators<'a>(&'a self, _: &'a SpaceId) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn run_resource_release_hook<'a>(
        &'a self,
        _: &'a Workspace,
        _: &'a ResourceLease,
        _: Option<&'a Path>,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }
}

impl Demos for StandIn {
    fn stop_demos<'a>(
        &'a self,
        _: &'a SpaceSelector,
        _: Option<String>,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }
}

impl Records for StandInRecords {
    fn load_workspace(&self, _: &Connection, _: &mut Workspace) -> Result<()> {
        Ok(())
    }

    fn load_chat(&self, _: &Connection, _: &mut Chat) -> Result<()> {
        Ok(())
    }

    fn assigned_items(
        &self,
        _: &Connection,
        _: &SpaceId,
        _: Option<shoal_forge_types::items::link::ItemKind>,
    ) -> Result<Vec<ItemRef>> {
        Ok(Default::default())
    }

    fn recorded_items(&self, _: &Connection, _: &SpaceId, _: Relation) -> Result<Vec<LinkedItem>> {
        Ok(Default::default())
    }

    fn space_links(&self, _: &Connection, _: &SpaceId) -> Result<SpaceLinks> {
        Ok(Default::default())
    }

    fn agent_state(
        &self,
        _: &Connection,
        id: &SpaceId,
    ) -> Result<Option<shoal_protocol::state::AgentState>> {
        Ok(self.agents.state(id))
    }

    fn hand_alerts_to_user(&self, _: &Transaction<'_>, _: &SpaceId) -> Result<Option<i64>> {
        Ok(Default::default())
    }

    fn remove_issue_links(
        &self,
        _: &Transaction<'_>,
        _: &SpaceId,
    ) -> Result<(Vec<Dropped>, Vec<ItemRef>)> {
        Ok(Default::default())
    }

    fn drop_pending_reviews(&self, _: &Transaction<'_>, _: &SpaceId) -> Result<()> {
        Ok(())
    }

    fn cancel_wake(&self, _: &Transaction<'_>, _: &SpaceId) -> Result<()> {
        Ok(())
    }

    fn turn_retry(&self, _: &Connection, _: &SpaceId) -> Result<Option<TurnRetry>> {
        Ok(Default::default())
    }

    fn other_executions(
        &self,
        _: &Connection,
        space: Option<&SpaceId>,
        own: Option<&ExecutionId>,
    ) -> Result<Vec<(SpaceId, ExecutionId)>> {
        let mut executions = self.agents.executions(space);
        executions.retain(|(_, execution)| Some(execution) != own);
        Ok(executions)
    }

    fn running_agents(&self, _: &Connection, id: &SpaceId) -> Result<Vec<ExecutionId>> {
        let executions = self.agents.executions(Some(id));
        Ok(executions
            .into_iter()
            .map(|(_, execution)| execution)
            .collect())
    }

    fn queued_agent(&self, _: &Connection, _: &SpaceId) -> Result<Option<Agent>> {
        Ok(Default::default())
    }

    fn launch_waits_to_open(&self, _: &Connection, _: &SpaceId) -> Result<bool> {
        Ok(Default::default())
    }

    fn forget_executions(&self, _: &Transaction<'_>, _: &SpaceId) -> Result<()> {
        Ok(())
    }

    fn forget_command_executions(&self, _: &Transaction<'_>, _: &SpaceId) -> Result<()> {
        Ok(())
    }

    fn interrupt_operations(&self, _: &Transaction<'_>) -> Result<()> {
        Ok(())
    }

    fn repositories(&self, db: &Connection) -> Result<Vec<Repository>> {
        repositories(db)
    }

    fn touch_repository(&self, _: &Transaction<'_>, _: &RepositoryId) -> Result<()> {
        Ok(())
    }

    fn leasing_workspaces(&self, _: &Connection, _: &str) -> Result<Vec<SpaceId>> {
        Ok(Default::default())
    }

    fn next_space_number(&self, _: &Transaction<'_>, _: &SpaceId) -> Result<Option<SpaceNumber>> {
        Ok(Default::default())
    }

    fn ensure_holders_fit(&self, _: &Transaction<'_>, _: &[PathBuf]) -> Result<()> {
        Ok(())
    }

    fn coordination(&self) -> &dyn shoal_coordination::daemon::Records {
        &self.coordination
    }
}

impl Daemon for StandIn {
    fn spaces(&self) -> &Spaces {
        &self.spaces
    }

    fn coordination(&self) -> &Coordination {
        &self.coordination
    }
}

impl shoal_coordination::daemon::Spaces for StandIn {
    fn space<'a>(&'a self, selector: &'a SpaceSelector) -> BoxFuture<'a, Result<Space>> {
        Box::pin(self.spaces.space(selector))
    }

    fn space_by_id<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<Space>> {
        Box::pin(self.spaces.space_by_id(id))
    }

    fn find_space<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<Option<Space>>> {
        Box::pin(self.spaces.find_space(id))
    }

    fn space_anywhere<'a>(&'a self, selector: &'a SpaceSelector) -> BoxFuture<'a, Result<Located>> {
        Box::pin(self.spaces.space_anywhere(self, selector))
    }

    fn space_id_anywhere<'a>(
        &'a self,
        selector: &'a SpaceSelector,
    ) -> BoxFuture<'a, Result<SpaceId>> {
        Box::pin(self.spaces.space_id_anywhere(self, selector))
    }

    fn spaces_within<'a>(&'a self, root: Option<&'a SpaceId>) -> BoxFuture<'a, Result<Vec<Space>>> {
        Box::pin(self.spaces.spaces_within(self, root))
    }

    fn find_spaces<'a>(
        &'a self,
        target: &'a SpaceTarget,
        among: Vec<Space>,
    ) -> BoxFuture<'a, Result<Vec<Space>>> {
        Box::pin(self.spaces.find_spaces(target, among))
    }

    fn being_removed(&self, space: &Space) -> bool {
        self.spaces.being_removed(space)
    }

    fn workspace<'a>(&'a self, selector: &'a SpaceSelector) -> BoxFuture<'a, Result<Workspace>> {
        Box::pin(self.spaces.workspace(selector))
    }

    fn find_workspace<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<Option<Workspace>>> {
        Box::pin(self.spaces.find_workspace(id))
    }

    fn list_workspaces(&self) -> BoxFuture<'_, Result<Vec<Workspace>>> {
        Box::pin(self.spaces.list_workspaces())
    }

    fn chat_home<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<Option<SpaceId>>> {
        Box::pin(self.spaces.chat_home(id))
    }

    fn user_holds<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<bool>> {
        Box::pin(self.spaces.user_holds(id))
    }

    fn hold_decision_guard<'a>(
        &'a self,
        request: &'a ApprovalRequest,
        approve: bool,
    ) -> BoxFuture<'a, Result<Option<Box<dyn Send>>>> {
        Box::pin(async move {
            let guard = self
                .spaces
                .hold_decision_guard(self, request, approve)
                .await?;
            Ok(guard.map(|guard| Box::new(guard) as Box<dyn Send>))
        })
    }

    fn ensure_merge_current<'a>(
        &'a self,
        request: &'a ApprovalRequest,
        approve: bool,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(self.spaces.ensure_merge_current(self, request, approve))
    }
}

impl shoal_coordination::daemon::Files for StandIn {
    fn attacher<'a>(&'a self, caller: Option<&'a Caller>) -> BoxFuture<'a, Attacher> {
        Box::pin(self.spaces.attacher(self, caller))
    }

    fn attach_files<'a>(
        &'a self,
        space: &'a Space,
        source: FileSource,
        message: &'a str,
        files: Vec<NewFile>,
        attacher: &'a Attacher,
    ) -> BoxFuture<'a, Result<Attaching>> {
        Box::pin(async move {
            let space = space.clone().into();
            self.spaces
                .attach_files(self, &space, source, message, files, attacher)
                .await
        })
    }

    fn sent_files(&self, _: Vec<PathBuf>) -> BoxFuture<'_, Result<Vec<SentFile>>> {
        Box::pin(async { Ok(Default::default()) })
    }

    fn queue_with_files<'a>(
        &'a self,
        _: Space,
        _: NewAlert,
        _: Vec<NewFile>,
        _: &'a Attacher,
    ) -> BoxFuture<'a, Result<i64>> {
        Box::pin(async { Ok(Default::default()) })
    }
}

impl shoal_coordination::daemon::Unsent for StandIn {
    fn forget(self: Box<Self>) -> BoxFuture<'static, ()> {
        Box::pin(async {})
    }
}

impl shoal_coordination::daemon::Agents for StandIn {
    fn tracked_agents(&self) -> BoxFuture<'_, TrackedAgents> {
        Box::pin(self.spaces.tracked_agents(self))
    }

    fn running_agent_workspaces(&self) -> BoxFuture<'_, Vec<SpaceId>> {
        let executions = self.agents.executions(None);
        Box::pin(async { executions.into_iter().map(|(space, _)| space).collect() })
    }

    fn recorded_agent_spaces(&self) -> BoxFuture<'_, Result<HashSet<SpaceId>>> {
        let executions = self.agents.executions(None);
        Box::pin(async { Ok(executions.into_iter().map(|(space, _)| space).collect()) })
    }

    fn queued_agent_workspaces(&self) -> BoxFuture<'_, Result<HashSet<SpaceId>>> {
        Box::pin(async { Ok(Default::default()) })
    }

    fn has_saved_agent(&self, _: &SpaceId) -> bool {
        Default::default()
    }

    fn queue_alert<'a>(
        &'a self,
        _: &'a SpaceId,
        _: NewAlert,
        _: bool,
    ) -> BoxFuture<'a, Result<i64>> {
        Box::pin(async { Ok(Default::default()) })
    }

    fn queue_agent_message<'a>(
        &'a self,
        _: &'a SpaceId,
        _: AlertKind,
        _: String,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn mark_messages_delivered<'a>(
        &'a self,
        _: &'a SpaceSelector,
        _: Vec<i64>,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn queue_entries(&self) -> BoxFuture<'_, Result<Vec<QueueEntry>>> {
        Box::pin(async { Ok(Default::default()) })
    }

    fn launch_parents(&self, _: &[QueueEntry]) -> HashMap<QueueEntryId, SpaceId> {
        Default::default()
    }

    fn started_launch<'a>(&'a self, _: &'a QueueEntryId) -> BoxFuture<'a, Result<Option<SpaceId>>> {
        Box::pin(async { Ok(Default::default()) })
    }
}

impl shoal_coordination::daemon::Callers for StandIn {
    fn execution_caller<'a>(&'a self, _: &'a ScopeToken) -> BoxFuture<'a, Option<Caller>> {
        Box::pin(async { Default::default() })
    }
}

impl shoal_coordination::daemon::Hub for StandIn {
    fn ask_machine<'a>(
        &'a self,
        _: &'a Member,
        _: Operation,
        _: Option<&'a Caller>,
    ) -> BoxFuture<'a, Result<Body>> {
        Box::pin(async { bail!("{NOT_HERE}: ask_machine") })
    }

    fn names_vouched_own(&self, _: &Caller, _: &SpaceSelector) -> bool {
        Default::default()
    }

    fn vouched_caller(&self, _: &ScopeToken) -> Option<Option<Caller>> {
        Default::default()
    }

    fn runs_elsewhere<'a>(&'a self, _: &'a SpaceId) -> BoxFuture<'a, bool> {
        Box::pin(async { Default::default() })
    }

    fn ancestors_elsewhere<'a>(&'a self, _: &'a SpaceId) -> BoxFuture<'a, Vec<SpaceId>> {
        Box::pin(async { Default::default() })
    }

    fn reached_through_elsewhere<'a>(&'a self, _: &'a SpaceId) -> BoxFuture<'a, Vec<SpaceId>> {
        Box::pin(async { Default::default() })
    }

    fn reaches_elsewhere<'a>(
        &'a self,
        _: &'a Caller,
        _: &'a SpaceId,
    ) -> BoxFuture<'a, Result<bool>> {
        Box::pin(async { Ok(Default::default()) })
    }

    fn deciders_elsewhere<'a>(&'a self, _: &'a SpaceId, _: bool) -> BoxFuture<'a, Vec<SpaceId>> {
        Box::pin(async { Default::default() })
    }

    fn permissions_elsewhere<'a>(
        &'a self,
        _: &'a SpaceId,
        _: Option<&'a RepositoryId>,
    ) -> BoxFuture<'a, Result<Option<Permissions>>> {
        Box::pin(async { Ok(Default::default()) })
    }

    fn reported_above<'a>(
        &'a self,
        _: &'a SpaceId,
    ) -> BoxFuture<'a, Result<Option<shoal_coordination::daemon::Reported>>> {
        Box::pin(async { Ok(Default::default()) })
    }

    fn repositories_view(&self) -> BoxFuture<'_, Result<Option<HubView>>> {
        Box::pin(async { Ok(Default::default()) })
    }

    fn acting_space<'a>(
        &'a self,
        _: Option<&'a Caller>,
        _: &'a SpaceSelector,
    ) -> BoxFuture<'a, Result<(SpaceId, String)>> {
        Box::pin(async { bail!("{NOT_HERE}: acting_space") })
    }

    fn message_elsewhere<'a>(
        &'a self,
        _: &'a SpaceId,
        _: AlertKind,
        _: String,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn ask_machine_or_hold<'a>(
        &'a self,
        _: &'a Member,
        _: Operation,
        _: Option<&'a Caller>,
    ) -> BoxFuture<'a, Result<Body>> {
        Box::pin(async { bail!("{NOT_HERE}: ask_machine_or_hold") })
    }

    fn pass_up<'a>(&'a self, _: SpaceId, _: PassedUp<'a>) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn mark_alerts_elsewhere<'a>(
        &'a self,
        _: &'a shoal_ids::member_ids::MemberId,
        _: shoal_protocol::hub::alerts::AlertMarks,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { anyhow::bail!("no other machine") })
    }

    fn withdraw_elsewhere<'a>(
        &'a self,
        _: &'a shoal_ids::member_ids::MemberId,
        _: i64,
        _: Option<&'a shoal_coordination::scope::Caller>,
    ) -> BoxFuture<'a, Result<shoal_protocol::alerts::Alert>> {
        Box::pin(async { anyhow::bail!("no other machine") })
    }
}

impl shoal_coordination::daemon::Repositories for StandIn {
    fn repository(&self, selector: Selector) -> BoxFuture<'_, Result<Repository>> {
        Box::pin(self.find_repository(selector))
    }

    fn repositories(&self) -> BoxFuture<'_, Result<Vec<Repository>>> {
        Box::pin(self.store.run(|db| repositories(db)))
    }

    fn space_settings_in<'a>(
        &'a self,
        id: Option<&'a RepositoryId>,
    ) -> BoxFuture<'a, Result<Effective>> {
        Box::pin(async move { self.config.current().resolve(&self.layers(id, None).await?) })
    }

    fn space_settings_from<'a>(
        &'a self,
        config: &'a Config,
        space: &'a Space,
        repository: Option<&'a RepositoryId>,
    ) -> BoxFuture<'a, Result<Effective>> {
        Box::pin(async move { config.resolve(&self.space_layers(space, repository).await?) })
    }
}

impl shoal_coordination::daemon::Reviews for StandIn {
    fn answer_posts(&self) {}

    fn give_gate<'a>(&'a self, _: &'a ApprovalRequest) -> BoxFuture<'a, Result<bool>> {
        Box::pin(async { Ok(Default::default()) })
    }

    fn keep_false_positive(
        &self,
        _: Thread,
        _: Option<Outcome>,
    ) -> BoxFuture<'_, Result<Option<Box<dyn KeptFinding + '_>>>> {
        Box::pin(async { Ok(Default::default()) })
    }
}

impl shoal_coordination::daemon::KeptFinding for StandIn {
    fn undo<'a>(self: Box<Self>) -> BoxFuture<'a, Result<()>>
    where
        Self: 'a,
    {
        Box::pin(async { Ok(()) })
    }
}

impl shoal_coordination::daemon::Links for StandIn {
    fn ensure_not_pushing(&self, _: &ApprovalRequest) -> Result<()> {
        Ok(())
    }

    fn decide_proposal<'a>(
        &'a self,
        _: ApprovalRequest,
        _: bool,
        _: ProposalDecision,
        _: Option<BTreeMap<String, String>>,
        _: Option<&'a SpaceId>,
    ) -> BoxFuture<'a, Result<ApprovalRequest>> {
        Box::pin(async { bail!("{NOT_HERE}: decide_proposal") })
    }

    fn prune_proposal_files<'a>(&'a self, _: &'a ApprovalRequest) -> BoxFuture<'a, ()> {
        Box::pin(async {})
    }
}
