//! Recording a space's completion: `shoal done` and its withdrawal.
use crate::Spaces;
use crate::daemon::Daemon;
use crate::model::EventCause;
use crate::opened_elsewhere::Followed;
use anyhow::{Context, Result, ensure};
use rusqlite::OptionalExtension;
use shoal_ids::space_ids::SpaceId;
use std::path::{Path, PathBuf};

use crate::{
    config::Effective,
    hooks::{Hook, HookKind},
    model::{Completion, HandedBack, Space},
    {
        gates::{GuardMode, ResourceGuard},
        item_numbers::ItemNumber,
        notifications::AlertKind,
        progress,
        store::require_ready_space,
        threads,
        workspace_files::Attacher,
    },
};
use shoal_protocol::{assignments::Done, space_names::SpaceSelector, threads::CloseCause};
use shoal_refs::CommitId;

impl Spaces {
    /// Stop the space's demos unless `done` keeps them, then record `done`
    /// for the space `selector` names, at the HEAD of where its agent works.
    /// Copies of the files it hands back are attached by `attacher`, and the
    /// space's attached files keep them after its removal.
    pub async fn mark_space_done(
        &self,
        daemon: &dyn Daemon,
        selector: &SpaceSelector,
        done: Done,
        attacher: &Attacher,
    ) -> Result<Completion> {
        done.check()?;
        // Before completion, whose cleanup may remove the space at once.
        if !done.keep_demos {
            daemon.stop_demos(selector, None).await?;
        }
        // Serialize cleanup choices with the PR and completion sweeps.
        let (space, _guard) = self.lock_space_gate(selector).await?;
        let head = self
            .completion_head(daemon, &space, done.integrated)
            .await?;
        let results = (self.keep_results(daemon, &space, done.results, attacher)).await?;
        let completion = Completion {
            head,
            cleanup: done.cleanup,
            integrated: done.integrated,
            results,
            summary: done.summary,
            error: None,
        };
        self.record_done(daemon, &space, completion).await
    }

    /// Withdraw the completion the space `selector` names recorded, and
    /// reopen the threads it closed; only holds and threads that wait keep
    /// the space.
    pub async fn undo_space_done(
        &self,
        daemon: &dyn Daemon,
        selector: &SpaceSelector,
    ) -> Result<Completion> {
        let (space, _guard) = self.lock_space_gate(selector).await?;
        self.verify_space(daemon, &space).await?;
        let id = space.id.clone();
        let records = self.records.clone();
        let (completion, delivered) = self
            .store
            .write(move |tx| {
                require_ready_space(tx, &id)?;
                let completion =
                    take_completion(tx, &id)?.context("the space is not marked done")?;
                let (_, delivered) =
                    threads::reopen(tx, records.coordination(), &id, CloseCause::Completion)?;
                Ok((completion, delivered))
            })
            .await?;
        daemon
            .coordination()
            .announce_threads(daemon, delivered)
            .await;
        let reopened = Followed::Reopened(CloseCause::Completion);
        self.tell_other_machines(daemon, &space.id, reopened).await;
        self.signals.wake_cleanup();
        daemon.settle_agents(&space.id);
        Ok(completion)
    }

    /// The completion the space `id` recorded, a workspace's or a chat's.
    pub async fn completion(&self, id: &SpaceId) -> Result<Option<Completion>> {
        let id = id.to_owned();
        self.store.run(move |db| recorded(db, &id)).await
    }

    /// Record that another branch has the work of the space `id` at `head`
    /// under other commits, as `shoal done --integrated` records it,
    /// keeping what a completion it recorded says otherwise.
    pub async fn record_integrated(&self, id: &SpaceId, head: CommitId) -> Result<()> {
        let id = id.to_owned();
        self.store
            .write(move |tx| {
                let completion = match take_completion(tx, &id)? {
                    Some(recorded) => Completion {
                        head,
                        integrated: true,
                        ..recorded
                    },
                    None => Completion {
                        head,
                        cleanup: false,
                        integrated: true,
                        results: Vec::new(),
                        summary: None,
                        error: None,
                    },
                };
                record_completion(tx, &id, &completion).map(drop)
            })
            .await
    }

    /// `shoal resume` restores a stopped agent of the space, which says its
    /// work is not finished: withdraw a standing completion, so a protection
    /// stop restores the agent and completion cleanup leaves the space.
    pub async fn reopen_for_resume(&self, daemon: &dyn Daemon, space_id: &SpaceId) {
        match self.completion(space_id).await {
            Ok(Some(_)) => {}
            Ok(None) => return,
            Err(error) => {
                crate::log!("completion of the resumed agent not read: {error:#}");
                return;
            }
        }
        if let Err(error) = self
            .undo_space_done(daemon, &SpaceSelector::id(space_id))
            .await
        {
            crate::log!("completion of the resumed agent not withdrawn: {error:#}");
        }
    }

