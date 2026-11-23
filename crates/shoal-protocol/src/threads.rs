//! Threads: conversations between whoever opened one, the user, a space or
//! Shoal, and whoever holds it now, the user or a space, with posts from
//! both sides and a typed resolution.
use std::{fmt, str::FromStr};

use anyhow::{Context as _, Result, bail};
use serde::{Deserialize, Serialize};
use shoal_ids::space_ids::{SpaceId, SpaceNumber};
use shoal_states::states;

pub use crate::alerts::Sender;
use crate::item_numbers::ItemNumber;
use crate::model::{Located, Space};
use crate::space_names::SpaceName;

/// Longest post, in bytes; posts may span lines.
pub const MAX_POST_LEN: usize = crate::validate::MAX_AGENT_MESSAGE_LEN;
/// Most choices a post may offer.
pub const MAX_CHOICES: usize = 20;
/// Most bytes a thread takes as JSON, which keeps it, with the response
/// around it, to one protocol frame.
pub const MAX_THREAD_BYTES: usize = crate::protocol::MAX_FRAME - 8 * 1024;

states!(
    /// What a thread is for, which decides the outcomes it takes.
    ThreadKind: ValueEnum {
        /// Its opener asks, and its holder answers.
        Question => "question",
        /// Its opener asks its holder to do something, such as implement
        /// the code its posts point at, and accepts the work once done.
        Request => "request",
        /// A reviewer's finding on the holder's code, which Shoal opens for
        /// a local review and its holder resolves, or a comment of a review
        /// the user or a space writes, which its opener resolves.
        Review => "review",
    }
);

states!(
    /// An outcome's name, as `shoal thread resolve` takes it.
    OutcomeKind: ValueEnum {
        Answered => "answered",
        Done => "done",
        Fixed => "fixed",
        FalsePositive => "false-positive",
        WontFix => "wont-fix",
        Deferred => "deferred",
        Duplicate => "duplicate",
        Withdrawn => "withdrawn",
        Closed => "closed",
    }
);

states!(
    /// Why Shoal closed a thread: a space that opened or held it finished
    /// its work, or was removed.
    CloseCause {
        Completion => "completion",
        Removal => "removal",
    }
);

states!(
    /// Which side of a thread a post came from.
    Side {
        /// Whoever opened the thread.
        Opener => "opener",
        /// Whoever held it when the post came.
        Holder => "holder",
    }
);

states!(
    /// Which threads a listing shows.
    ThreadState: ValueEnum {
        Open => "open",
        Resolved => "resolved",
        All => "all",
    }
);

/// How a thread was settled; each outcome carries what it needs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "kebab-case")]
pub enum Outcome {
    /// A question got its answer, optionally a choice it offered.
    Answered {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        choice: Option<u32>,
    },
    /// A request's work was made and its opener accepted it.
    Done,
    /// A finding's code was changed to address it; the next review checks
    /// it against the changes made for the thread.
    Fixed,
    /// A finding's claim is wrong.
    FalsePositive { reason: String },
    /// A finding's claim holds, but the code is not changed.
    WontFix { reason: String },
    /// A finding's claim holds and the work of an issue fixes it.
    Deferred { issue: String },
    /// Another thread asks the same.
    Duplicate { of: ItemNumber },
    /// Its opener no longer needs it.
    Withdrawn {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    /// Shoal closed it when a space that took part finished or went.
    Closed { cause: CloseCause },
}

impl Outcome {
    /// The outcome `kind` with its data for a thread of `thread`, refusing
    /// an outcome the kind does not take, data the outcome does not take
    /// and data it needs, and `closed`, which only Shoal sets.
    pub fn new(
        thread: ThreadKind,
        kind: OutcomeKind,
        choice: Option<u32>,
        of: Option<ItemNumber>,
        reason: Option<String>,
        issue: Option<String>,
    ) -> Result<Self> {
        if !thread.takes(kind) {
            bail!("a {thread} thread is never {kind}");
        }
        let present = |text: Option<String>| text.filter(|text| !text.trim().is_empty());
        let (reason, issue) = (present(reason), present(issue));
        Ok(match (kind, choice, of, reason, issue) {
            (OutcomeKind::Answered, choice, None, None, None) => Self::Answered { choice },
            (OutcomeKind::Done, None, None, None, None) => Self::Done,
            (OutcomeKind::Fixed, None, None, None, None) => Self::Fixed,
            (OutcomeKind::FalsePositive, None, None, Some(reason), None) => {
                Self::FalsePositive { reason }
            }
            (OutcomeKind::WontFix, None, None, Some(reason), None) => Self::WontFix { reason },
            (OutcomeKind::Deferred, None, None, None, Some(issue)) => Self::Deferred { issue },
            (OutcomeKind::Duplicate, None, Some(of), None, None) => Self::Duplicate { of },
            (OutcomeKind::Withdrawn, None, None, reason, None) => Self::Withdrawn { reason },
            (OutcomeKind::Closed, ..) => {
                bail!("only Shoal closes threads, when a space that took part finishes or goes")
            }
            (kind, ..) => bail!("{kind} takes {}", kind.data()),
        })
    }

