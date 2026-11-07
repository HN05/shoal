//! Space cleanup: automatic removal of idle spaces, clean and fully pushed
//! where they have a worktree, of merged ones, and of workspaces whose
//! directory was deleted outside Shoal; the user can remove the idle
//! candidates on request. A chat has no worktree, so it meets every condition
//! on one, and goes once its agent stopped and nothing keeps it.
use anyhow::{Result, ensure};
use std::{
    collections::{HashMap, hash_map::DefaultHasher},
    fs,
    hash::{Hash, Hasher},
    os::unix::fs::MetadataExt,
    path::Path,
    sync::Arc,
    time::Duration,
};
use tokio::time::{Instant, sleep};

pub(crate) use crate::client::cleanup::{
    CacheCleanup, Kept, ManualCleanup, Pick, Retained, Selection,
};
use crate::client::space_names::SpaceSelector;
use crate::{
    daemon::{
        caches::Emptying, events::EventCause, file_access, log, spaces::removal_notices::Cleanup,
        workspace::Manager,
    },
    fsutil::off_runtime,
    model::{Chat, CleanupReason, Workspace},
    process::DirectoryUsers,
    state::WorkspaceState,
    time::UnixTime,
};

mod failures;
mod grace;
mod health;
mod kept_branches;
mod merged;

pub(crate) use failures::{SpaceFailures, SweepFailures};
pub(crate) use health::CleanupHealth;

const SWEEP_INTERVAL: Duration = Duration::from_secs(30);

/// The rules that made a space a cleanup candidate, rechecked during removal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rule {
    /// Clean, unused, every commit pushed or merged, and no assignment state.
    Idle,
    /// Merged into the default branch and unused, whatever its assignment.
    Merged,
}

impl Rule {
    /// Whether the worktree's state still satisfies the rule.
    pub fn allows(self, check: &crate::removal::RemovalCheck) -> bool {
        match self {
            Rule::Idle => check.safe(),
            // Unsaved files go into the checkpoint. A base stays until PR
            // cleanup has moved the workspaces stacked on it.
            Rule::Merged => {
                check.unused()
                    && check.matches_default_branch
                    && check.workspace.stacked_workspaces.is_empty()
            }
        }
    }

    pub fn describe(self) -> &'static str {
        match self {
            Rule::Idle => "idle, clean and fully pushed",
            Rule::Merged => "unused and merged",
        }
    }
}

/// What automatic cleanup must see unchanged before removing a space.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Snapshot {
    pub rule: Rule,
    /// HEAD and recorded activity, cheap enough to compare on every pass.
    pub signals: u64,
    /// The metadata of every file in the worktree, which takes a full walk;
    /// a chat has none.
    pub tree: u64,
}

/// When a space was last seen unchanged in a removable state.
struct Idle {
    snapshot: Snapshot,
    since: Instant,
}

#[derive(Default)]
pub struct Timers {
    idle: HashMap<String, Idle>,
    merged: merged::State,
    kept_branches: kept_branches::State,
}

impl Timers {
    /// Whether the timer has run for `delay` with unchanged `signals`; a
    /// change, or no timer, needs a full snapshot to start from.
    fn expired(&self, id: &str, signals: u64, now: Instant, delay: Duration) -> Expiry {
        match self.idle.get(id) {
            Some(idle) if idle.snapshot.signals != signals => Expiry::Restart,
            Some(idle) if now.duration_since(idle.since) < delay => Expiry::Running,
            Some(idle) => Expiry::Expired(idle.snapshot),
            None => Expiry::Restart,
        }
    }

    fn start(&mut self, id: &str, snapshot: Snapshot, now: Instant) {
        self.idle.insert(
            id.into(),
            Idle {
                snapshot,
                since: now,
            },
        );
    }
}

enum Expiry {
    Running,
    Restart,
    Expired(Snapshot),
}

/// HEAD and the activity counter of a cleanup candidate.
pub fn signals(head: &str, activity: u64) -> u64 {
    let mut hash = DefaultHasher::new();
    head.hash(&mut hash);
    activity.hash(&mut hash);
    hash.finish()
}

