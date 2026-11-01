//! Persisted opt-in PR watches and manual merge acknowledgements.
use crate::daemon::refusal::{Reason, Refusal};
use anyhow::{Context, Result, bail, ensure};
use rusqlite::{Connection, OptionalExtension};

pub(crate) use crate::client::links::pr::{Action, Registration, RegistrationKind};
use crate::daemon::log;
use crate::{
    daemon::{
        events::{self, EventKind},
        forge_hosts::ForgeHosts,
        links::space::LinkSpace,
        notifications::NotificationKind,
        store,
        workspace::{Manager, PrGuard},
    },
    forge::{ForgeRepo, account::Account, items::link::ItemKind, review::status::PrStatus},
    git,
    model::Workspace,
    repository,
};

/// How a new PR watch is checked before it is recorded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WatchCheck {
    /// Look the PR up with the user's login.
    Lookup,
    /// The caller just read the PR from the forge with the workspace branch as
    /// its head, so a lookup that could fail now must not leave it unlinked.
    Known,
}

/// Prefixes errors from moving stacked workspaces, which a later sweep clears.
const STACK_ERROR: &str = "could not move the workspaces stacked on it";

impl Manager {
    pub async fn pr_registration(&self, id: &str) -> Result<Option<Registration>> {
        let id = id.to_owned();
        self.store
            .run(move |db| {
                let record: Option<String> = db
                    .query_row(
                        "SELECT record FROM pr_cleanup WHERE space_id=?1",
                        [id],
                        |r| r.get(0),
                    )
                    .optional()?;
                record
                    .map(|s| serde_json::from_str(&s).map_err(Into::into))
                    .transpose()
            })
            .await
    }

    pub async fn set_pr(&self, selector: &str, action: Action) -> Result<()> {
        let space = self.link_space(selector).await?;
        if let LinkSpace::Chat { .. } = space {
            return self.set_chat_pr(&space, action).await;
        }
        let (workspace, _guard) = self.lock_pr_gate(space.id()).await?;
        self.set_pr_gated(&workspace.id, action, WatchCheck::Lookup)
            .await
    }

    /// [`Self::set_pr`] for a chat, whose PR watches drive no cleanup and
    /// whose checkout's branch is not its own.
    async fn set_chat_pr(&self, chat: &LinkSpace, action: Action) -> Result<()> {
        let id = chat.id().to_owned();
        let _guard = self.gates.lock_prs([&id]).await;
        let previous = match self.pr_registration(&id).await {
            Ok(previous) => previous,
            Err(_) if matches!(action, Action::Clear) => None,
            Err(error) => return Err(error),
        };
        let kind = match action {
            Action::Clear => None,
            Action::Unwatch { url } => {
                (self.without_pr_watch(chat.origin_path(), &id, &url)).await?
            }
            Action::Watch { url } => {
                let (_, _, url) = self.pr_forge(chat.origin_path(), &url).await?;
                Some(self.adding_pr_watch(&id, url).await?)
            }
            Action::Acknowledge => bail!("a chat has no branch whose merge it acknowledges"),
        };
        let change = RegistrationChange::new(
            previous.as_ref(),
            kind.map(|kind| Registration { kind, error: None }),
        );
        self.record_chat_registration(id, change).await
    }

    /// Applies `change` to the PR registration of the chat `id`, unless the
    /// chat ended meanwhile, whose end deleted its registration.
    async fn record_chat_registration(&self, id: String, change: RegistrationChange) -> Result<()> {
        self.store
            .run(move |db| record_chat_change(db, &id, change))
            .await
    }

