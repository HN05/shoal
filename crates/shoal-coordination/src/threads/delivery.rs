//! How a thread's posts, forwards and resolutions reach the parties: as
//! alerts, queued in the transaction that records the change, to the
//! user, a space's agent or, for a holder on another machine, passed up
//! there once it commits. Every alert about a thread names it as its
//! subject, so withdrawing the thread takes back those still unread. A
//! thread that holds its posts, as a pending review's does, reaches nobody;
//! submitting the review tells its holder of them all at once.
use crate::{Coordination, daemon::Daemon};

use anyhow::Result;
use rusqlite::{Transaction, params};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::{
    changes::ChangeSize,
    space_names::SpaceName,
    threads::{Holder, Post, Sender, Side, Thread, ThreadKind, ThreadSpace},
};

use crate::{
    alerts::{About, AlertKind, NewAlert},
    daemon::{PassedUp, Records},
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
            (Side::Opener, Sender::Space, _) => thread.opener_id().cloned().map(Self::Space),
            (Side::Opener, Sender::Shoal, _) => None,
        }
    }
}

/// What a change of a thread delivered in its transaction, which the
/// daemon announces once it commits.
#[derive(Debug, Default)]
pub struct Delivered {
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
    /// The line the user reads should no agent there take it.
    message: String,
    /// The text the agent that takes it reads, as an agent here would.
    agent: String,
    files: Vec<shoal_protocol::files::SentFile>,
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
    let alert = NewAlert {
        sender,
        about: About {
            subject: Some(subject(thread)),
            ..about
        },
        ..NewAlert::shoal(kind, About::default(), message)
    };
    alert.with_files(post.map_or(&[], |post| &post.files))
}

/// Deliver `message` of `kind` about `thread` from `post` to `party`, the
/// user in one line, in the caller's transaction; the alert's ID, none for
/// a space on another machine or a thread that holds its posts. A space
/// that takes no alert, as while it is being removed, is skipped and
/// logged.
pub(super) fn deliver(
    tx: &Transaction<'_>,
    outside: &dyn Records,
    thread: &Thread,
    post: Option<&Post>,
    to: &Party,
    texts: Texts,
    delivered: &mut Delivered,
) -> Result<Option<i64>> {
    if thread.held {
        return Ok(None);
    }
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
                agent: texts.agent,
                files: post.map(|post| post.files.clone()).unwrap_or_default(),
            });
            Ok(None)
        }
        Party::Space(id) => {
            let alert = alert(thread, post, texts.kind, texts.agent);
            match outside.queue_alert_in(tx, id, &alert) {
                Ok(queued) => {
                    delivered.spaces |= queued.new;
                    Ok(Some(queued.id))
                }
                Err(error) => {
                    crate::log!("thread {} not delivered to {id}: {error:#}", thread.number);
                    Ok(None)
                }
            }
        }
    }
}