/// What one walk of a worktree's files sees.
#[derive(Clone, Copy, Debug)]
pub struct Tree {
    /// The metadata of every file. Change times only move forward and cannot
    /// be set, unlike modification times, so two equal fingerprints mean no
    /// file changed between them.
    pub fingerprint: u64,
    /// The newest change or modification time of any file.
    pub changed: UnixTime,
}

/// The later of a file's change and modification times.
pub fn change_time(metadata: &fs::Metadata) -> UnixTime {
    UnixTime::from_seconds(metadata.ctime().max(metadata.mtime()))
}

/// Metadata only, including ignored files; never follow symlinks out of the tree.
/// Git administrative storage is shared and inspected separately through HEAD.
pub fn walk(root: &Path) -> Result<Tree> {
    fn visit(path: &Path, hash: &mut DefaultHasher, changed: &mut UnixTime) -> Result<()> {
        let metadata = fs::symlink_metadata(path)?;
        path.hash(hash);
        metadata.len().hash(hash);
        metadata.modified()?.hash(hash);
        (metadata.ctime(), metadata.ctime_nsec()).hash(hash);
        *changed = (*changed).max(change_time(&metadata));
        if metadata.is_dir() {
            let mut children = fs::read_dir(path)?
                .map(|entry| entry.map(|e| e.path()))
                .collect::<std::io::Result<Vec<_>>>()?;
            children.sort();
            for child in children {
                if child.file_name().is_some_and(|name| name == ".git") {
                    continue;
                }
                visit(&child, hash, changed)?;
            }
        }
        Ok(())
    }
    let mut hash = DefaultHasher::new();
    let mut changed = UnixTime::EPOCH;
    visit(root, &mut hash, &mut changed)?;
    Ok(Tree {
        fingerprint: hash.finish(),
        changed,
    })
}

/// One pass of every automatic cleanup. A failed step does not skip the
/// steps after it; the error names each step that failed, in the workspace
/// it failed in where it concerns one.
pub async fn sweep(manager: &Manager, timers: &mut Timers) -> Result<(), SweepFailures> {
    manager.cleanup_orphan_sessions().await;
    let mut failed = SweepFailures::default();
    let mut step = |name: &str, result: Result<()>| failed.step(name, result);
    step(
        "ownership recheck",
        manager.recheck_startup_failures().await,
    );
    step("grace periods", manager.begin_grace_pass().await);
    step("branch observation", observe_branches(manager).await);
    step("completion cleanup", manager.sweep_completed().await);
    step("PR cleanup", manager.sweep_prs().await);
    // Every step that asks which processes use a worktree reads one listing.
    let users = DirectoryUsers::default();
    step("idle cleanup", sweep_idle(manager, timers, &users).await);
    step("idle agents", manager.stop_idle_agents().await);
    step(
        "merged cleanup",
        merged::sweep(manager, &mut timers.merged, &users).await,
    );
    step(
        "kept branches",
        kept_branches::sweep(manager, &mut timers.kept_branches).await,
    );
    step("checkpoint expiry", manager.expire_checkpoints().await);
    step("result expiry", manager.expire_results().await);
    step("event expiry", manager.expire_events().await);
    step(
        "earlier attached files",
        manager.record_earlier_files().await,
    );
    step("contributions", manager.sweep_contributions().await);
    step("proposal files", manager.sweep_proposal_files().await);
    step("temporary directories", manager.sweep_tmp_dirs().await);
    step("leftovers", manager.find_leftovers().await);
    step("grace periods", manager.end_unclaimed_grace().await);
    failed.into_result()
}

async fn observe_branches(manager: &Manager) -> Result<()> {
    for workspace in manager.list_workspaces().await? {
        if workspace.state != WorkspaceState::Ready || !is_dir(&workspace.path).await {
            continue;
        }
        let observed = manager.observe_workspace_branch(&workspace);
        if let Err(error) = manager.check_access(&workspace.path, observed).await {
            log!("branch observation skipped {}: {error:#}", workspace.name);
        }
    }
    Ok(())
}

