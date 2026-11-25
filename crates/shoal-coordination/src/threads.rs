//! Threads: conversations between whoever opened one, the user, a space or
//! Shoal, and whoever holds it now, the user or a space. A space opens a
//! thread with its coordinator, else the user, or with a space below it;
//! the user opens one with a space. A thread is numbered in its opener's
//! space, or in the space the user opened it with, sharing that space's
//! numbers with approval requests, and keeps its number until that space
//! goes. Posts from either side reach the other as alerts, and the spaces
//! that linked the thread; the holder forwards it, and the opener resolves
//! it with a typed outcome. A thread its opener opens towards a workspace
//! it has a pending review of joins that review and holds its posts until
//! the review is submitted; the workspace's machine keeps the review and
//! numbers such a comment in the workspace when the opener runs elsewhere.
use crate::{
    Coordination,
    daemon::{Daemon, Records},
};
use anyhow::{Context as _, Result, bail, ensure};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::{
    hub::Member,
    model::{Located, SpaceKind},
    protocol::{Body, Operation, ThreadMethod},
    reviews::{ReviewId, Severity},
    space_names::SpaceSelector,
    threads::{Finding, Holder, Side, Thread, ThreadKind, ThreadOpening, ThreadSpace},
};

use crate::{alerts::Recipients, notifications::Source, scope::Caller};
use posting::Opening;
use records::{NewThread, Opener};

mod chat;
mod closing;
mod code;
mod delivery;
pub mod findings;
mod forwarding;
mod moving;
mod parties;
pub mod posting;
mod reading;
mod records;
mod resolving;
mod sent_files;

pub use chat::{number_of, record_replies as record_chat_replies};
pub use closing::{close, reopen};
pub use code::{Followed, canonical, relative};
pub use delivery::Delivered as ThreadsDelivered;
pub use reading::{newest_waiting_for_user, waiting_count, waiting_on_spaces};
pub use records::{renumber_alert, thread_space};

impl Coordination {
    /// Open a thread of `kind` named `name` from `caller`, or from the
    /// user without one, with the space `to` names, else with the caller's
    /// coordinator or the user. Without a kind, it is a `review` thread of
    /// the opener's pending review of its holder, which holds its posts,
    /// else a question; a review of a workspace on another machine is kept
    /// there, which numbers its comments. An empty one notifies no one
    /// until its first post; one with a body or code gets its opening post
    /// at once, which carries the code.
    pub async fn open_thread(
        &self,
        daemon: &dyn Daemon,
        caller: Option<&Caller>,
        opening: ThreadOpening,
        kind: Option<ThreadKind>,
        to: Option<&SpaceSelector>,
    ) -> Result<Thread> {
        validate(&opening)?;
        ensure!(
            kind != Some(ThreadKind::Review),
            "a review thread joins a pending review: start one with shoal review start <space>, \
             then open threads towards that space without --kind"
        );
        let opened = match (caller, to) {
            // Numbered in the space it is opened with, on that space's
            // machine, which takes a request for a space this one lacks.
            (None, Some(to)) => {
                let target = ThreadSpace::of(&daemon.space(to).await?);
                Opened::by(None, target.id.clone(), Some(target))
            }
            (None, None) => bail!("the user opens a thread with a space; name it with --space"),
            // Numbered in the opener's space here, and held by a space here
            // or on another machine, unless it comments the opener's review
            // kept there.
            (Some(caller), Some(to)) => {
                let located = daemon.space_anywhere(to).await?;
                ensure!(
                    *located.id() != caller.space_id,
                    "a space opens a thread with another space"
                );
                if kind.is_none()
                    && let Some(thread) =
                        comment_elsewhere(daemon, caller, &located, &opening).await?
                {
                    return Ok(thread);
                }
                let target = ThreadSpace::located(located);
                Opened::by(Some(caller), caller.space_id.clone(), Some(target))
            }
            (Some(caller), None) => {
                let holder = self.coordinator_of(daemon, &caller.space_id).await?;
                Opened::by(Some(caller), caller.space_id.clone(), holder)
            }
        };
        let opened = Opened { kind, ..opened };
        let thread = self.record_opened(daemon, caller, opened, opening).await?;
        thread.context("a thread that joins no review is a question")
    }

