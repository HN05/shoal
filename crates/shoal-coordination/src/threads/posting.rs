//! Posting in a thread: the post's checks, the surface it offers and the
//! one it answers, and recording and delivering it from the poster's side,
//! or from a participant on neither.
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
        Answer, Holder, MAX_POST_LEN, NewPost, Sender, Side, Thread, ThreadNumber, ThreadSpace,
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

/// Refuse a post's text that is empty or too long.
pub(super) fn validate_text(text: &str) -> Result<()> {
    ensure!(
        !text.trim().is_empty()
            && text.len() <= MAX_POST_LEN
            && !text.chars().any(|c| c.is_control() && c != '\n'),
        "a post is nonempty text without control characters other than newlines (max {MAX_POST_LEN} bytes)"
    );
    Ok(())
}

/// `post` as it is recorded from `side` of `thread`: its surface checked,
/// its images naming files of `attached`, and its answer resolved against
/// the latest surface the other side posted, with that action's label as
/// its text when it has none. A participant's post, on neither side,
/// offers no surface and answers none.
fn interacting(
    thread: &Thread,
    side: Option<Side>,
    mut post: NewPost,
    attached: &[&str],
) -> Result<NewPost> {
    let Some(side) = side else {
        ensure!(
            post.ui.is_none() && post.answer.is_none(),
            "you post in thread {} as a participant, on neither side, which offers no surface \
             and answers none",
            thread.number
        );
        return Ok(post);
    };
    post.ui = (post.ui).map(|ui| ui.checked(attached)).transpose()?;
    if let Some(answer) = post.answer.take() {
        let (answer, label) = answered(thread, side, answer)?;
        if post.text.trim().is_empty() {
            post.text = label;
        }
        post.answer = Some(answer);
    }
    Ok(post)
}

/// `answer` from `side` of `thread` as it is recorded: on the latest
/// surface the other side posted, the only one that takes actions, with
/// the action's whole context; and the action's label.
fn answered(thread: &Thread, side: Side, answer: Answer) -> Result<(Answer, String)> {
    let other = thread.party(side.other());
    let offer = (thread.surface_from(side.other())).with_context(|| {
        format!(
            "thread {} has no surface from {other} to answer",
            thread.number
        )
    })?;
    let named = |id: i64| {
        (thread.post_number(id)).map_or_else(|| id.to_string(), |number| format!("#{number}"))
    };
    if let Some(post) = answer.post {
        ensure!(
            post == offer.id,
            "post {} of thread {} is not the latest surface {other} posted, which alone takes \
             actions; answer {}",
            named(post),
            thread.number,
            named(offer.id)
        );
    }
    let surface = (offer.post.ui.as_ref()).context("the offer has a surface")?;
    let values = surface.resolve(&answer.action, answer.values)?;
    let label = (surface.action(&answer.action)).map_or_else(String::new, |action| action.label);
    let answer = Answer {
        post: Some(offer.id),
        action: answer.action,
        values,
    };
    Ok((answer, label))
}

/// Who posts: the side it takes, none for a participant, who sends it, and
/// the space it posts from, none for the user or Shoal.
pub struct Poster {
    pub side: Option<Side>,
    pub sender: Sender,
    pub space: Option<ThreadSpace>,
    /// Who held the thread when the poster's side was checked.
    pub seen: Holder,
}

impl Poster {
    pub(super) fn author(&self) -> Author<'_> {
        Author {
            side: self.side,
            sender: self.sender,
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

/// The opening post of a thread: who posts it, its text, and the places in
/// code it carries.
pub(super) struct Opening {
    pub(super) poster: Poster,
    pub(super) text: String,
    pub(super) code: Vec<NewCode>,
}

impl Opening {
    /// Record the post in the thread `id`, in the transaction that records
    /// the thread, and deliver it; the thread as it is now.
    pub(super) fn record(
        &self,
        tx: &Transaction<'_>,
        id: i64,
        outside: &dyn Records,
    ) -> Result<(Thread, Delivered)> {
        let post = NewPost {
            text: self.text.clone(),
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
/// takes none. A participant's post links the thread for its space, which
/// hears the replies.
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
    match poster.side {
        Some(Side::Holder) => records::ensure_held_by(&thread, &poster.seen)?,
        Some(Side::Opener) if thread.opener == Sender::Space => {
            outside.require_taking_work(tx, &thread.space_id)?;
        }
        Some(Side::Opener) => {}
        None => {
            if let Some(space) = &poster.space {
                records::link_here(tx, &space.id, id)?;
            }
        }
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
    /// the side `caller` posts on, or the user's without a caller, and
    /// deliver it.
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
        let side = self.posting_side(daemon, caller, &thread).await?;
        let attached: Vec<&str> = (attachments.iter())
            .filter_map(|file| file.path.file_name()?.to_str())
            .collect();
        let post = interacting(&thread, side, post, &attached)?;
        validate_text(&post.text)?;
        let poster = self.poster(daemon, caller, side, &thread.holder).await?;
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
    /// Shoal for an answer run, or the user.
    pub(super) async fn poster(
        &self,
        daemon: &dyn Daemon,
        caller: Option<&Caller>,
        side: Option<Side>,
        seen: &Holder,
    ) -> Result<Poster> {
        let (sender, space) = match caller {
            Some(caller) if caller.access.answer().is_some() => (Sender::Shoal, None),
            Some(caller) => (Sender::Space, Some(self.speaker(daemon, caller).await?)),
            None => (Sender::User, None),
        };
        Ok(Poster {
            side,
            sender,
            space,
            seen: seen.clone(),
        })
    }
}