    /// [`Self::set_pr`] for a caller already holding the workspace's PR gate.
    pub(crate) async fn set_pr_gated(
        &self,
        selector: &str,
        action: Action,
        check: WatchCheck,
    ) -> Result<()> {
        let workspace = self.workspace(selector).await?;
        // Links serve more than cleanup; only an acknowledgement exists for it alone.
        ensure!(
            !matches!(action, Action::Acknowledge)
                || self
                    .workspace_settings(&workspace)
                    .await?
                    .cleanup
                    .pr
                    .enabled,
            "PR cleanup is disabled by [cleanup.pr] enabled = false"
        );
        if !matches!(action, Action::Clear | Action::Unwatch { .. }) {
            self.verify_worktree(&workspace).await?;
        }
        let previous = match self.pr_registration(&workspace.id).await {
            Ok(previous) => previous,
            Err(_) if matches!(action, Action::Clear) => None,
            Err(error) => return Err(error),
        };
        let kind = match action {
            Action::Clear => None,
            Action::Unwatch { url } => {
                (self.without_pr_watch(&workspace.path, &workspace.id, &url)).await?
            }
            Action::Watch { url } => Some(self.with_pr_watch(&workspace, &url, check).await?),
            Action::Acknowledge => Some(RegistrationKind::Acknowledgement {
                head: current_head(&workspace).await?,
            }),
        };
        let change = RegistrationChange::new(
            previous.as_ref(),
            kind.map(|kind| Registration { kind, error: None }),
        );
        let id = workspace.id;
        self.store
            .run(move |db| {
                let tx = db.transaction()?;
                store::require_ready(&tx, &id)?;
                change.apply(&tx, &id)?;
                tx.commit()?;
                Ok(())
            })
            .await?;
        self.signals.wake_cleanup();
        Ok(())
    }

    async fn with_pr_watch(
        &self,
        workspace: &Workspace,
        input: &str,
        check: WatchCheck,
    ) -> Result<RegistrationKind> {
        current_head(workspace).await?;
        let (forge, number, url) = self.pr_forge(&workspace.path, input).await?;
        // Detect missing tools/login, wrong branches and invalid PRs now.
        if check == WatchCheck::Lookup {
            forge
                .merged_commits(&self.user_account(), number, &workspace.branch)
                .await?;
        }
        self.adding_pr_watch(&workspace.id, url).await
    }

    /// The PR watches of the space `id` with `url` among them.
    async fn adding_pr_watch(&self, id: &str, url: String) -> Result<RegistrationKind> {
        let (mut urls, mut merged_head) = match self.pr_registration(id).await? {
            Some(Registration {
                kind: RegistrationKind::Watch { urls, merged_head },
                ..
            }) => (urls, merged_head),
            _ => (Vec::new(), None),
        };
        if !urls.contains(&url) {
            urls.push(url);
            merged_head = None;
        }
        Ok(RegistrationKind::Watch { urls, merged_head })
    }

    /// The PR watches of the space `id` without the PR `input` names in the
    /// repository whose origin `checkout` has, which must be watched.
    async fn without_pr_watch(
        &self,
        checkout: &std::path::Path,
        id: &str,
        input: &str,
    ) -> Result<Option<RegistrationKind>> {
        let (_, _, url) = self.pr_forge(checkout, input).await?;
        let Some(Registration {
            kind:
                RegistrationKind::Watch {
                    mut urls,
                    merged_head,
                },
            ..
        }) = self.pr_registration(id).await?
        else {
            return Err(Refusal::new(Reason::NotFound, format!("PR is not watched: {url}")).into());
        };
        ensure!(
            urls.contains(&url),
            Refusal::new(Reason::NotFound, format!("PR is not watched: {url}"))
        );
        urls.retain(|watched| watched != &url);
        Ok((!urls.is_empty()).then_some(RegistrationKind::Watch { urls, merged_head }))
    }

    /// The forge of the repository whose origin `checkout` has, with the
    /// number and canonical URL of the PR `input` names there.
    pub(super) async fn pr_forge(
        &self,
        checkout: &std::path::Path,
        input: &str,
    ) -> Result<(ForgeRepo, u64, String)> {
        let remote = repository::remote_url_from_path(checkout)
            .await?
            .context("PR lookup needs an origin remote")?;
        let forge = self.forge(&remote).await?;
        let (number, url) = forge.pull(input)?;
        Ok((forge, number, url))
    }