    pub fn kind(&self) -> OutcomeKind {
        match self {
            Self::Answered { .. } => OutcomeKind::Answered,
            Self::Done => OutcomeKind::Done,
            Self::Fixed => OutcomeKind::Fixed,
            Self::FalsePositive { .. } => OutcomeKind::FalsePositive,
            Self::WontFix { .. } => OutcomeKind::WontFix,
            Self::Deferred { .. } => OutcomeKind::Deferred,
            Self::Duplicate { .. } => OutcomeKind::Duplicate,
            Self::Withdrawn { .. } => OutcomeKind::Withdrawn,
            Self::Closed { .. } => OutcomeKind::Closed,
        }
    }

    /// The outcome with its data, as one line.
    pub fn describe(&self) -> String {
        match self {
            Self::Answered { choice: None } => "answered".into(),
            Self::Answered {
                choice: Some(choice),
            } => format!("answered with choice {choice}"),
            Self::Done => "done".into(),
            Self::Fixed => "fixed".into(),
            Self::FalsePositive { reason } => format!("false-positive: {reason}"),
            Self::WontFix { reason } => format!("wont-fix: {reason}"),
            Self::Deferred { issue } => format!("deferred to {issue}"),
            Self::Duplicate { of } => format!("duplicate of {of}"),
            Self::Withdrawn { reason: None } => "withdrawn".into(),
            Self::Withdrawn {
                reason: Some(reason),
            } => format!("withdrawn: {reason}"),
            Self::Closed {
                cause: CloseCause::Completion,
            } => "closed: a space that took part finished".into(),
            Self::Closed {
                cause: CloseCause::Removal,
            } => "closed: a space that took part was removed".into(),
        }
    }
}

impl OutcomeKind {
    /// The options the outcome takes.
    fn data(self) -> &'static str {
        match self {
            Self::Answered => "only --choose",
            Self::Done | Self::Fixed => "nothing",
            Self::FalsePositive | Self::WontFix => "--reason and nothing else",
            Self::Deferred => "--issue and nothing else",
            Self::Duplicate => "--of and nothing else",
            Self::Withdrawn => "only --reason",
            Self::Closed => "nothing",
        }
    }
}

impl ThreadKind {
    /// The outcome it is settled with once it got what it asked.
    pub fn settled(self) -> OutcomeKind {
        match self {
            Self::Question => OutcomeKind::Answered,
            Self::Request => OutcomeKind::Done,
            Self::Review => OutcomeKind::Fixed,
        }
    }

    /// Whether a thread of this kind may end with `outcome`.
    pub fn takes(self, outcome: OutcomeKind) -> bool {
        match outcome {
            OutcomeKind::Answered => self == Self::Question,
            OutcomeKind::Done => self == Self::Request,
            OutcomeKind::Fixed
            | OutcomeKind::FalsePositive
            | OutcomeKind::WontFix
            | OutcomeKind::Deferred => self == Self::Review,
            OutcomeKind::Duplicate | OutcomeKind::Withdrawn | OutcomeKind::Closed => true,
        }
    }
}

/// A space a thread names: by its ID, and by its name and number while it
/// exists here, or as Shoal found them when the thread named a space of
/// another machine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadSpace {
    pub id: SpaceId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<SpaceName>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub number: Option<SpaceNumber>,
}

