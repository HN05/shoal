//! Posting in a thread: the post's checks, the choice it picks, and
//! recording and delivering it from the poster's side.
use anyhow::{Context as _, Result, ensure};
use rusqlite::Transaction;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::{
    space_names::{SpaceName, SpaceSelector},
    threads::{MAX_CHOICES, MAX_POST_LEN, NewPost, Sender, Side, Thread, ThreadNumber},
};

use super::{
    delivery::{self, Delivered},
    ensure_open,
    records::{self, Author, NewRecord},
};
use crate::daemon::{
    manager::Manager,
    scope::Caller,
    spaces::Removing,
    workspace_files::{self, FileSource, NewFile, SentFile},
};

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
        .find(|earlier| earlier.side != side && !earlier.post.choices.is_empty())
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

/// Who posts: the side it takes, and the space it posts from, none for
/// the user.
pub(super) struct Poster {
    pub(super) side: Side,
    pub(super) space: Option<(SpaceId, SpaceName)>,
}

impl Poster {
    pub(super) fn author(&self) -> Author<'_> {
        Author {
            side: self.side,
            sender: match self.space {
                Some(_) => Sender::Space,
                None => Sender::User,
            },
            space: self.space.as_ref().map(|(id, name)| (id, name)),
        }
    }
}

/// Record `post` with `files` from `poster` in the thread `id`, which must
/// still be open, and deliver it; the thread as it is now. A post from the
/// side of a space that opened the thread makes the thread wait, which
/// keeps that space, so a space `removing` reserved takes none.
pub(super) fn record_post(
    tx: &Transaction<'_>,
    id: i64,
    poster: &Poster,
    post: &NewPost,
    files: &[SentFile],
    in_chat: bool,
    removing: &Removing,
) -> Result<(Thread, Delivered)> {
    let thread = records::reread(tx, id)?;
    ensure_open(&thread)?;
    if poster.side == Side::Opener && thread.opener == Sender::Space {
        removing.require_taking_work(tx, &thread.space_id)?;
    }
    records::insert_post(
        tx,
        &NewRecord {
            thread: id,
            author: poster.author(),
            post,
            files,
            in_chat,
            forwards: None,
        },
    )?;
    let thread = records::reread(tx, id)?;
    records::ensure_fits(&thread)?;
    let mut delivered = Delivered::default();
    delivery::deliver_post(tx, &thread, &mut delivered)?;
    Ok((records::reread(tx, id)?, delivered))
}

impl Manager {
    /// Add `post` with copies of the files `attachments` to the thread
    /// `number` from `caller`'s side, or the user's without a caller, and
    /// deliver it to the other side.
    pub async fn post_to_thread(
        &self,
        caller: Option<&Caller>,
        number: ThreadNumber,
        post: NewPost,
        attachments: Vec<NewFile>,
    ) -> Result<Thread> {
        workspace_files::check_sent(&attachments)?;
        let thread = self.readable_thread(caller, number).await?;
        let side = self.side(caller, &thread).await?;
        let post = picked(&thread, side, post)?;
        validate(&post)?;
        let poster = self.poster(caller, side).await?;
        // A poster on another machine keeps its files in the thread's space.
        let files_space = match &poster.space {
            Some((id, _)) => self.find_space(id).await?,
            None => None,
        };
        let files_space = match files_space {
            Some(space) => space,
            None => self.space(&SpaceSelector::id(&thread.space_id)).await?,
        };
        let attacher = self.attacher(caller).await;
        let text = post.text.clone();
        let (id, removing) = (thread.id, self.space_removals.clone());
        let (thread, delivered) = self
            .send_with_files(
                &files_space.into(),
                FileSource::Thread,
                &text,
                attachments,
                &attacher,
                |files| {
                    self.store.write(move |tx| {
                        record_post(tx, id, &poster, &post, &files, false, &removing)
                    })
                },
            )
            .await?;
        self.announce_threads(delivered).await;
        Ok(thread)
    }

    /// Who posts for `caller` on `side`: its space, here or on another
    /// machine, or the user.
    pub(super) async fn poster(&self, caller: Option<&Caller>, side: Side) -> Result<Poster> {
        let space = match caller {
            Some(caller) => Some(self.speaker(caller).await?),
            None => None,
        };
        Ok(Poster { side, space })
    }
}
