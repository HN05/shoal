//! Posting in a thread: the post's checks, the choice it picks, and
//! recording and delivering it from the poster's side.
use crate::{Coordination, daemon::Daemon};
use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context as _, Result, ensure};
use rusqlite::Transaction;
use shoal_ids::{repository_ids::RepositoryId, space_ids::SpaceId};
use shoal_protocol::{
    changes::ChangeNumber,
    code::NewCodeRef,
    space_names::SpaceSelector,
    threads::{
        Holder, MAX_CHOICES, MAX_POST_LEN, NewPost, Sender, Side, Thread, ThreadNumber, ThreadSpace,
    },
};

use super::{
    code::{self, NewCode},
    delivery::{self, Delivered},
    ensure_open,
    records::{self, Author, NewRecord},
};
use crate::{alerts::send_with_files, daemon::Records, model::Space, scope::Caller};
use shoal_protocol::files::{FileSource, NewFile, SentFile};

/// Refuse a post that is empty, too long, or offers too many choices.
fn validate(post: &NewPost) -> Result<()> {
    ensure!(
        !post.text.trim().is_empty()
            && post.text.len() <= MAX_POST_LEN
            && !post.text.chars().any(|c| c.is_control() && c != '\n'),
        "a post is nonempty text without control characters other than newlines (max {MAX_POST_LEN} bytes)"
    );
    ensure!(
        post.choices.len() <= MAX_CHOICES,
        "a post offers at most {MAX_CHOICES} choices"
    );
    for choice in &post.choices {
        crate::validate::message(choice).context("invalid choice")?;
    }
    for (field, value) in [
        ("recommendation", &post.recommendation),
        ("reason", &post.reason),
    ] {
        if let Some(value) = value {
            crate::validate::message(value).with_context(|| format!("invalid {field}"))?;
        }
    }
    Ok(())
}

/// `post` with the choice it picks checked against the choices the other
/// side of `thread` offered last, and its text that choice's when it has
/// none.
fn picked(thread: &Thread, side: Side, mut post: NewPost) -> Result<NewPost> {
    let Some(choice) = post.choice else {
        return Ok(post);
    };
    let offered = (thread.posts.iter().rev())
        .find(|earlier| {
            earlier.side.is_some_and(|earlier| earlier != side) && !earlier.post.choices.is_empty()
        })
        .map_or(&[][..], |earlier| &earlier.post.choices[..]);
    let text = (choice.checked_sub(1))
        .and_then(|index| offered.get(usize::try_from(index).ok()?))
        .with_context(|| match offered.len() {
            0 => format!("thread {} offers you no choices", thread.number),
            count => format!("choose a number from 1 to {count}"),
        })?;
    if post.text.trim().is_empty() {
        post.text = text.clone();
    }
    Ok(post)
}

/// Who posts: the side it takes, none for a participant, and the space it
/// posts from, none for the user.
pub struct Poster {
    pub side: Option<Side>,
    pub space: Option<ThreadSpace>,
    /// Who held the thread when the poster's side was checked.
    pub seen: Holder,
}

impl Poster {
    pub(super) fn author(&self) -> Author<'_> {
        Author {
            side: self.side,
            sender: match self.space {
                Some(_) => Sender::Space,
                None => Sender::User,
            },
            space: self.space.as_ref(),
        }
    }
}

/// What a post carries besides its text: copies of files, places in code
/// and changes, by their space and number.
#[derive(Clone, Copy, Default)]
pub struct Attached<'a> {
    pub(super) files: &'a [SentFile],
    pub(super) code: &'a [NewCode],
    pub(super) changes: &'a [(SpaceId, u32)],
}

/// The opening post of a thread about code: who posts it, and the places
/// in code it carries.
pub(super) struct Opening {
    pub(super) poster: Poster,
    pub(super) code: Vec<NewCode>,
}

impl Opening {
    /// Record the post in the thread `id` named `name`, in the transaction
    /// that records the thread, its text the thread's name, and deliver
    /// it; the thread as it is now.
    pub(super) fn record(
        &self,
        tx: &Transaction<'_>,
        id: i64,
        name: &str,
        outside: &dyn Records,
    ) -> Result<(Thread, Delivered)> {
        let post = NewPost {
            text: name.to_owned(),
            ..NewPost::default()
        };
        let attached = Attached {
            code: &self.code,
            ..Attached::default()
        };
        record_post(tx, id, &self.poster, &post, attached, false, outside)
    }
}

