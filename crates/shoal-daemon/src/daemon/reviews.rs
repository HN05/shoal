//! What reviews need of the rest of the daemon, which [`Manager`] gives
//! them: the workspaces they review, their repositories and settings, the
//! agent queue's admission of their runs, the agents and coordinators that
//! hear of them and the forge a PR's review is posted on.
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::PathBuf;

use anyhow::Result;
use futures_util::future::BoxFuture;
use shoal_ids::repository_ids::RepositoryId;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::repository::Selector;
use shoal_protocol::space_names::{SpaceName, SpaceSelector};
use shoal_protocol::{
    item_numbers::ItemNumber,
    reviews::ReviewId,
    threads::{Thread, ThreadSpace},
};
use shoal_refs::CommitId;
use shoal_reviews::daemon::{
    Admission, Agents, AwaitedPost, FindingThread, Forge, Records, Repositories, Settings, Spaces,
    Threads,
};

use super::{
    agent_queue::{self, OneShotAdmission, Serving},
    alerts::{About, AlertKind, NewAlert},
    manager::Manager,
    notifications::Source,
};
use crate::{
    config::Effective,
    forge::{
        ForgeRepo,
        items::{action::Action, link::ItemKind, reference::ItemRef, review::NewReview},
    },
    model::{Repository, Workspace},
    protocol::ItemContext,
};
use shoal_forge::transport::account::Account;

impl Spaces for Manager {
    fn workspace<'a>(&'a self, selector: &'a SpaceSelector) -> BoxFuture<'a, Result<Workspace>> {
        Box::pin(Manager::workspace(self, selector))
    }

    fn workspace_by_id<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<Workspace>> {
        Box::pin(Manager::workspace_by_id(self, id))
    }

    fn verify_worktree<'a>(&'a self, workspace: &'a Workspace) -> BoxFuture<'a, Result<()>> {
        Box::pin(Manager::verify_worktree(self, workspace))
    }

    fn head<'a>(&'a self, workspace: &'a Workspace) -> BoxFuture<'a, Result<CommitId>> {
        Box::pin(shoal_links::pr::current_head(workspace))
    }

    fn diff_base<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<CommitId>> {
        Box::pin(async move {
            let selector = SpaceSelector::id(id);
            Ok(Manager::diff_base(self, &selector).await?.commit)
        })
    }
}

impl Repositories for Manager {
    fn repository<'a>(&'a self, id: &'a RepositoryId) -> BoxFuture<'a, Result<Repository>> {
        Box::pin(self.repositories.repository(id))
    }

    fn select_repository(&self, selector: Selector) -> BoxFuture<'_, Result<Repository>> {
        Box::pin(self.repositories.repository(selector))
    }

    fn repositories(&self) -> BoxFuture<'_, Result<Vec<Repository>>> {
        Box::pin(self.repositories.list())
    }

    fn false_positives_path<'a>(
        &'a self,
        id: &'a RepositoryId,
    ) -> BoxFuture<'a, Result<Option<PathBuf>>> {
        Box::pin(self.repositories.false_positives_path(id))
    }

    fn writable_false_positives_path<'a>(
        &'a self,
        id: &'a RepositoryId,
    ) -> BoxFuture<'a, Result<PathBuf>> {
        Box::pin(self.repositories.writable_false_positives_path(id))
    }
}

impl Settings for Manager {
    fn settings<'a>(&'a self, workspace: &'a Workspace) -> BoxFuture<'a, Result<Effective>> {
        Box::pin(self.repositories.workspace_settings(self, workspace))
    }

    fn settings_with_templates<'a>(
        &'a self,
        workspace: &'a Workspace,
    ) -> BoxFuture<'a, Result<Effective>> {
        Box::pin(
            self.repositories
                .workspace_settings_with_templates(self, workspace),
        )
    }

    fn repository_settings<'a>(&'a self, id: &'a RepositoryId) -> BoxFuture<'a, Result<Effective>> {
        Box::pin(self.repositories.space_settings_in(Some(id)))
    }

    fn environment<'a>(
        &'a self,
        workspace: &'a Workspace,
        settings: &'a Effective,
    ) -> BoxFuture<'a, Result<BTreeMap<String, OsString>>> {
        Box::pin(self.hook_environment(workspace, Some(workspace.repository()), settings))
    }
}

impl Agents for Manager {
    fn admit<'a>(
        &'a self,
        repository: &'a RepositoryId,
        space: &'a SpaceId,
        after_stop: bool,
        stop: Box<dyn FnOnce() + Send + Sync>,
    ) -> BoxFuture<'a, Result<Box<dyn Admission + 'a>>> {
        Box::pin(async move {
            let queued_after = self.store.read(|tx| agent_queue::last_seq(tx)).await?;
            let serving = Serving {
                space: space.clone(),
                queued_after,
            };
            let admission =
                (self.agents.agent_queue()).request_oneshot(repository, serving, after_stop, stop);
            Ok(Box::new(QueueAdmission(admission)) as Box<dyn Admission>)
        })
    }

    fn hold_admission(&self, repository: &RepositoryId) {
        self.agents.agent_queue().hold_oneshot(repository);
    }

    fn message<'a>(
        &'a self,
        id: &'a SpaceId,
        kind: AlertKind,
        message: String,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(self.agents.queue_agent_message(id, kind, message))
    }

    fn tell_coordinator<'a>(
        &'a self,
        name: &'a SpaceName,
        id: &'a SpaceId,
        kind: AlertKind,
        message: String,
    ) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            let about = About::space(name, id);
            let alert = NewAlert::shoal(kind, about, message);
            (self.coordination)
                .tell_coordinator(self, alert, false, Source::Daemon)
                .await;
        })
    }
}

