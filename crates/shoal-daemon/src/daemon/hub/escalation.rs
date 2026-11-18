//! An offline coordinator does not hold up the spaces below it: a question
//! or approval request of a space here whose coordinator runs on a machine
//! that has been offline for `[machine.hub] escalate_after_minutes` passes
//! to the next space above with an agent on a connected machine, or to the
//! user, as a chat whose agent stops passes its undecided requests to the
//! user.
use std::{collections::HashMap, time::Duration};

use anyhow::Result;
use rusqlite::{Transaction, params};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::{
    approvals::DecisionStatus,
    hub::{
        directory::Directory,
        routing::{Told, ToldAbout},
    },
    notifications::NotificationKind,
    questions::Question,
};

use crate::{
    daemon::{log, workspace::Manager},
    time::UnixTime,
};

/// How often a machine looks for what waits for an offline coordinator,
/// besides once its stored state settles after a change.
const SWEEP: Duration = Duration::from_secs(60);

/// Lets a burst of changes settle into one look.
const SETTLE: Duration = Duration::from_secs(1);

impl Manager {
    /// Pass on what waits too long for an offline coordinator, every
    /// [`SWEEP`] and after each change, until the daemon stops.
    pub(super) async fn escalate_waiting(&self) {
        let mut stored = self.store.watch_changes();
        let mut sweep = tokio::time::interval(SWEEP);
        loop {
            tokio::select! {
                _ = sweep.tick() => {}
                changed = stored.changed() => {
                    if changed.is_err() {
                        return;
                    }
                    tokio::time::sleep(SETTLE).await;
                    stored.borrow_and_update();
                }
            }
            if let Err(error) = self.escalate_once().await {
                log!("hub: could not pass on what waits for an offline coordinator: {error:#}");
            }
        }
    }

    async fn escalate_once(&self) -> Result<()> {
        let Some(settings) = self.hub_settings() else {
            return Ok(());
        };
        let after = Duration::from_secs(settings.escalate_after_minutes * 60);
        // Not in a hub, or cut off from it, a machine cannot tell who is
        // offline.
        let Ok(Some((_, directory))) = self.hub_directory().await else {
            return Ok(());
        };
        let offline = offline_since(&directory);
        if offline.is_empty() {
            return Ok(());
        }
        let now = UnixTime::now();
        let waited = |created: UnixTime, decider: &SpaceId| {
            (offline.get(decider))
                .is_some_and(|since| now.duration_since(created.max(*since)) >= after)
        };
        for request in self.approval_requests(None).await? {
            let pending = request.status == DecisionStatus::Pending;
            if pending && (request.decider.as_ref()).is_some_and(|d| waited(request.created_at, d))
            {
                self.pass_over_offline(&request).await?;
            }
        }
        self.store.write(forget_closed).await?;
        for question in self.open_questions().await? {
            let Some(coordinator) = self.remote_coordinator(&question.space_id).await else {
                continue;
            };
            if waited(question.created_at, &coordinator) && !self.escalated(&question).await? {
                self.pass_question_on(&question).await?;
            }
        }
        Ok(())
    }

    /// The coordinator of the space `id` when it runs on another machine:
    /// the nearest space above with an agent, when none here has one.
    async fn remote_coordinator(&self, id: &SpaceId) -> Option<SpaceId> {
        let owned = id.clone();
        let agents = self.tracked_agents().await;
        let here = (self.store)
            .run(move |db| crate::daemon::workspace::coordinator_of(db, &owned, &agents))
            .await;
        match here {
            Ok(None) => self.deciders_elsewhere(id, false).await.into_iter().next(),
            _ => None,
        }
    }

    /// Tell the next space above `question`'s space with an agent on a
    /// connected machine, or else the user, of the question, once.
    async fn pass_question_on(&self, question: &Question) -> Result<()> {
        let online = self.deciders_elsewhere(&question.space_id, true).await;
        match online.into_iter().next() {
            Some(above) => {
                let told = Told {
                    above,
                    about: Some(ToldAbout {
                        space: question.space_id.clone(),
                        name: question.space.clone(),
                    }),
                    kind: Some(NotificationKind::QuestionAsked),
                    message: question.notification(),
                    files: Vec::new(),
                };
                self.send_told(told).await?;
            }
            None => {
                let kind = NotificationKind::QuestionAsked;
                let (name, id) = (&question.space, &question.space_id);
                (self.notify_user_of_space(name, id, kind, question.notification())).await;
            }
        }
        let question = question.clone();
        self.store
            .write(move |tx| record_escalated(tx, &question))
            .await
    }

    /// Whether `question` went on past its offline coordinator already.
    async fn escalated(&self, question: &Question) -> Result<bool> {
        let question = question.clone();
        self.store
            .read(move |tx| {
                crate::daemon::store::exists(
                    tx,
                    "SELECT 1 FROM hub_escalated_questions
                        WHERE space_id=?1 AND number=?2 AND created_at=?3",
                    params![question.space_id, question.number.item, question.created_at],
                )
            })
            .await
    }
}

/// When the hub last saw the machine of each space on an offline machine.
fn offline_since(directory: &Directory) -> HashMap<SpaceId, UnixTime> {
    (directory.spaces.iter())
        .filter_map(|listed| {
            let machine = directory.machine(&listed.machine)?;
            let since = machine.last_seen.unwrap_or(machine.joined_at);
            (!machine.connected).then(|| (listed.space.id.clone(), since))
        })
        .collect()
}

fn record_escalated(tx: &Transaction<'_>, question: &Question) -> Result<()> {
    tx.execute(
        "INSERT OR IGNORE INTO hub_escalated_questions (space_id, number, created_at)
            VALUES (?1, ?2, ?3)",
        params![question.space_id, question.number.item, question.created_at],
    )?;
    Ok(())
}

/// Forget the escalations of questions that are no longer open.
fn forget_closed(tx: &Transaction<'_>) -> Result<()> {
    tx.execute(
        "DELETE FROM hub_escalated_questions WHERE NOT EXISTS (SELECT 1 FROM questions
            WHERE questions.space_id=hub_escalated_questions.space_id
                AND questions.number=hub_escalated_questions.number
                AND questions.created_at=hub_escalated_questions.created_at
                AND questions.closed_at IS NULL)",
        [],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests;
