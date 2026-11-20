//! Resolving a thread with a typed outcome, or reopening it, and telling
//! the other side.
use anyhow::{Context as _, Result, ensure};
use rusqlite::Transaction;
use shoal_protocol::threads::{
    Outcome, OutcomeKind, Post, Resolution, Sender, Side, Thread, ThreadNumber,
};

use super::{
    delivery::{self, Delivered, Party, Texts},
    records,
};
use crate::daemon::{manager::Manager, scope::Caller};

impl Manager {
    /// Resolve the thread `number` for `caller` as `outcome` with its
    /// data, or reopen it without an outcome, telling the other side.
    pub async fn resolve_thread(
        &self,
        caller: Option<&Caller>,
        number: ThreadNumber,
        outcome: Option<OutcomeKind>,
        choice: Option<u32>,
        of: Option<ThreadNumber>,
        reason: Option<String>,
    ) -> Result<Thread> {
        let thread = self.readable_thread(caller, number).await?;
        let side = self.ensure_resolves(caller, &thread).await?;
        let resolution = match outcome {
            None => {
                ensure!(
                    !thread.is_open(),
                    "thread {} is open; resolve it with an outcome",
                    thread.number
                );
                ensure!(
                    choice.is_none() && of.is_none() && reason.is_none(),
                    "--undo takes no outcome data"
                );
                None
            }
            Some(kind) => Some(
                self.resolution(caller, &thread, kind, choice, of, reason)
                    .await?,
            ),
        };
        let by = match caller {
            Some(caller) => self.speaker(caller).await?.1.to_string(),
            None => "the user".to_owned(),
        };
        let (id, removing) = (thread.id, self.space_removals.clone());
        let (thread, delivered) = self
            .store
            .write(move |tx| {
                records::set_resolution(tx, id, resolution.as_ref())?;
                let thread = records::reread(tx, id)?;
                // A reopened thread that waits keeps the space that opened
                // it, which a space reserved for removal takes no more.
                if thread.is_waiting() && thread.opener == Sender::Space {
                    removing.require_taking_work(tx, &thread.space_id)?;
                }
                let delivered = tell_resolution(tx, &thread, side, &by)?;
                Ok((thread, delivered))
            })
            .await?;
        self.announce_threads(delivered).await;
        Ok(thread)
    }

    /// The resolution `kind` with its data that `caller` gives `thread`.
    async fn resolution(
        &self,
        caller: Option<&Caller>,
        thread: &Thread,
        kind: OutcomeKind,
        choice: Option<u32>,
        of: Option<ThreadNumber>,
        reason: Option<String>,
    ) -> Result<Resolution> {
        let of = match of {
            Some(of) => {
                let of = self.readable_thread(caller, of).await?;
                ensure!(of.id != thread.id, "a thread is no duplicate of itself");
                Some(of.number)
            }
            None => None,
        };
        if let Some(reason) = &reason {
            crate::validate::message(reason).context("invalid reason")?;
        }
        let outcome = Outcome::new(thread.kind, kind, choice, of, reason)?;
        if let Outcome::Answered {
            choice: Some(choice),
        } = outcome
        {
            let offered = thread.choices().len();
            ensure!(
                usize::try_from(choice).is_ok_and(|choice| (1..=offered).contains(&choice)),
                "choose a number from 1 to {offered}"
            );
        }
        Ok(Resolution {
            outcome,
            by: match caller {
                Some(_) => Sender::Space,
                None => Sender::User,
            },
            by_space: caller.map(|caller| caller.space_id.clone()),
            resolved_at: crate::time::UnixTime::now(),
        })
    }
}

/// Tell the side of `thread` across from `side`, which resolved or
/// reopened it as `by`, unless it heard nothing of it yet. A withdrawal
/// takes back that side's unread alerts instead, and tells it only when
/// it read one already.
fn tell_resolution(
    tx: &Transaction<'_>,
    thread: &Thread,
    side: Side,
    by: &str,
) -> Result<Delivered> {
    let mut delivered = Delivered::default();
    let other = match side {
        Side::Opener => Side::Holder,
        Side::Holder => Side::Opener,
    };
    let Some(party) = Party::of(thread, other) else {
        return Ok(delivered);
    };
    if !thread.posts.iter().any(Post::speaks) {
        return Ok(delivered);
    }
    let withdrawn = matches!(
        thread
            .resolution
            .as_ref()
            .map(|resolution| resolution.outcome.kind()),
        Some(OutcomeKind::Withdrawn)
    );
    if withdrawn && delivery::retract(tx, thread, &party)? {
        return Ok(delivered);
    }
    let texts = Texts::resolved(thread, by);
    delivery::deliver(tx, thread, None, &party, texts, &mut delivered)?;
    Ok(delivered)
}