    /// Open a comment of the pending review `caller`, a space of another
    /// machine, writes of the workspace `workspace` names, which this
    /// machine keeps, numbered in that workspace, with `opening`; none
    /// without such a review, for the caller's machine to open as a
    /// question.
    pub async fn open_comment(
        &self,
        daemon: &dyn Daemon,
        caller: &Caller,
        workspace: &SpaceSelector,
        opening: ThreadOpening,
    ) -> Result<Option<Thread>> {
        validate(&opening)?;
        let holder = ThreadSpace::of(&daemon.space(workspace).await?);
        let opened = Opened {
            opener: Some(self.speaker(daemon, caller).await?),
            only_comment: true,
            ..Opened::by(None, holder.id.clone(), Some(holder))
        };
        self.record_opened(daemon, Some(caller), opened, opening)
            .await
    }

    /// Record `opened`, which `caller` opens with `opening`; none for a
    /// comment that joins no review.
    async fn record_opened(
        &self,
        daemon: &dyn Daemon,
        caller: Option<&Caller>,
        opened: Opened,
        opening: ThreadOpening,
    ) -> Result<Option<Thread>> {
        let outside = self.records.clone();
        let (name, severity) = (opening.name.clone(), opening.severity);
        let opening = self.opening_post(daemon, caller, opening).await?;
        let versioned = opening
            .as_ref()
            .map(|opening| code::versioned(&opening.code));
        let recorded = self
            .store
            .write(move |tx| {
                // A space reserved for removal takes no thread its cleanup
                // checks have not seen.
                outside.require_taking_work(tx, &opened.space_id)?;
                let review = opened.joined_review(tx, &*outside)?;
                if review.is_none() && opened.only_comment {
                    return Ok(None);
                }
                let (kind, finding) = joining(opened.kind, review, severity)?;
                let new = NewThread {
                    kind,
                    finding,
                    held: review.is_some(),
                    name: &name,
                    opener: opened.opener.as_ref().map_or(Opener::User, Opener::Space),
                    space_id: &opened.space_id,
                    holder: opened.holder.as_ref(),
                };
                let id = records::insert_thread(tx, &new)?;
                match &opening {
                    Some(opening) => {
                        let (thread, delivered) = opening.record(tx, id, &*outside)?;
                        Ok(Some((thread, Some(delivered))))
                    }
                    None => Ok(Some((records::reread(tx, id)?, None))),
                }
            })
            .await?;
        let Some((thread, delivered)) = recorded else {
            return Ok(None);
        };
        self.signals.thread_changed();
        if let (Some(delivered), Some(versioned)) = (delivered, versioned) {
            self.announce_post(daemon, delivered, versioned).await;
        }
        Ok(Some(thread))
    }

    /// The opening post of a thread `caller` opens with `opening`: who
    /// makes it, its text and the references it makes; none for an empty
    /// thread, which has no post. The thread's transaction records the
    /// post, so a refused one leaves no thread behind.
    async fn opening_post(
        &self,
        daemon: &dyn Daemon,
        caller: Option<&Caller>,
        opening: ThreadOpening,
    ) -> Result<Option<Opening>> {
        let Some(text) = opening.post_text().map(str::to_owned) else {
            return Ok(None);
        };
        let code = self.new_code(daemon, caller, opening.code).await?;
        // The opener's side is checked against no holder.
        let poster = self
            .poster(daemon, caller, Some(Side::Opener), &Holder::User)
            .await?;
        Ok(Some(Opening { poster, text, code }))
    }

    /// The space whose agent coordinates the space `id`, here or on another
    /// machine, or none for the user.
    async fn coordinator_of(
        &self,
        daemon: &dyn Daemon,
        id: &SpaceId,
    ) -> Result<Option<ThreadSpace>> {
        Ok(match self.route_up(daemon, id, Source::Agent).await? {
            Recipients::Coordinator(id) | Recipients::Above(id) => {
                Some(self.named_space(daemon, id).await)
            }
            Recipients::User | Recipients::Spaces(_) => None,
        })
    }
}