impl ThreadSpace {
    /// The space `id` of this machine, which its record here names.
    pub fn here(id: SpaceId) -> Self {
        Self {
            id,
            name: None,
            number: None,
        }
    }

    /// The space `space` of this machine, by its number and name.
    pub fn of(space: &Space) -> Self {
        Self {
            id: space.id.clone(),
            name: Some(space.name.clone()),
            number: Some(space.number),
        }
    }

    /// The space `located`, by the number and name it has there.
    pub fn located(located: Located) -> Self {
        match located {
            Located::Here(space) => Self::of(&space),
            Located::Elsewhere { space, .. } => Self {
                id: space.space.id,
                name: Some(space.space.name),
                number: Some(space.space.number),
            },
        }
    }

    /// The space as text names it: its number and name while it has them.
    pub fn label(&self) -> String {
        match (self.number, &self.name) {
            (Some(number), Some(name)) => crate::space_names::label(number, name),
            (None, Some(name)) => name.to_string(),
            _ => self.id.to_string(),
        }
    }
}

/// Who holds a thread: the user or a space.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Holder {
    User,
    Space(ThreadSpace),
}

impl Holder {
    /// The space it names, unless it is the user.
    pub fn space(&self) -> Option<&ThreadSpace> {
        match self {
            Self::User => None,
            Self::Space(space) => Some(space),
        }
    }

    pub fn label(&self) -> String {
        match self {
            Self::User => "the user".into(),
            Self::Space(space) => space.label(),
        }
    }
}

/// What a new post says: its text, and optionally choices with the one it
/// recommends and why, or the number of a choice it picks.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewPost {
    pub text: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub choices: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recommendation: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// The number of a choice the other side offered that the post picks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub choice: Option<u32>,
}

/// A post in a thread.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Post {
    pub id: i64,
    pub created_at: crate::time::UnixTime,
    pub side: Side,
    pub sender: Sender,
    /// The space that sent it, unless the user or Shoal did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub space: Option<ThreadSpace>,
    #[serde(flatten)]
    pub post: NewPost,
    /// The files sent with it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<crate::files::SentFile>,
    /// The places in code it points at.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub code: Vec<crate::code::CodeRef>,
    /// The changes it points at, where Shoal followed them to.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub changes: Vec<crate::changes::ChangeDiff>,
    /// Whether it is the final message of a turn in the sender's chat.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub in_chat: bool,
    /// Whom it forwarded the thread to, with its text as the note.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub forwarded_to: Option<Holder>,
    /// The alert that carried it to whom it was for, until delivered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alert_id: Option<i64>,
}

impl Post {
    /// Who sent it, as text names them.
    pub fn author(&self) -> String {
        match (&self.space, self.sender) {
            (Some(space), _) => space.label(),
            (None, Sender::User) => "the user".into(),
            (None, Sender::Shoal | Sender::Space) => "Shoal".into(),
        }
    }

    /// Whether it says something to the other side, rather than forwarding
    /// the thread.
    pub fn speaks(&self) -> bool {
        self.forwarded_to.is_none()
    }
}

/// Where a thread stands for editors, which draw it around its code; it
/// comes from its posts and resolution, not a record of its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThreadStatus {
    /// Open, and its holder's agent works for it: its changes go to it.
    Working,
    /// Open, and its last post is its opener's: its holder's turn.
    Queued,
    /// Open, and its last post is its holder's, or it has none: its
    /// opener's turn.
    Waiting,
    /// Resolved with this outcome.
    Resolved(OutcomeKind),
}

impl fmt::Display for ThreadStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Working => f.write_str("working"),
            Self::Queued => f.write_str("queued"),
            Self::Waiting => f.write_str("waiting"),
            Self::Resolved(outcome) => outcome.fmt(f),
        }
    }
}

/// How a thread is to be settled, as `shoal thread resolve` names it: an
/// outcome with its data, or none to reopen it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Resolving {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<OutcomeKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub choice: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub of: Option<ThreadNumber>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issue: Option<String>,
}