/// Record `post` with what it has `attached` from `poster` in the thread
/// `id`, which must still be open, and deliver it; the thread as it is
/// now. A post from the side of a space that opened the thread makes the
/// thread wait, which keeps that space, so a space a removal reserved
/// takes none.
pub fn record_post(
    tx: &Transaction<'_>,
    id: i64,
    poster: &Poster,
    post: &NewPost,
    attached: Attached<'_>,
    in_chat: bool,
    outside: &dyn Records,
) -> Result<(Thread, Delivered)> {
    let thread = records::reread(tx, id)?;
    ensure_open(&thread)?;
    if poster.side == Some(Side::Holder) {
        records::ensure_held_by(&thread, &poster.seen)?;
    }
    if poster.side == Some(Side::Opener) && thread.opener == Sender::Space {
        outside.require_taking_work(tx, &thread.space_id)?;
    }
    let recorded = records::insert_post(
        tx,
        &NewRecord {
            thread: id,
            author: poster.author(),
            post,
            files: attached.files,
            code: attached.code,
            in_chat,
            forwards: None,
        },
    )?;
    crate::changes::records::point(tx, recorded, attached.changes)?;
    let thread = records::reread(tx, id)?;
    records::ensure_fits(&thread)?;
    let mut delivered = Delivered::default();
    delivery::deliver_post(tx, outside, &thread, &mut delivered)?;
    Ok((records::reread(tx, id)?, delivered))
}

impl Coordination {
    /// Add `post` with copies of the files `attachments`, the places in
    /// code `code` and the changes `changes` to the thread `number` from
    /// `caller`'s side, or the user's without a caller, and deliver it to
    /// the other side.
    #[allow(
        clippy::too_many_arguments,
        reason = "what the post says, beside the daemon"
    )]
    pub async fn post_to_thread(
        &self,
        daemon: &dyn Daemon,
        caller: Option<&Caller>,
        number: ThreadNumber,
        post: NewPost,
        attachments: Vec<NewFile>,
        code: Vec<NewCodeRef>,
        changes: Vec<ChangeNumber>,
    ) -> Result<Thread> {
        shoal_protocol::files::check_sent(&attachments)?;
        let code = self.new_code(daemon, caller, code).await?;
        let changes = self.named_changes(daemon, caller, None, &changes).await?;
        let thread = self.readable_thread(daemon, caller, number).await?;
        let side = self.side(daemon, caller, &thread).await?;
        let post = picked(&thread, side, post)?;
        validate(&post)?;
        let poster = (self.poster(daemon, caller, Some(side), &thread.holder)).await?;
        let files_space = self.files_space(daemon, &poster, &thread).await?;
        let attacher = daemon.attacher(caller).await;
        let text = post.text.clone();
        let (id, outside) = (thread.id, self.records.clone());
        let versioned = code::versioned(&code);
        let (thread, delivered) = send_with_files(
            daemon,
            &files_space,
            FileSource::Thread,
            &text,
            attachments,
            &attacher,
            |files| {
                self.store.write(move |tx| {
                    let attached = Attached {
                        files: &files,
                        code: &code,
                        changes: &changes,
                    };
                    record_post(tx, id, &poster, &post, attached, false, &*outside)
                })
            },
        )
        .await?;
        self.announce_post(daemon, delivered, versioned).await;
        if super::findings::awaits_reviewer(&thread) {
            daemon.answer_posts();
        }
        Ok(thread)
    }

    /// Announce what a post `delivered` once it committed, and keep the
    /// versions of the code it points at, those of the spaces `versioned`
    /// names by repository.
    pub(super) async fn announce_post(
        &self,
        daemon: &dyn Daemon,
        delivered: Delivered,
        versioned: BTreeMap<RepositoryId, BTreeSet<SpaceId>>,
    ) {
        self.announce_threads(daemon, delivered).await;
        for (repository, spaces) in versioned {
            self.keep_code_versions(daemon, &repository, spaces).await;
        }
    }

    /// The space whose attached files keep the files `poster` sends in
    /// `thread`: the poster's own, or the thread's for the user or a
    /// poster on another machine.
    async fn files_space(
        &self,
        daemon: &dyn Daemon,
        poster: &Poster,
        thread: &Thread,
    ) -> Result<Space> {
        let own = match &poster.space {
            Some(space) => daemon.find_space(&space.id).await?,
            None => None,
        };
        match own {
            Some(space) => Ok(space),
            None => Ok(daemon.space(&SpaceSelector::id(&thread.space_id)).await?),
        }
    }

    /// Who posts for `caller` on `side`, none for a participant, checked
    /// while `seen` held the thread: its space, here or on another machine,
    /// or the user.
    pub(super) async fn poster(
        &self,
        daemon: &dyn Daemon,
        caller: Option<&Caller>,
        side: Option<Side>,
        seen: &Holder,
    ) -> Result<Poster> {
        let space = match caller {
            Some(caller) => Some(self.speaker(daemon, caller).await?),
            None => None,
        };
        Ok(Poster {
            side,
            space,
            seen: seen.clone(),
        })
    }
}