/// Whether `path` is a directory, read off the runtime's threads and within
/// a check's time limit, since stalled file access can hold the read. The daemon's
/// own read says nothing about the processes it starts there, so it records
/// no stall.
async fn is_dir(path: &std::path::Path) -> bool {
    matches!(
        tokio::time::timeout(file_access::LIMIT, off_runtime::is_dir(path)).await,
        Ok(Ok(true))
    )
}

/// Each space's idle delay comes from its own layered config, a chat's from
/// its repository's; a disabled workspace gets only deleted-directory
/// cleanup. A failure in one space does not skip the others.
async fn sweep_idle(manager: &Manager, timers: &mut Timers, users: &DirectoryUsers) -> Result<()> {
    let workspaces = manager.list_workspaces().await?;
    let chats = manager.chats().await?;
    timers.idle.retain(|id, _| {
        workspaces.iter().any(|workspace| &workspace.id == id)
            || chats.iter().any(|chat| &chat.id == id)
    });
    let mut failed = SpaceFailures::default();
    for workspace in workspaces {
        if let Err(error) = sweep_workspace(manager, timers, &workspace, users).await {
            failed.push(&workspace.id, &workspace.name, &error);
        }
    }
    for chat in chats {
        if let Err(error) = sweep_chat(manager, timers, &chat).await {
            failed.push(&chat.id, &chat.name, &error);
        }
    }
    failed.into_result()
}

/// Remove `chat` once idle cleanup has seen it unchanged for its idle delay
/// and its grace period ended.
async fn sweep_chat(manager: &Manager, timers: &mut Timers, chat: &Chat) -> Result<()> {
    let settings = manager.chat_settings(chat).await?;
    let snapshot = match settings.cleanup.auto.delay() {
        Some(delay) => (manager.chat_cleanup_snapshot(chat, Rule::Idle).await?)
            .map(|snapshot| (delay, snapshot)),
        None => None,
    };
    let Some((delay, snapshot)) = snapshot else {
        timers.idle.remove(&chat.id);
        return Ok(());
    };
    let now = Instant::now();
    match timers.expired(&chat.id, snapshot.signals, now, delay) {
        Expiry::Running => return Ok(()),
        Expiry::Expired(started) if started == snapshot => {}
        Expiry::Restart | Expiry::Expired(_) => {
            timers.start(&chat.id, snapshot, now);
            return Ok(());
        }
    }
    let removal = (manager.remove_unused_chat(chat, snapshot, EventCause::Idle)).await;
    if matches!(removal, Ok(false)) {
        return Ok(());
    }
    timers.idle.remove(&chat.id);
    manager
        .announce_removal(chat, Cleanup::Idle, &removal)
        .await
}

async fn sweep_workspace(
    manager: &Manager,
    timers: &mut Timers,
    workspace: &Workspace,
    users: &DirectoryUsers,
) -> Result<()> {
    // Its idle timer stays for the pass after a check there finishes.
    if manager.sweep_skips(&workspace.path).await {
        return Ok(());
    }
    // Only a worktree that existed (identity recorded) can have been deleted;
    // a failed creation keeps its error until removed explicitly.
    if matches!(
        workspace.state,
        WorkspaceState::Ready | WorkspaceState::Failed
    ) && workspace.git_dir.is_some()
        && !off_runtime::try_exists(&workspace.path).await?
    {
        timers.idle.remove(&workspace.id);
        remove_deleted(manager, workspace).await;
        return Ok(());
    }
    let Some((delay, signals)) = observe(manager, workspace, users).await else {
        timers.idle.remove(&workspace.id);
        return Ok(());
    };
    // Each pass compares only the signals; Git state is inspected and the
    // tree walked when the timer starts and again when it expires.
    let now = Instant::now();
    let started = match timers.expired(&workspace.id, signals, now, delay) {
        Expiry::Running => return Ok(()),
        Expiry::Restart => None,
        Expiry::Expired(snapshot) => Some(snapshot),
    };
    let Some(snapshot) = candidate(manager, workspace, users).await else {
        timers.idle.remove(&workspace.id);
        return Ok(());
    };
    if started != Some(snapshot) {
        timers.start(&workspace.id, snapshot, now);
        return Ok(());
    }
    let removal = manager
        .remove_idle(&SpaceSelector::id(&workspace.id), snapshot)
        .await;
    if matches!(removal, Ok(false)) {
        return Ok(());
    }
    timers.idle.remove(&workspace.id);
    (manager.announce_removal(workspace, Cleanup::Idle, &removal)).await
}