/// Whether the space `id` is this machine's.
fn here(tx: &Transaction<'_>, id: &SpaceId) -> Result<bool> {
    shoal_store::exists(tx, "SELECT 1 FROM spaces WHERE id=?1", [id])
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
        let reader = Party::of(thread, to);
        agent.push_str(&code_lines(post, reader.as_ref()));
        let changed = changes_since(thread, post);
        if let Some((size, command)) = &changed {
            agent.push_str(&format!(
                "\nChanges for it since then: {size} (`{command}`)"
            ));
        }
        agent.push_str(&format!("\n{}", hint(thread, to)));
        agent.push_str(&switch_reminder(thread, reader.as_ref()));
        Self {
            kind: AlertKind::ThreadPosted,
            agent: crate::validate::shorten(agent, crate::validate::MAX_AGENT_MESSAGE_LEN),
            user: match changed {
                Some((size, _)) => crate::validate::shorten(
                    format!("{} [{size}]", thread.notice()),
                    crate::validate::MAX_MESSAGE_LEN,
                ),
                None => thread.notice(),
            },
        }
    }

    /// `post` in `thread`, for the space `linker` that linked it.
    pub(super) fn linked(thread: &Thread, post: &Post, linker: &Party) -> Self {
        let text = format!(
            "Thread {} ({}), which you linked, {}: {}",
            thread.number,
            thread.one_line(),
            post.author(),
            post.post.text
        );
        let agent = format!("{text}{}", code_lines(post, Some(linker)));
        Self {
            kind: AlertKind::ThreadPosted,
            user: crate::validate::shorten(
                text.replace(['\n', '\t'], " "),
                crate::validate::MAX_MESSAGE_LEN,
            ),
            agent: crate::validate::shorten(agent, crate::validate::MAX_AGENT_MESSAGE_LEN),
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
        let reader = Party::of(thread, Side::Holder);
        for post in thread.posts.iter().filter(|post| post.speaks()) {
            agent.push_str(&format!("\n{}: {}", post.author(), post.post.text));
            agent.push_str(&code_lines(post, reader.as_ref()));
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

/// The places in code and the changes `post` points at, each on lines of
/// its own with its snippet or diff, naming the space whose worktree it is
/// in unless that is `reader`'s.
fn code_lines(post: &Post, reader: Option<&Party>) -> String {
    let mut lines = String::new();
    let elsewhere = |space: &ThreadSpace| reader != Some(&Party::Space(space.id.clone()));
    for code in &post.code {
        lines.push_str(&format!("\nCode: {}", code.place()));
        if elsewhere(&code.space) {
            lines.push_str(&format!(" in {}", code.space.label()));
        }
        lines.push_str(&code.snippet_lines("    "));
    }
    for diff in &post.changes {
        let change = &diff.change;
        lines.push_str(&format!("\nChange {}: {}", change.number, change.place()));
        if elsewhere(&change.space) {
            lines.push_str(&format!(" in {}", change.space.label()));
        }
        for line in diff.patch().iter().flat_map(|patch| patch.lines()) {
            lines.push_str(&format!("\n    {line}"));
        }
    }
    lines
}

/// The size of the changes made for `thread` since the last post before
/// `post` from its side, with the command that shows them; none without
/// any.
fn changes_since(thread: &Thread, post: &Post) -> Option<(ChangeSize, String)> {
    let speaking: Vec<&Post> = thread.posts.iter().filter(|post| post.speaks()).collect();
    let last = (speaking.iter().enumerate())
        .rfind(|(_, earlier)| earlier.id < post.id && earlier.side == post.side);
    let since = last.map(|(_, earlier)| earlier.created_at);
    let entries =
        (thread.changes.iter()).filter(|entry| since.is_none_or(|since| entry.at >= since));
    let size = ChangeSize::of_entries(entries);
    if size.is_empty() {
        return None;
    }
    let number = thread.number;
    let command = match last {
        Some((index, _)) => format!("shoal diff --thread {number} --since {}", index + 1),
        None => format!("shoal diff --thread {number}"),
    };
    Some((size, command))
}

/// The reminder a space's agent reads with a post in `thread` to switch to
/// it before working on it, which sends a workspace's changes to it and
/// shows it `working`; nothing for the user.
fn switch_reminder(thread: &Thread, reader: Option<&Party>) -> String {
    match reader {
        Some(Party::Space(_)) => format!(
            " Run `shoal thread switch {}` before working on it.",
            thread.number
        ),
        _ => String::new(),
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
        (Side::Holder, ThreadKind::Review) if thread.opener != Sender::Shoal => format!(
            "Fix it, then say what you did with `shoal send {number} \"<text>\"`, or there why you would not; {} resolves it.",
            thread.party(Side::Opener)
        ),
        (Side::Holder, ThreadKind::Review) => format!(
            "Settle it with `shoal thread resolve {number} fixed`, or `false-positive` or `wont-fix` with `--reason`, or `deferred` with `--issue`; discuss it with the reviewer with `shoal send {number} \"<text>\"`."
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
    outside: &dyn Records,
    thread: &Thread,
    delivered: &mut Delivered,
) -> Result<()> {
    let Some(post) = thread.posts.last() else {
        return Ok(());
    };
    let Some(to) = post.side.map(Side::other) else {
        return Ok(());
    };
    let party = Party::of(thread, to);
    if let Some(party) = &party
        && let Some(alert) = deliver(
            tx,
            outside,
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
            let linker = Party::Space(linker);
            let texts = Texts::linked(thread, post, &linker);
            deliver(tx, outside, thread, Some(post), &linker, texts, delivered)?;
        }
    }
    Ok(())
}

impl Coordination {
    /// Wake what waits for the alerts `delivered` recorded, and pass those
    /// for other machines up there.
    pub async fn announce_threads(&self, daemon: &dyn Daemon, delivered: Delivered) {
        if !delivered.user.is_empty() {
            self.notifications.recorded();
        }
        if delivered.spaces {
            self.signals.message_queued();
        }
        for elsewhere in delivered.elsewhere {
            let event = PassedUp {
                name: &elsewhere.name,
                space: &elsewhere.from,
                kind: elsewhere.kind,
                for_user: true,
                message: elsewhere.message,
                agent: Some(elsewhere.agent),
                files: &elsewhere.files,
            };
            let passed = Box::pin(daemon.pass_up(elsewhere.to, event)).await;
            if let Err(error) = passed {
                crate::log!("thread alert for another machine not sent: {error:#}");
            }
        }
        self.signals.thread_changed();
    }
}