impl Resolving {
    /// Whether it names any outcome data, which reopening takes none of.
    pub fn has_data(&self) -> bool {
        self.choice.is_some() || self.of.is_some() || self.reason.is_some() || self.issue.is_some()
    }
}

/// What a `review` thread is about: the review that opened it, while it
/// exists, and how much its finding matters, as Shoal's reviewer judged it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finding {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub severity: Option<crate::reviews::Severity>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review: Option<crate::reviews::ReviewId>,
}

/// How a thread was settled, by whom and when.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Resolution {
    #[serde(flatten)]
    pub outcome: Outcome,
    pub by: Sender,
    /// The space that resolved it, or whose completion or removal closed
    /// it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by_space: Option<SpaceId>,
    pub resolved_at: crate::time::UnixTime,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Thread {
    /// Its record's ID, which outlasts a change of its number.
    pub id: i64,
    /// Its number: the space it is numbered in, then its own.
    #[serde(flatten)]
    pub number: ItemNumber,
    /// The space it is numbered in: its opener's, or the space the user or
    /// Shoal opened it with.
    pub space_id: SpaceId,
    /// That space's current name.
    pub space: SpaceName,
    pub kind: ThreadKind,
    /// A `review` thread's finding.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finding: Option<Finding>,
    pub name: String,
    pub opener: Sender,
    /// The space that opened it, for a thread a space opened: the one it is
    /// numbered in, or for a comment of a space's review of a workspace on
    /// another machine, that space, as the hub listed it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opener_space: Option<ThreadSpace>,
    pub holder: Holder,
    pub created_at: crate::time::UnixTime,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub posts: Vec<Post>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolution: Option<Resolution>,
    /// Whether its holder's agent works for it now, having switched to it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub working: bool,
    /// Whether its posts are held, as a pending review's are, which reach
    /// nobody until the review is submitted.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub held: bool,
    /// What agents changed for it, one entry per turn, which its history
    /// shows between its posts.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub changes: Vec<crate::changes::ChangeEntry>,
}

impl Thread {
    /// Its posts' files as a machine other than `machine`, which keeps
    /// them, sees them.
    pub fn files_kept_on(&mut self, machine: &str) {
        for file in self.posts.iter_mut().flat_map(|post| &mut post.files) {
            file.copy.kept_on(machine);
        }
    }

    pub fn is_open(&self) -> bool {
        self.resolution.is_none()
    }

    /// The side whose turn it is to post: the other side of its last post
    /// that speaks, while it is open; none before its first post, or while
    /// it holds its posts, which have reached nobody.
    pub fn awaits(&self) -> Option<Side> {
        if !self.is_open() || self.held {
            return None;
        }
        let last = self.posts.iter().rev().find(|post| post.speaks())?;
        Some(match last.side {
            Side::Opener => Side::Holder,
            Side::Holder => Side::Opener,
        })
    }

    /// Where it stands for editors.
    pub fn status(&self) -> ThreadStatus {
        match (&self.resolution, self.awaits()) {
            (Some(resolution), _) => ThreadStatus::Resolved(resolution.outcome.kind()),
            (None, _) if self.working => ThreadStatus::Working,
            (None, Some(Side::Holder)) => ThreadStatus::Queued,
            (None, Some(Side::Opener) | None) => ThreadStatus::Waiting,
        }
    }

    /// The ID of the space that opened it, for a thread a space opened.
    pub fn opener_id(&self) -> Option<&SpaceId> {
        self.opener_space.as_ref().map(|opener| &opener.id)
    }

    /// Whether the space `space` takes part in it now: as the space it is
    /// numbered in, as its opener, or as its holder.
    pub fn has_party(&self, space: &SpaceId) -> bool {
        self.space_id == *space
            || self.opener_id() == Some(space)
            || self
                .holder
                .space()
                .is_some_and(|holder| holder.id == *space)
    }

    /// Whether it waits for its holder: open, with a post, the last from
    /// its opener's side.
    pub fn is_waiting(&self) -> bool {
        self.awaits() == Some(Side::Holder)
    }

    /// Whether it waits for the user: the side whose turn it is is the
    /// user's.
    pub fn waits_for_user(&self) -> bool {
        match self.awaits() {
            Some(Side::Holder) => self.holder == Holder::User,
            Some(Side::Opener) => self.opener == Sender::User,
            None => false,
        }
    }