    /// Watches survive restarts. A failed lookup never counts as a merge and a
    /// failed removal keeps its registration and ownership records for retry.
    /// Each workspace's PR gate, and those of the workspaces stacked on it,
    /// are held only while that workspace is handled. A failure in one
    /// workspace does not skip the others.
    pub async fn sweep_prs(&self) -> Result<()> {
        let mut failed = crate::daemon::cleanup::WorkspaceFailures::default();
        for listed in self.list_workspaces().await? {
            if let Err(error) = self.sweep_listed_prs(&listed).await {
                failed.push(&listed.id, &listed.name, &error);
            }
        }
        failed.into_result()
    }

    async fn sweep_listed_prs(&self, listed: &Workspace) -> Result<()> {
        if self.sweep_skips(&listed.path).await {
            return Ok(());
        }
        match self.lock_for_pr_sweep(listed).await? {
            Some((workspace, _guard)) => self.sweep_workspace_prs(&workspace).await,
            None => Ok(()),
        }
    }

    /// The `listed` workspace, read again under its PR gate and those of the
    /// workspaces stacked on it, while it is ready. One that had a workspace
    /// stacked on it since the listing waits for the next sweep.
    async fn lock_for_pr_sweep(&self, listed: &Workspace) -> Result<Option<(Workspace, PrGuard)>> {
        let locked = |id: &str| listed.stacked_workspaces.iter().any(|held| held.id == id);
        let stacked = listed.stacked_workspaces.iter().map(|stacked| &stacked.id);
        let guard = self.gates.lock_prs(stacked.chain([&listed.id])).await;
        let Some(workspace) = self.find_workspace(&listed.id).await? else {
            return Ok(None);
        };
        let unchanged = workspace
            .stacked_workspaces
            .iter()
            .all(|stacked| locked(&stacked.id));
        let ready = workspace.state == crate::state::WorkspaceState::Ready;
        Ok((ready && unchanged).then_some((workspace, guard)))
    }

    /// Advance the workspace's stack and remove it once its watched PRs are
    /// complete. The caller holds its PR gate and those of the workspaces
    /// stacked on it.
    async fn sweep_workspace_prs(&self, workspace: &Workspace) -> Result<()> {
        let mut registration = match self.pr_registration(&workspace.id).await {
            Ok(Some(registration)) => registration,
            Ok(None) => return Ok(()),
            Err(error) => {
                self.record_retained(&workspace.id, crate::daemon::events::EventCause::Pr, &error)
                    .await?;
                self.notify(
                    Some(&workspace.name),
                    NotificationKind::CleanupFailed,
                    format!("PR cleanup retained the workspace: {error:#}"),
                )
                .await;
                return Ok(());
            }
        };
        // A base stays until its stacked workspaces moved; a failure retries next sweep.
        let restacked = self.advance_stack(workspace, &registration.kind).await;
        if let Ok(Some(error)) = &restacked {
            registration.error = Some(format!("{STACK_ERROR}: {error:#}"));
            self.save_registration(&workspace.id, &registration).await?;
            return Ok(());
        }
        // A repository that disabled PR cleanup keeps its watches waiting;
        // unreadable config is recorded like a failed lookup.
        let settings = self.workspace_settings(workspace).await;
        if restacked.is_ok()
            && settings
                .as_ref()
                .is_ok_and(|settings| !settings.cleanup.pr.enabled)
        {
            // This sweep resolved any stored stack error; others stay.
            if registration
                .error
                .as_deref()
                .is_some_and(|error| error.starts_with(STACK_ERROR))
            {
                registration.error = None;
                self.save_registration(&workspace.id, &registration).await?;
            }
            return Ok(());
        }
        // `Ok(true)` once the workspace is removed; `Ok(false)` while the PR is open.
        let result: Result<bool> = async {
            restacked.context(STACK_ERROR)?;
            settings?;
            let verified = async {
                self.verify_worktree(workspace).await?;
                current_head(workspace).await
            };
            let head = self.check_access(&workspace.path, verified).await?;
            if !self
                .confirm_pr_completion(workspace, &mut registration.kind, &head)
                .await?
            {
                return Ok(false);
            }
            if self.keep_reason(&workspace.id).await?.is_some()
                || !self.completion_allows_cleanup(workspace).await?
            {
                return Ok(false);
            }
            if !self.remove_merged(&workspace.id, &head).await? {
                return Ok(false);
            }
            log!("PR cleanup removed {}", workspace.name);
            Ok(true)
        }
        .await;
        match &result {
            Ok(true) => {
                let cause = match registration.kind {
                    RegistrationKind::Watch { .. } => {
                        "removed after all watched pull requests merged"
                    }
                    RegistrationKind::Acknowledgement { .. } => {
                        "removed after the merge acknowledgement"
                    }
                };
                self.notify(
                    Some(&workspace.name),
                    NotificationKind::WorkspaceRemoved,
                    crate::daemon::workspace::removal_notice(&workspace.name, cause),
                )
                .await;
            }
            Ok(false) => {}
            Err(error) => {
                self.record_retained(&workspace.id, crate::daemon::events::EventCause::Pr, error)
                    .await?;
                self.notify(
                    Some(&workspace.name),
                    NotificationKind::CleanupFailed,
                    format!("PR cleanup retained the workspace: {error:#}"),
                )
                .await;
            }
        }
        registration.error = result.err().map(|error| format!("{error:#}"));
        self.save_registration(&workspace.id, &registration).await?;
        Ok(())
    }