impl Threads for Manager {
    fn review_threads_changed<'a>(&'a self, workspace: &'a Workspace) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            self.signals.thread_changed();
            let spaces = [workspace.id.clone()];
            self.coordination
                .keep_code_versions(self, workspace.repository(), spaces)
                .await;
        })
    }

    fn answer_finding(&self, thread: i64, text: String) -> BoxFuture<'_, Result<()>> {
        Box::pin(self.coordination.answer_finding(self, thread, text))
    }
}

/// A one-shot run's admission in the daemon's agent queue.
struct QueueAdmission<'a>(OneShotAdmission<'a>);

impl Admission for QueueAdmission<'_> {
    fn admitted(&mut self) -> BoxFuture<'_, ()> {
        Box::pin(self.0.admitted())
    }

    fn keep(self: Box<Self>) {
        self.0.keep();
    }
}

impl Forge for Manager {
    fn forge_account<'a>(
        &'a self,
        workspace: &'a Workspace,
    ) -> BoxFuture<'a, Result<(ForgeRepo, Account)>> {
        Box::pin(self.links.workspace_forge_account(self, workspace))
    }

    fn checkout_forge<'a>(
        &'a self,
        repository: &'a Repository,
    ) -> BoxFuture<'a, Result<ForgeRepo>> {
        Box::pin(shoal_links::forges::of_checkout(self, repository))
    }

    fn post<'a>(
        &'a self,
        workspace: &'a Workspace,
        pull: &'a ItemRef,
        review: NewReview,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let target = ItemContext::space(SpaceSelector::id(&workspace.id)).into();
            let pull = Some(pull.clone().into());
            let action = Action::Review(review);
            (self
                .links
                .item_action(self, &target, ItemKind::Pr, pull, action, None))
            .await?;
            Ok(())
        })
    }
}

/// The spaces' records and their findings' threads, as reviews read and
/// write them in their own connections and transactions.
pub(crate) struct ReviewRecords;

impl Records for ReviewRecords {
    fn space_name(&self, db: &rusqlite::Connection, id: &SpaceId) -> Result<Option<SpaceName>> {
        super::spaces::space_name(db, id)
    }

    fn thread_space(&self, db: &rusqlite::Connection, id: &SpaceId) -> Result<ThreadSpace> {
        super::threads::thread_space(db, id)
    }

    fn workspaces(
        &self,
        db: &rusqlite::Connection,
        repository: Option<&RepositoryId>,
    ) -> Result<Vec<SpaceId>> {
        super::spaces::workspace_ids(db, repository)
    }

    fn open_finding(
        &self,
        tx: &rusqlite::Transaction<'_>,
        finding: &FindingThread<'_>,
    ) -> Result<()> {
        let code = &finding.code;
        let finding = super::threads::findings::FindingThread {
            review: finding.review,
            workspace: finding.workspace,
            severity: finding.severity,
            title: finding.title,
            body: finding.body,
            code: super::threads::findings::FindingCode {
                commit: &code.commit,
                at: &code.at,
                snippet: code.snippet.as_deref(),
            },
        };
        super::threads::findings::open(tx, &finding)
    }

    fn reopen_finding(
        &self,
        tx: &rusqlite::Transaction<'_>,
        workspace: &SpaceId,
        number: ItemNumber,
        why: &str,
    ) -> Result<Option<i64>> {
        super::threads::findings::reopen(tx, workspace, number, why)
    }

    fn drop_review_threads(&self, tx: &rusqlite::Transaction<'_>, review: ReviewId) -> Result<()> {
        super::threads::findings::drop_of_review(tx, review)
    }

    fn review_threads(&self, db: &rusqlite::Connection, review: ReviewId) -> Result<Vec<Thread>> {
        super::threads::findings::of_review(db, review)
    }

    fn threads(&self, db: &rusqlite::Connection, ids: &[i64]) -> Result<Vec<Thread>> {
        super::threads::findings::by_ids(db, ids)
    }

    fn awaiting_answers(&self, db: &rusqlite::Connection) -> Result<Vec<AwaitedPost>> {
        let awaited = super::threads::findings::awaiting_answers(db)?;
        Ok((awaited.into_iter())
            .map(|awaited| AwaitedPost {
                thread: awaited.thread,
                post: awaited.post,
            })
            .collect())
    }
}

#[cfg(test)]
mod tests;