/// The idle delay of a ready workspace, or `None` when idle cleanup is
/// disabled for it.
async fn idle_delay(manager: &Manager, workspace: &Workspace) -> Result<Option<Duration>> {
    if workspace.state != WorkspaceState::Ready {
        return Ok(None);
    }
    let settings = manager.workspace_settings(workspace).await?;
    Ok(settings.cleanup.auto.delay())
}

/// The idle delay and timer signals of a ready workspace nothing recorded
/// and no process keeps; `None` when something does, its idle cleanup is
/// disabled, or its config cannot be read.
async fn observe(
    manager: &Manager,
    workspace: &Workspace,
    users: &DirectoryUsers,
) -> Option<(Duration, u64)> {
    let observed = async {
        let Some(delay) = idle_delay(manager, workspace).await? else {
            return Ok(None);
        };
        let signals = manager.idle_timer_signals(&workspace.id, users).await?;
        Ok(signals.map(|signals| (delay, signals)))
    }
    .await;
    skip_failed(workspace, observed)
}

/// The full snapshot of an idle cleanup candidate, with the processes in
/// its worktree read from `users`.
pub(super) async fn candidate(
    manager: &Manager,
    workspace: &Workspace,
    users: &DirectoryUsers,
) -> Option<Snapshot> {
    let observed = async {
        if idle_delay(manager, workspace).await?.is_none() {
            return Ok(None);
        }
        manager.swept_snapshot(&workspace.id, users).await
    }
    .await;
    skip_failed(workspace, observed)
}

fn skip_failed<T>(workspace: &Workspace, observed: Result<Option<T>>) -> Option<T> {
    observed.unwrap_or_else(|error| {
        log!("auto cleanup skipped {}: {error:#}", workspace.name);
        None
    })
}

/// Operations on the client type [`ManualCleanup`] that only the `shoal` command uses.
trait ManualCleanupExt {
    /// A dry run lists why it keeps each space, and a request naming spaces
    /// says why it kept those.
    fn keep(&mut self, space: String, reason: CleanupReason, named: bool);

    /// Record that removing the space `space` failed with `error`.
    fn retain(&mut self, space: String, error: &anyhow::Error);
}

impl ManualCleanupExt for ManualCleanup {
    fn keep(&mut self, space: String, reason: CleanupReason, named: bool) {
        if self.dry_run || named {
            self.kept.push(Kept { space, reason });
        }
    }

    fn retain(&mut self, space: String, error: &anyhow::Error) {
        self.retained.push(Retained {
            space,
            error: format!("{error:#}"),
        });
    }
}

/// Operations on the client type [`Pick`] that only the `shoal` command uses.
trait PickExt {
    fn covers(&self, name: &str, id: &str) -> bool;

    /// Fails for a name none of `items`, as `(name, id)` pairs, has.
    fn ensure_known<'a>(
        &self,
        items: impl Iterator<Item = (&'a str, &'a str)> + Clone,
    ) -> Result<()>;
}

impl PickExt for Pick {
    fn covers(&self, name: &str, id: &str) -> bool {
        match self {
            Self::None => false,
            Self::All => true,
            Self::Only(names) => names.iter().any(|n| n == name || n == id),
        }
    }

    fn ensure_known<'a>(
        &self,
        items: impl Iterator<Item = (&'a str, &'a str)> + Clone,
    ) -> Result<()> {
        if let Self::Only(names) = self {
            for selector in names {
                ensure!(
                    items
                        .clone()
                        .any(|(name, id)| selector == name || selector == id),
                    "{selector} is not a space or repository this cleanup covers"
                );
            }
        }
        Ok(())
    }
}