    /// The HEAD a completion of `space` records, of where its agent works:
    /// a workspace's verified worktree, or the worktree or checkout a chat
    /// runs in. A chat without a repository records an empty one, since its
    /// `root_dir` is no repository. Only a workspace is `integrated`: that
    /// records that another branch has its commits.
    async fn completion_head(
        &self,
        daemon: &dyn Daemon,
        space: &Space,
        integrated: bool,
    ) -> Result<CommitId> {
        if let Some(workspace) = space.to_workspace() {
            self.verify_worktree(daemon, &workspace).await?;
            return daemon.current_head(&workspace).await;
        }
        ensure!(
            !integrated,
            "{space} has no branch of its own whose commits another branch could have"
        );
        if space.workdir.root_dir().is_some() {
            return Ok(String::new().into());
        }
        let (head, _) = (crate::git::head_and_branch(space.workdir.path()).await)
            .context("cannot read the HEAD where the space works")?;
        Ok(head)
    }

    /// Record `completion`, with cleanup also when the space's settings
    /// request it. Caller holds the space's PR gate and has verified
    /// ownership and the completion's HEAD.
    async fn record_done(
        &self,
        daemon: &dyn Daemon,
        space: &Space,
        completion: Completion,
    ) -> Result<Completion> {
        let settings = daemon.space_settings(space).await?;
        let hook = self.done_hook(space, &settings).await?;
        let completion = Completion {
            cleanup: completion.cleanup || settings.cleanup.done.enabled,
            ..completion
        };
        let persisted = self
            .persist_completion(daemon, &space.id, &completion)
            .await?;
        // Rerunning the hook is the one effect a repeated done may have left.
        ensure!(
            persisted.changed || hook.is_some(),
            "the space is already marked done at this HEAD"
        );
        if persisted.progressed {
            self.publications.request(&space.id);
        }
        daemon.settle_agents(&space.id);
        let closed = persisted.closed;
        daemon.forget_last_sessions(&space.id);
        // Finished work needs no agent; only naming the space in `shoal
        // restore` restores it now.
        if let Err(error) = daemon.cancel_stopped_restores(&space.id).await {
            crate::log!("agent queue: {error:#}");
        }
        daemon
            .coordination()
            .notify_space(
                daemon,
                &space.name,
                &space.id,
                AlertKind::SpaceDone,
                done_notice(&space.number.to_string(), &completion, &closed),
            )
            .await;
        if let Some((command, _resources)) = hook {
            (self.run_done_hook(daemon, space, &settings, &command, &completion)).await;
        }
        self.signals.wake_cleanup();
        Ok(completion)
    }

    /// Record `completion` of the space `space`, with its summary as the
    /// status line, and close its open threads, here and on the other
    /// machines, which its finished work waits on no longer, telling their
    /// openers.
    async fn persist_completion(
        &self,
        daemon: &dyn Daemon,
        space: &SpaceId,
        completion: &Completion,
    ) -> Result<Persisted> {
        let id = space.to_owned();
        let completion = completion.clone();
        let now = crate::time::UnixTime::now();
        let records = self.records.clone();
        let (persisted, delivered) = self
            .store
            .write(move |tx| {
                require_ready_space(tx, &id)?;
                let replaced = record_completion(tx, &id, &completion)?;
                // Completion removes what blocked the space.
                let progressed =
                    progress::record_status(tx, &id, completion.summary.as_deref(), now)?
                        | progress::remove_blockers(tx, &id, now)?;
                let (closed, delivered) =
                    threads::close(tx, records.coordination(), &id, CloseCause::Completion)?;
                // Finished work leaves its failed turn alone, and has
                // nothing left for a timer to wake it for.
                records.cancel_wake(tx, &id)?;
                let persisted = Persisted {
                    changed: replaced || !closed.is_empty() || progressed,
                    closed,
                    progressed,
                };
                Ok((persisted, delivered))
            })
            .await?;
        daemon
            .coordination()
            .announce_threads(daemon, delivered)
            .await;
        let closed = Followed::Closed(CloseCause::Completion);
        self.tell_other_machines(daemon, space, closed).await;
        Ok(persisted)
    }

    /// The configured `hooks.post_done`, in where the space's agent works,
    /// holding off permit and lifecycle changes until it has run.
    async fn done_hook(
        &self,
        space: &Space,
        settings: &Effective,
    ) -> Result<Option<(PathBuf, ResourceGuard)>> {
        let Some(command) = Self::space_hook(space, HookKind::PostDone, settings) else {
            return Ok(None);
        };
        let guard = (self.gates).resource_guard(&space.id, GuardMode::Exclusive)?;
        Ok(Some((command, guard)))
    }

