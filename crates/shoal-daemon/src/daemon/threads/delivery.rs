//! How a thread's posts, forwards and resolutions reach the parties: as
//! alerts, queued in the transaction that records the change, to the
//! user, a space's agent or, for a holder on another machine, passed up
//! there once it commits. Every alert about a thread names it as its
//! subject, so withdrawing the thread takes back those still unread.
use std::path::PathBuf;

use anyhow::Result;
use rusqlite::{Transaction, params};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::{
    space_names::SpaceName,
    threads::{Holder, Post, Sender, Side, Thread, ThreadKind},
};

use crate::daemon::{
    agent_messages,
    alerts::{About, AlertKind, NewAlert},
    manager::Manager,
    notifications,
};

/// Whom an alert about a thread goes to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Party {
    User,
    Space(SpaceId),
}

impl Party {
    /// The party on `side` of `thread`; none for Shoal.
    pub(super) fn of(thread: &Thread, side: Side) -> Option<Self> {
        match (side, thread.opener, &thread.holder) {
            (Side::Holder, _, Holder::User) => Some(Self::User),
            (Side::Holder, _, Holder::Space(holder)) => Some(Self::Space(holder.id.clone())),
            (Side::Opener, Sender::User, _) => Some(Self::User),
            (Side::Opener, Sender::Space, _) => Some(Self::Space(thread.space_id.clone())),
            (Side::Opener, Sender::Shoal, _) => None,
        }
    }
}

/// What a change of a thread delivered in its transaction, which the
/// daemon announces once it commits.
#[derive(Debug, Default)]
pub(crate) struct Delivered {
    /// The alerts recorded for the user.
    user: Vec<i64>,
    /// Whether an alert was queued for a space's agent.
    spaces: bool,
    /// The alerts for spaces on other machines.
    elsewhere: Vec<Elsewhere>,
}

/// An alert for a space on another machine, which goes up there from the
/// space a thread is numbered in.
#[derive(Debug)]
struct Elsewhere {
    to: SpaceId,
    from: SpaceId,
    name: SpaceName,
    kind: AlertKind,
    message: String,
    attachments: Vec<PathBuf>,
}

/// The subject that names `thread` on its alerts.
pub(super) fn subject(thread: &Thread) -> String {
    format!("thread/{}", thread.id)
}

/// An alert of `kind` about `thread` with `message`, sent by `post`'s
/// sender, or by Shoal without a post.
fn alert(thread: &Thread, post: Option<&Post>, kind: AlertKind, message: String) -> NewAlert {
    let (sender, about) = match post.map(|post| (post.sender, &post.space)) {
        Some((sender, Some(space))) => (
            sender,
            About::space(space.name.as_ref().unwrap_or(&thread.space), &space.id),
        ),
        Some((sender, None)) => (sender, About::default()),
        None => (Sender::Shoal, About::default()),
    };
    NewAlert {
        kind,
        sender,
        about: About {
            subject: Some(subject(thread)),
            ..about
        },
        message,
        attachments: post
            .map(|post| post.files.iter().map(|file| file.path.clone()).collect())
            .unwrap_or_default(),
    }
}

/// Deliver `message` of `kind` about `thread` from `post` to `party`, the
/// user in one line, in the caller's transaction; the alert's ID, none for
/// a space on another machine. A space that takes no alert, as while it
/// is being removed, is skipped and logged.
pub(super) fn deliver(
    tx: &Transaction<'_>,
    thread: &Thread,
    post: Option<&Post>,
    to: &Party,
    texts: Texts,
    delivered: &mut Delivered,
) -> Result<Option<i64>> {
    match to {
        Party::User => {
            let alert = alert(thread, post, texts.kind, texts.user);
            let recorded = notifications::insert(tx, &alert)?;
            if recorded.new {
                delivered.user.push(recorded.id);
            }
            Ok(Some(recorded.id))
        }
        Party::Space(id) if !here(tx, id)? => {
            let alert = alert(thread, post, texts.kind, texts.user);
            delivered.elsewhere.push(Elsewhere {
                to: id.clone(),
                from: thread.space_id.clone(),
                name: thread.space.clone(),
                kind: alert.kind,
                message: alert.message,
                attachments: alert.attachments,
            });
            Ok(None)
        }
        Party::Space(id) => {
            let alert = alert(thread, post, texts.kind, texts.agent);
            match agent_messages::queue_in(tx, id, &alert) {
                Ok(queued) => {
                    delivered.spaces |= queued.new;
                    Ok(Some(queued.id))
                }
                Err(error) => {
                    crate::daemon::log!(
                        "thread {} not delivered to {id}: {error:#}",
                        thread.number
                    );
                    Ok(None)
                }
            }
        }
    }
}