/// The comment `opening` opens of `caller`'s pending review of the
/// workspace `located`,
/// which the machine that runs that workspace keeps and opens; none for
/// a space here, a chat, which no review is of, or without such a review.
/// Only that machine knows whether it keeps one, so failing to ask it
/// refuses the thread.
async fn comment_elsewhere(
    daemon: &dyn Daemon,
    caller: &Caller,
    located: &Located,
    opening: &ThreadOpening,
) -> Result<Option<Thread>> {
    let Located::Elsewhere { space, machine } = located else {
        return Ok(None);
    };
    if space.space.kind != SpaceKind::Workspace {
        return Ok(None);
    }
    let opened = ask_comment(daemon, caller, machine, &space.space.id, opening);
    opened.await.with_context(|| {
        format!(
            "machine {} keeps any review of {} and could not open the thread as its comment; \
             open a question with --kind question",
            machine.name,
            shoal_protocol::space_names::label(space.space.number, &space.space.name)
        )
    })
}

/// Ask `machine` to open the comment `opening` opens of `caller`'s
/// pending review of its workspace `workspace`; none without such a
/// review.
async fn ask_comment(
    daemon: &dyn Daemon,
    caller: &Caller,
    machine: &Member,
    workspace: &SpaceId,
    opening: &ThreadOpening,
) -> Result<Option<Thread>> {
    let operation = Operation::Thread(ThreadMethod::OpenComment {
        workspace: SpaceSelector::id(workspace),
        opening: opening.clone(),
    });
    match daemon.ask_machine(machine, operation, Some(caller)).await? {
        Body::SpaceThread(thread) => Ok(Some(*thread)),
        Body::Ok => Ok(None),
        body => bail!("machine {} answered a comment with {body:?}", machine.name),
    }
}

/// A thread to open: its kind, the space that opens it, none for the
/// user, the space it is numbered in and its holder, none for the user.
struct Opened {
    /// Without one, it comments the opener's pending review of its holder
    /// where it has one, and is a question otherwise.
    kind: Option<ThreadKind>,
    opener: Option<ThreadSpace>,
    space_id: SpaceId,
    holder: Option<ThreadSpace>,
    /// Whether it opens only as such a comment.
    only_comment: bool,
}

impl Opened {
    /// A question `caller` opens, the user without one, numbered in the
    /// space `space_id` and held by `holder`.
    fn by(caller: Option<&Caller>, space_id: SpaceId, holder: Option<ThreadSpace>) -> Self {
        Self {
            kind: None,
            opener: caller.map(|caller| ThreadSpace::here(caller.space_id.clone())),
            space_id,
            holder,
            only_comment: false,
        }
    }

    /// The pending review its opener writes of its holder, which a thread
    /// without a kind joins.
    fn joined_review(
        &self,
        tx: &rusqlite::Transaction<'_>,
        outside: &dyn Records,
    ) -> Result<Option<ReviewId>> {
        match (self.kind, &self.holder) {
            (None, Some(holder)) => {
                let writer = self.opener.as_ref().map(|opener| &opener.id);
                outside.joined_review(tx, &holder.id, writer)
            }
            _ => Ok(None),
        }
    }
}

/// Refuse an opening whose name or opening post a thread does not take.
fn validate(opening: &ThreadOpening) -> Result<()> {
    crate::validate::message(&opening.name).context("invalid thread name")?;
    if let Some(body) = &opening.body {
        posting::validate_text(body)?;
    }
    Ok(())
}

/// The kind and finding of a thread of `kind`, or without one of a
/// question, or a comment of `severity` of the pending review `review` it
/// joins; a severity marks only such a comment.
fn joining(
    kind: Option<ThreadKind>,
    review: Option<ReviewId>,
    severity: Option<Severity>,
) -> Result<(ThreadKind, Option<Finding>)> {
    Ok(match review {
        Some(review) => (
            ThreadKind::Review,
            Some(Finding {
                severity,
                review: Some(review),
            }),
        ),
        None => {
            ensure!(
                severity.is_none(),
                "--severity marks a comment of a review; start one with shoal review start <space>"
            );
            (kind.unwrap_or(ThreadKind::Question), None)
        }
    })
}

/// Refuse a resolved thread, naming how to reopen it.
fn ensure_open(thread: &Thread) -> Result<()> {
    match &thread.resolution {
        None => Ok(()),
        Some(resolution) => bail!(
            "thread {} is {}; reopen it with shoal thread resolve {} --undo",
            thread.number,
            resolution.outcome.describe(),
            thread.number
        ),
    }
}

#[cfg(test)]
mod tests;