    /// Run `hooks.post_done` in `space`. Failure only notifies; it never
    /// undoes completion.
    async fn run_done_hook(
        &self,
        daemon: &dyn Daemon,
        space: &Space,
        settings: &Effective,
        command: &Path,
        completion: &Completion,
    ) {
        let hook = Hook::PostDone(completion);
        if let Err(error) = (self.run_space_hook(daemon, hook, space, settings, command)).await {
            let kind = AlertKind::HookFailed;
            let message = format!("finished; {error:#}");
            (daemon
                .coordination()
                .notify_space(daemon, &space.name, &space.id, kind, message))
            .await;
        }
    }
}

/// What recording a completion changed.
struct Persisted {
    /// Whether it replaced another completion, released a hold or closed a
    /// thread.
    changed: bool,
    /// The numbers of the threads it closed.
    closed: Vec<ItemNumber>,
    /// Whether it changed the status line.
    progressed: bool,
}

/// The completion the space `id` recorded.
pub fn recorded(db: &rusqlite::Connection, id: &SpaceId) -> Result<Option<Completion>> {
    let record: Option<String> = db
        .query_row(
            "SELECT record FROM space_completion WHERE space_id=?1",
            [id],
            |row| row.get(0),
        )
        .optional()?;
    record
        .map(|record| serde_json::from_str(&record).map_err(Into::into))
        .transpose()
}

/// Record `completion` for the space `id`, returning whether it differs from
/// the recorded one in any field, or that record is unreadable.
pub fn record_completion(
    tx: &rusqlite::Transaction,
    id: &SpaceId,
    completion: &Completion,
) -> Result<bool> {
    let recorded: Option<String> = tx
        .query_row(
            "SELECT record FROM space_completion WHERE space_id=?1",
            [id],
            |row| row.get(0),
        )
        .optional()?;
    let recorded = recorded.and_then(|record| serde_json::from_str::<Completion>(&record).ok());
    tx.execute(
        "INSERT INTO space_completion(space_id,record,cause) VALUES (?1,?2,?3)
         ON CONFLICT(space_id) DO UPDATE SET record=excluded.record,cause=excluded.cause",
        rusqlite::params![id, serde_json::to_string(completion)?, EventCause::Manual],
    )?;
    Ok(recorded.as_ref() != Some(completion))
}

/// Delete and return the completion the space `id` recorded, or `None`
/// without one. An unreadable record stays in place for diagnosis.
pub fn take_completion(tx: &rusqlite::Transaction, id: &SpaceId) -> Result<Option<Completion>> {
    let record: Option<String> = tx
        .query_row(
            "DELETE FROM space_completion WHERE space_id=?1 RETURNING record",
            [id],
            |row| row.get(0),
        )
        .optional()?;
    Ok(record
        .map(|record| serde_json::from_str(&record))
        .transpose()?)
}

/// What the user is told about a completion, the threads it closed and
/// the files it handed back, in the space numbered `space`.
pub fn done_notice(space: &str, completion: &Completion, closed: &[ItemNumber]) -> String {
    let mut notice = String::from("finished");
    if completion.integrated {
        notice.push_str("; integrated elsewhere");
    }
    notice.push_str(if completion.cleanup {
        "; cleanup requested"
    } else {
        "; no cleanup requested"
    });
    push_completion_details(&mut notice, space, completion, closed);
    notice
}

/// Add the threads a completion closed, the results it handed back and,
/// last, its summary to the notice of the space `space`, a number, which
/// selects it.
fn push_completion_details(
    notice: &mut String,
    space: &str,
    completion: &Completion,
    closed: &[ItemNumber],
) {
    if let [number] = closed {
        notice.push_str(&format!("; closed thread {number}"));
    } else if !closed.is_empty() {
        let numbers: Vec<_> = closed.iter().map(ToString::to_string).collect();
        notice.push_str(&format!("; closed threads {}", numbers.join(", ")));
    }
    if !completion.results.is_empty() {
        notice.push_str(&format!(
            "; results in shoal files {space}: {}",
            results_preview(&completion.results)
        ));
    }
    if let Some(summary) = &completion.summary {
        notice.push_str(&format!("; summary: {summary}"));
    }
}

/// The results a notice names: the first few, then how many more, since
/// `shoal files` lists them all.
pub fn results_preview(results: &[HandedBack]) -> String {
    const NAMED: usize = 3;
    let names: Vec<_> = results
        .iter()
        .take(NAMED)
        .map(|result| result.name.as_str())
        .collect();
    match results.len().saturating_sub(NAMED) {
        0 => names.join(", "),
        more => format!("{} and {more} more", names.join(", ")),
    }
}

#[cfg(test)]
mod tests;