/// Whether the space `id` is this machine's.
fn here(tx: &Transaction<'_>, id: &SpaceId) -> Result<bool> {
    crate::daemon::store::exists(tx, "SELECT 1 FROM spaces WHERE id=?1", [id])
}

/// Take back the unread alerts about `thread` that `party` has; whether
/// there were any.
pub(super) fn retract(tx: &Transaction<'_>, thread: &Thread, party: &Party) -> Result<bool> {
    let recipient = match party {
        Party::User => None,
        Party::Space(id) => Some(id),
    };
    let deleted = tx.execute(
        "DELETE FROM alerts WHERE subject=?1 AND read=0 AND recipient_id IS ?2",
        params![subject(thread), recipient],
    )?;
    Ok(deleted > 0)
}

/// Take back the alert `id` while it is unread; whether it was.
pub(super) fn retract_alert(tx: &Transaction<'_>, id: i64) -> Result<bool> {
    Ok(tx.execute("DELETE FROM alerts WHERE id=?1 AND read=0", [id])? > 0)
}

/// What an alert of `kind` says to an agent and, in one line, to the user.
pub(super) struct Texts {
    pub(super) kind: AlertKind,
    pub(super) agent: String,
    pub(super) user: String,
}

impl Texts {
    /// `post`, the latest in `thread`, for the party on `to`, who answers
    /// it.
    pub(super) fn post(thread: &Thread, post: &Post, to: Side) -> Self {
        let number = thread.number;
        let mut agent = format!(
            "Thread {number} ({}), {}:",
            thread.one_line(),
            post.author()
        );
        if post.post.text.contains('\n') {
            agent.push('\n');
        } else {
            agent.push(' ');
        }
        agent.push_str(&post.post.text);
        for (index, choice) in (1..).zip(&post.post.choices) {
            agent.push_str(&format!("\n  {index}. {choice}"));
        }
        if let Some(recommendation) = &post.post.recommendation {
            agent.push_str(&format!("\nRecommended: {recommendation}"));
        }
        if let Some(reason) = &post.post.reason {
            agent.push_str(&format!("\nReason: {reason}"));
        }
        agent.push_str(&format!("\n{}", hint(thread, to)));
        Self {
            kind: AlertKind::ThreadPosted,
            agent: crate::validate::shorten(agent, crate::validate::MAX_AGENT_MESSAGE_LEN),
            user: thread.notice(),
        }
    }

    /// `post` in `thread`, for a space that linked it.
    pub(super) fn linked(thread: &Thread, post: &Post) -> Self {
        let text = format!(
            "Thread {} ({}), which you linked, {}: {}",
            thread.number,
            thread.one_line(),
            post.author(),
            post.post.text
        );
        Self {
            kind: AlertKind::ThreadPosted,
            user: crate::validate::shorten(
                text.replace(['\n', '\t'], " "),
                crate::validate::MAX_MESSAGE_LEN,
            ),
            agent: crate::validate::shorten(text, crate::validate::MAX_AGENT_MESSAGE_LEN),
        }
    }

    /// `thread`, which `forward` gave its new holder, with what it says.
    pub(super) fn forwarded(thread: &Thread, forward: &Post) -> Self {
        let number = thread.number;
        let note = match forward.post.text.as_str() {
            "" => String::new(),
            note => format!(": {note}"),
        };
        let heading = format!(
            "Thread {number} ({}) came to you from {}{note}",
            thread.one_line(),
            forward.author()
        );
        let mut agent = format!("{heading}\nIt reads:");
        for post in thread.posts.iter().filter(|post| post.speaks()) {
            agent.push_str(&format!("\n{}: {}", post.author(), post.post.text));
        }
        agent.push_str(&format!("\n{}", hint(thread, Side::Holder)));
        Self {
            kind: AlertKind::ThreadForwarded,
            user: crate::validate::shorten(
                format!(
                    "{} (shoal view thread {number})",
                    heading.replace('\n', " ")
                ),
                crate::validate::MAX_MESSAGE_LEN,
            ),
            agent: crate::validate::shorten(agent, crate::validate::MAX_AGENT_MESSAGE_LEN),
        }
    }