    async fn save_registration(&self, id: &str, registration: &Registration) -> Result<()> {
        let (id, record) = (id.to_owned(), serde_json::to_string(registration)?);
        self.store
            .run(move |db| {
                db.execute(
                    "UPDATE pr_cleanup SET record=?2 WHERE space_id=?1",
                    rusqlite::params![id, record],
                )?;
                Ok(())
            })
            .await
    }

    async fn confirm_pr_completion(
        &self,
        workspace: &Workspace,
        kind: &mut RegistrationKind,
        head: &str,
    ) -> Result<bool> {
        match kind {
            RegistrationKind::Acknowledgement { head: acknowledged } => {
                ensure!(
                    acknowledged == head,
                    "HEAD changed after the merge acknowledgement; retaining workspace"
                );
            }
            RegistrationKind::Watch { urls, merged_head } => {
                if let Some(confirmed) = merged_head {
                    ensure!(
                        confirmed == head,
                        "HEAD changed after the watched PRs completed; retaining workspace"
                    );
                    return Ok(true);
                }
                // Only an explicit done is waited on; merged cleanup covers the rest.
                let Some(completion) = self.completion(&workspace.id).await? else {
                    return Ok(false);
                };
                if !self.all_prs_merged(workspace, urls, head).await? {
                    return Ok(false);
                }
                ensure!(
                    completion.head == head,
                    "HEAD changed after the assignment was marked done; retaining workspace"
                );
                // The latch prevents repeated forge queries.
                *merged_head = Some(head.to_owned());
            }
        }
        Ok(true)
    }

    async fn all_prs_merged(
        &self,
        workspace: &Workspace,
        urls: &[String],
        head: &str,
    ) -> Result<bool> {
        let mut contains_head = false;
        for url in urls {
            let (forge, number, _) = self.pr_forge(&workspace.path, url).await?;
            let Some(commits) = forge
                .merged_commits(&self.user_account(), number, &workspace.branch)
                .await?
            else {
                return Ok(false);
            };
            contains_head |= commits.iter().any(|commit| commit == head);
        }
        if contains_head {
            return Ok(true);
        }
        // A branch rebased after its PR merged has new commits for the same changes.
        let merged = self
            .default_branch_has_head(workspace)
            .await
            .context("merged PRs do not contain HEAD; could not check the default branch")?;
        ensure!(
            merged,
            "merged PRs do not contain HEAD and the default branch lacks its changes; retaining workspace"
        );
        Ok(true)
    }
}

/// A workspace's PR registration replacing its previous one.
struct RegistrationChange {
    registration: Option<Registration>,
    added: Vec<String>,
    removed: Vec<String>,
}