    /// Whom the side `side` names, as text names them.
    pub fn party(&self, side: Side) -> String {
        match (side, self.opener) {
            (Side::Holder, _) => self.holder.label(),
            (Side::Opener, Sender::User) => "the user".into(),
            (Side::Opener, Sender::Shoal) => "Shoal".into(),
            (Side::Opener, Sender::Space) => match &self.opener_space {
                Some(opener) if opener.id != self.space_id => opener.label(),
                _ => self.space.to_string(),
            },
        }
    }

    /// Its name on one line, as listings and alerts show it.
    pub fn one_line(&self) -> String {
        self.name.replace(['\n', '\t'], " ")
    }

    /// The choices of the latest post that offered any.
    pub fn choices(&self) -> &[String] {
        (self.posts.iter().rev())
            .find(|post| !post.post.choices.is_empty())
            .map_or(&[], |post| &post.post.choices)
    }

    /// The place in code its opening post points at first, which a
    /// review's finding is about.
    pub fn about(&self) -> Option<&crate::code::CodeRef> {
        self.posts.first()?.code.first()
    }

    /// Its number, a finding's severity, where the code it is about is now
    /// and its name, on one line, such as
    /// `3.2 blocker at src/a.rs:4-6: Unchecked index`.
    pub fn finding_line(&self) -> String {
        let severity = (self.finding.and_then(|finding| finding.severity))
            .map_or(String::new(), |severity| format!(" {severity}"));
        let place = (self.about()).map_or(String::new(), |code| format!(" at {}", code.place()));
        format!("{}{severity}{place}: {}", self.number, self.one_line())
    }

    /// The latest post, which a listing shows.
    pub fn last_post(&self) -> Option<&Post> {
        self.posts.iter().rev().find(|post| post.speaks())
    }

    /// What the user is told of its latest post, in one line, with how to
    /// reply; it is cut to the length of other alerts, so a batch of them
    /// still fits one response.
    pub fn notice(&self) -> String {
        let number = self.number;
        let mut text = format!("thread {number} ({})", self.one_line());
        if let Some(post) = self.last_post() {
            text.push_str(&format!(": {}", post.post.text.replace(['\n', '\t'], " ")));
            if let Some(recommendation) = &post.post.recommendation {
                text.push_str(&format!(" Recommended: {recommendation}"));
            }
        }
        text.push_str(&format!(" (reply with shoal send {number})"));
        crate::validate::shorten(text, crate::validate::MAX_MESSAGE_LEN)
    }
}

/// Threads that fit one response, and how many older ones were left out.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Listing {
    pub threads: Vec<Thread>,
    pub omitted: usize,
}

/// A thread as a command names it: `S.N`, or inside a space a bare `N`,
/// which names the thread `N` of the caller's own space.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ThreadNumber {
    Full(ItemNumber),
    Own(u32),
}

impl ThreadNumber {
    /// The thread's full number, given the number of the caller's space,
    /// which a bare number needs.
    pub fn resolve(self, own: Option<SpaceNumber>) -> Result<ItemNumber> {
        match (self, own) {
            (Self::Full(number), _) => Ok(number),
            (Self::Own(item), Some(space)) => Ok(ItemNumber { space, item }),
            (Self::Own(item), None) => bail!(
                "{item} names no space outside one: name the thread as SPACE.ITEM, such as 3.{item}"
            ),
        }
    }
}

impl fmt::Display for ThreadNumber {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Full(number) => number.fmt(f),
            Self::Own(item) => item.fmt(f),
        }
    }
}

impl FromStr for ThreadNumber {
    type Err = anyhow::Error;

    fn from_str(text: &str) -> Result<Self> {
        if text.contains('.') {
            return Ok(Self::Full(text.parse()?));
        }
        let item = (text.parse::<u32>().ok())
            .filter(|item| *item > 0)
            .with_context(|| {
                format!("invalid thread {text}: expected SPACE.ITEM, such as 3.2, or ITEM inside a space")
            })?;
        Ok(Self::Own(item))
    }
}

#[cfg(test)]
mod tests;