/// Remove the selected spaces idle or merged cleanup would remove, now: the
/// user's request replaces their idle periods and applies where they are
/// disabled. Then empty the selected shared caches nothing is using.
pub async fn on_request(
    manager: &Manager,
    dry_run: bool,
    selection: &Selection,
) -> Result<ManualCleanup> {
    let targets = Targets::of(manager, selection).await?;
    if !dry_run {
        manager.cleanup_orphan_sessions().await;
    }
    let mut report = ManualCleanup {
        dry_run,
        ..ManualCleanup::default()
    };
    remove_on_request(manager, &mut report, targets.workspaces, &selection.spaces).await?;
    remove_chats_on_request(manager, &mut report, targets.chats, &selection.spaces).await?;
    report.caches =
        empty_caches_on_request(manager, dry_run, targets.repositories, &selection.caches).await?;
    Ok(report)
}

/// The spaces and repositories a cleanup request can reach.
struct Targets {
    workspaces: Vec<Workspace>,
    chats: Vec<Chat>,
    repositories: Vec<crate::model::Repository>,
}

impl Targets {
    /// Those of the selected repository, or all, once every name the
    /// selection gives is among them.
    async fn of(manager: &Manager, selection: &Selection) -> Result<Self> {
        let repository = match &selection.repository {
            Some(selector) => Some(manager.repository(selector).await?.id),
            None => None,
        };
        let within = |id: &str| repository.as_deref().is_none_or(|only| only == id);
        // A chat without a repository is in none of them.
        let chat_within = |chat: &Chat| match &chat.repository_id {
            Some(id) => within(id),
            None => repository.is_none(),
        };
        let targets = Self {
            workspaces: manager
                .list_workspaces()
                .await?
                .into_iter()
                .filter(|workspace| within(&workspace.repository_id))
                .collect(),
            chats: (manager.chats().await?.into_iter())
                .filter(|chat| chat_within(chat))
                .collect(),
            repositories: manager
                .repositories()
                .await?
                .into_iter()
                .filter(|repo| within(&repo.id))
                .collect(),
        };
        let workspaces = (targets.workspaces.iter()).map(|w| (w.name.as_str(), w.id.as_str()));
        let chats = (targets.chats.iter()).map(|chat| (chat.name.as_str(), chat.id.as_str()));
        selection.spaces.ensure_known(workspaces.chain(chats))?;
        selection.caches.ensure_known(
            targets
                .repositories
                .iter()
                .map(|repo| (crate::repository::name(repo), repo.id.as_str())),
        )?;
        Ok(targets)
    }
}

async fn empty_caches_on_request(
    manager: &Manager,
    dry_run: bool,
    repositories: Vec<crate::model::Repository>,
    pick: &Pick,
) -> Result<Vec<CacheCleanup>> {
    let mut report = Vec::new();
    for repo in repositories {
        if !pick.covers(crate::repository::name(&repo), &repo.id) {
            continue;
        }
        let emptying = match manager.empty_caches(&repo, dry_run).await {
            Ok(None) => continue,
            Ok(Some(emptying)) => emptying,
            Err(error) => {
                log!(
                    "manual cleanup kept caches of {}: {error:#}",
                    repo.path.display()
                );
                Emptying::Failed {
                    error: format!("{error:#}"),
                }
            }
        };
        report.push(CacheCleanup {
            repository: crate::repository::name(&repo).to_owned(),
            repository_id: repo.id.clone(),
            emptying,
        });
    }
    Ok(report)
}