    /// `thread`, resolved as it is now, or reopened, by `by`.
    pub(super) fn resolved(thread: &Thread, by: &str) -> Self {
        let text = match &thread.resolution {
            Some(resolution) => format!(
                "Thread {} ({}) was {} by {by}.",
                thread.number,
                thread.one_line(),
                resolution.outcome.describe()
            ),
            None => format!(
                "Thread {} ({}) is open again; {by} reopened it.",
                thread.number,
                thread.one_line()
            ),
        };
        Self::notice(text)
    }

    /// A notice about a thread, the same for an agent and the user.
    pub(super) fn notice(text: String) -> Self {
        Self {
            kind: AlertKind::ThreadResolved,
            agent: crate::validate::shorten(text.clone(), crate::validate::MAX_AGENT_MESSAGE_LEN),
            user: crate::validate::shorten(text, crate::validate::MAX_MESSAGE_LEN),
        }
    }
}

/// What the party on `side` of `thread` does next.
fn hint(thread: &Thread, side: Side) -> String {
    let number = thread.number;
    match (side, thread.kind) {
        (Side::Holder, ThreadKind::Question) => format!(
            "Reply with `shoal send {number} \"<answer>\"`, `--choose N` picking a choice; pass it on with `shoal thread forward {number}`."
        ),
        (Side::Holder, ThreadKind::Request) => format!(
            "Once it is done, say what you did with `shoal send {number} \"<text>\"`, or there why you cannot; pass it on with `shoal thread forward {number}`."
        ),
        (Side::Opener, kind) => format!(
            "Reply with `shoal send {number} \"<text>\"`, or settle it with `shoal thread resolve {number} {}`.",
            kind.settled()
        ),
    }
}

/// Deliver the newest post of `thread` to the party on the other side and
/// to the spaces that linked it but the sender's, recording on the post
/// the alert that carried it to that party.
pub(super) fn deliver_post(
    tx: &Transaction<'_>,
    thread: &Thread,
    delivered: &mut Delivered,
) -> Result<()> {
    let Some(post) = thread.posts.last() else {
        return Ok(());
    };
    let to = match post.side {
        Side::Opener => Side::Holder,
        Side::Holder => Side::Opener,
    };
    let party = Party::of(thread, to);
    if let Some(party) = &party
        && let Some(alert) = deliver(
            tx,
            thread,
            Some(post),
            party,
            Texts::post(thread, post, to),
            delivered,
        )?
    {
        super::records::set_alert(tx, post.id, alert)?;
    }
    let sender = post.space.as_ref().map(|space| &space.id);
    for linker in super::records::linkers(tx, thread.id)? {
        let reached = party == Some(Party::Space(linker.clone()));
        if !reached && Some(&linker) != sender {
            let texts = Texts::linked(thread, post);
            deliver(
                tx,
                thread,
                Some(post),
                &Party::Space(linker),
                texts,
                delivered,
            )?;
        }
    }
    Ok(())
}

impl Manager {
    /// Wake what waits for the alerts `delivered` recorded, and pass those
    /// for other machines up there.
    pub(crate) async fn announce_threads(&self, delivered: Delivered) {
        if let Some(id) = delivered.user.iter().max() {
            self.notifications.recorded(*id);
        }
        if delivered.spaces {
            self.signals.message_queued();
        }
        for elsewhere in delivered.elsewhere {
            let event = crate::daemon::hub::Event {
                name: &elsewhere.name,
                space: &elsewhere.from,
                kind: elsewhere.kind,
                for_user: true,
                message: elsewhere.message,
                attachments: &elsewhere.attachments,
            };
            let passed = Box::pin(self.hub.pass_up(self, elsewhere.to, event)).await;
            if let Err(error) = passed {
                crate::daemon::log!("thread alert for another machine not sent: {error:#}");
            }
        }
        self.signals.thread_changed();
    }
}