impl RegistrationChange {
    fn new(previous: Option<&Registration>, registration: Option<Registration>) -> Self {
        let before = watched_urls(previous);
        let after = watched_urls(registration.as_ref());
        let added = after
            .iter()
            .filter(|url| !before.contains(url))
            .cloned()
            .collect();
        let removed = before
            .iter()
            .filter(|url| !after.contains(url))
            .cloned()
            .collect();
        Self {
            registration,
            added,
            removed,
        }
    }

    /// Store the registration within the caller's transaction. A dropped watch
    /// takes its replay cursors and review mark with it, and every watch added
    /// or dropped is journaled as an assigned PR link change.
    fn apply(&self, db: &Connection, id: &str) -> Result<()> {
        super::forget_assigned(db, id, ItemKind::Pr, &self.removed)?;
        store_registration(db, id, self.registration.as_ref())?;
        for url in &self.added {
            events::record_link(
                db,
                id,
                EventKind::Linked,
                crate::model::Relation::Assigned,
                ItemKind::Pr,
                url,
            )?;
        }
        Ok(())
    }
}

fn watched_urls(registration: Option<&Registration>) -> &[String] {
    match registration {
        Some(Registration {
            kind: RegistrationKind::Watch { urls, .. },
            ..
        }) => urls,
        _ => &[],
    }
}

fn store_registration(
    db: &Connection,
    id: &str,
    registration: Option<&Registration>,
) -> Result<()> {
    match registration {
        Some(registration) => db.execute(
            "INSERT INTO pr_cleanup(space_id,record) VALUES (?1,?2)
             ON CONFLICT(space_id) DO UPDATE SET record=excluded.record",
            rusqlite::params![id, serde_json::to_string(registration)?],
        )?,
        None => db.execute("DELETE FROM pr_cleanup WHERE space_id=?1", [id])?,
    };
    Ok(())
}

pub(crate) async fn current_head(workspace: &Workspace) -> Result<String> {
    let (head, branch) = git::head_and_branch(&workspace.path)
        .await
        .context("cannot check the workspace's branch")?;
    ensure!(
        branch.as_deref() == Some(workspace.branch.as_str()),
        "workspace is not on its recorded branch"
    );
    Ok(head)
}

/// Look up each watched PR concurrently from the workspace's origin
/// `remote`, as read from its worktree; a failure stays on its PR.
pub(crate) async fn watched(
    hosts: &ForgeHosts<'_>,
    account: &Account,
    remote: &Result<Option<String>>,
    registration: Option<&Registration>,
) -> Vec<PrStatus> {
    let Some(Registration {
        kind: RegistrationKind::Watch { urls, .. },
        ..
    }) = registration
    else {
        return Vec::new();
    };
    let forge = async {
        let remote = origin(remote, "PR lookup needs an origin remote")?;
        ForgeRepo::resolve(remote, hosts).await
    }
    .await;
    match forge {
        Ok(forge) => {
            futures_util::future::join_all(urls.iter().map(|url| forge.pr_status(account, url)))
                .await
        }
        Err(error) => urls
            .iter()
            .map(|url| PrStatus::failed(url.clone(), &error))
            .collect(),
    }
}

/// The origin `remote` a worktree read gave, or why a lookup that needs it
/// fails, saying `missing` when the worktree has none.
pub(crate) fn origin<'a>(
    remote: &'a Result<Option<String>>,
    missing: &'static str,
) -> Result<&'a str> {
    match remote {
        Ok(remote) => remote.as_deref().context(missing),
        Err(error) => Err(anyhow::anyhow!("{error:#}")),
    }
}

/// Apply `change` to the PR registration of the chat `id` in a transaction
/// that finds the chat still recorded: one that ended has no registration
/// to change, and writing one would outlive it.
fn record_chat_change(db: &mut Connection, id: &str, change: RegistrationChange) -> Result<()> {
    let tx = db.transaction()?;
    ensure!(
        super::space::is_chat(&tx, id)?,
        "the chat ended before its PR links changed"
    );
    change.apply(&tx, id)?;
    tx.commit()?;
    Ok(())
}

#[cfg(test)]
mod tests;
