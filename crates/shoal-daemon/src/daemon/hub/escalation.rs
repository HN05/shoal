//! An offline coordinator does not hold up the spaces below it: a question
//! or approval request of a space here whose coordinator runs on a machine
//! that has been offline for `[machine.hub] escalate_after_minutes` passes
//! to the next space above with an agent on a connected machine, or to the
//! user. An approval request whose decider elsewhere lost its agent or is
//! gone goes to the user at once, as a chat whose agent stops here passes
//! its undecided requests to the user.
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
        let Ok(Some((own, directory))) = self.hub_directory().await else {
            return Ok(());
        };
        let now = UnixTime::now();
        let deciders = Deciders {
            own: &own,
            directory: &directory,
            now,
            after,
        };
        for request in self.approval_requests(None).await? {
            let Some(decider) = request.decider.as_ref() else {
                continue;
            };
            // This machine's own rules cover a decider here, which the hub
            // may not list yet.
            if request.status != DecisionStatus::Pending
                || self.find_space(decider).await?.is_some()
            {
                continue;
            }
            match deciders.unavailable(decider, request.created_at) {
                Some(Unavailable::Offline) => self.pass_over_offline(&request).await?,
                Some(Unavailable::Gone) => self.pass_to_user(&request).await?,
                None => {}
            }
        }
        let offline = offline_since(&directory);
        let waited = |created: UnixTime, decider: &SpaceId| {
            (offline.get(decider))
                .is_some_and(|since| now.duration_since(created.max(*since)) >= after)
        };
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
            .run(move |db| crate::daemon::tree::coordinator_of(db, &owned, &agents))
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

/// What keeps a request's decider on another machine from deciding it.
#[derive(Debug, PartialEq, Eq)]
enum Unavailable {
    /// Its machine has been offline for too long; the next space above with
    /// an agent on a connected machine decides it.
    Offline,
    /// It no longer has an agent, or is gone, which gives its undecided
    /// requests to the user, as a chat's agent stopping here does.
    Gone,
}

/// The hub's directory as this machine, `own`, sees deciders in it.
struct Deciders<'a> {
    own: &'a shoal_ids::member_ids::MemberId,
    directory: &'a Directory,
    now: UnixTime,
    after: Duration,
}

impl Deciders<'_> {
    /// What keeps `decider`, a space elsewhere that a request made at
    /// `created` waits for, from deciding it; none for one that can.
    fn unavailable(&self, decider: &SpaceId, created: UnixTime) -> Option<Unavailable> {
        let listed = (self.directory.spaces.iter()).find(|listed| &listed.space.id == decider);
        let Some(listed) = listed else {
            return Some(Unavailable::Gone);
        };
        if &listed.machine == self.own {
            return None;
        }
        if !listed.space.agent {
            return Some(Unavailable::Gone);
        }
        let machine = self.directory.machine(&listed.machine)?;
        if machine.connected {
            return None;
        }
        let since = machine.last_seen.unwrap_or(machine.joined_at);
        (self.now.duration_since(created.max(since)) >= self.after).then_some(Unavailable::Offline)
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