async fn remove_on_request(
    manager: &Manager,
    report: &mut ManualCleanup,
    workspaces: Vec<Workspace>,
    pick: &Pick,
) -> Result<()> {
    let named = matches!(pick, Pick::Only(_));
    for workspace in workspaces {
        if !pick.covers(&workspace.name, &workspace.id) {
            continue;
        }
        if workspace.state != WorkspaceState::Ready {
            let state = workspace.state;
            report.keep(
                workspace.name.into(),
                CleanupReason::NotReady { state },
                named,
            );
            continue;
        }
        let snapshot = match requested_candidacy(manager, &workspace).await {
            Ok(Ok(snapshot)) => snapshot,
            Ok(Err(reason)) => {
                report.keep(workspace.name.into(), reason, named);
                continue;
            }
            Err(error) => {
                log!("manual cleanup skipped {}: {error:#}", workspace.name);
                report.retain(workspace.name.into(), &error);
                continue;
            }
        };
        if report.dry_run {
            report.removed.push(workspace.name.into());
            continue;
        }
        match manager
            .remove_on_request(&SpaceSelector::id(&workspace.id), snapshot)
            .await
        {
            Ok(()) => {
                log!("manual cleanup removed {}", workspace.name);
                report.removed.push(workspace.name.into());
            }
            Err(error) => {
                manager
                    .record_retained(&workspace.id, EventCause::Manual, &error)
                    .await?;
                log!("manual cleanup retained {}: {error:#}", workspace.name);
                report.retain(workspace.name.into(), &error);
            }
        }
    }
    Ok(())
}

/// Remove the selected chats idle or merged cleanup would remove, now.
async fn remove_chats_on_request(
    manager: &Manager,
    report: &mut ManualCleanup,
    chats: Vec<Chat>,
    pick: &Pick,
) -> Result<()> {
    let named = matches!(pick, Pick::Only(_));
    for chat in chats {
        if !pick.covers(&chat.name, &chat.id) {
            continue;
        }
        let snapshot = match manager.requested_chat_candidacy(&chat).await {
            Ok(Ok(snapshot)) => snapshot,
            Ok(Err(reason)) => {
                report.keep(chat.name.into(), reason, named);
                continue;
            }
            Err(error) => {
                log!("manual cleanup skipped chat {}: {error:#}", chat.name);
                report.retain(chat.name.into(), &error);
                continue;
            }
        };
        if report.dry_run {
            report.removed.push(chat.name.into());
            continue;
        }
        match (manager.remove_unused_chat(&chat, snapshot, EventCause::Manual)).await {
            Ok(_) => {
                log!("manual cleanup removed chat {}", chat.name);
                report.removed.push(chat.name.into());
            }
            Err(error) => {
                log!("manual cleanup retained chat {}: {error:#}", chat.name);
                report.retain(chat.name.into(), &error);
            }
        }
    }
    Ok(())
}

/// The snapshot of an idle or merged cleanup candidate, or why idle cleanup
/// keeps the workspace. Merged candidates need a freshly fetched default branch.
async fn requested_candidacy(
    manager: &Manager,
    workspace: &Workspace,
) -> Result<Result<Snapshot, CleanupReason>> {
    let reason = match manager.cleanup_candidacy(&workspace.id).await? {
        Ok(snapshot) => return Ok(Ok(snapshot)),
        Err(reason) => reason,
    };
    if !manager.default_branch_fresh(&workspace.repository_id).await {
        return Ok(Err(reason));
    }
    Ok(manager.merged_snapshot(&workspace.id).await?.ok_or(reason))
}

/// A deleted worktree is forgotten unless it was moved or still has commands
/// Shoal cannot verify; the branch is retained either way.
async fn remove_deleted(manager: &Manager, workspace: &Workspace) {
    match manager.missing_worktree(workspace).await {
        Ok(Some(_)) => return,
        Ok(None) => {}
        Err(error) => {
            log!("deleted worktree {} retained: {error:#}", workspace.name);
            return;
        }
    }
    let removal = manager
        .remove_deleted(&SpaceSelector::id(&workspace.id))
        .await;
    let announced = manager.announce_removal(workspace, Cleanup::DeletedWorktree, &removal);
    if let Err(error) = announced.await {
        log!("record retained workspace {}: {error:#}", workspace.name);
    }
}

pub async fn run(manager: Arc<Manager>) {
    let mut timers = Timers::default();
    loop {
        {
            let _operation = manager.gates.operation().await;
            let result = sweep(&manager, &mut timers).await;
            if let Err(failed) = &result {
                log!("auto cleanup: {failed}");
            }
            manager.record_sweep(result).await;
        }
        tokio::select! {
            _ = sleep(SWEEP_INTERVAL) => {},
            _ = manager.signals.cleanup_woken() => {},
        }
    }
}

#[cfg(test)]
mod tests;
