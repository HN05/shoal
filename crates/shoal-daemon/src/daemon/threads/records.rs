//! The rows of threads and their posts: reading them as protocol records,
//! and recording a thread, a post, a new holder and a resolution.
use std::collections::HashMap;

use anyhow::{Context as _, Result};
use rusqlite::{Connection, OptionalExtension as _, Transaction, params};
use shoal_ids::space_ids::{SpaceId, SpaceNumber};
use shoal_protocol::{
    changes::MAX_ENTRIES_BYTES,
    space_names::{MAX_SPACE_NAME_LEN, SpaceName},
    threads::{
        Finding, Holder, MAX_THREAD_BYTES, NewPost, Outcome, Post, Resolution, Sender, Side,
        Thread, ThreadKind, ThreadSpace,
    },
};

use super::code::NewCode;
use crate::daemon::{
    item_numbers::{self, ItemKind, ItemNumber},
    store::sent_files::{sent_files, sent_ids},
    workspace_files::SentFile,
};
use crate::time::UnixTime;

/// The filter on a `threads` row aliased `t` that keeps open threads.
pub(super) const OPEN: &str = "t.outcome IS NULL";

/// The filter that keeps the threads a number names: those of a space
/// that has a number.
const NUMBERED: &str = "t.space_number IS NOT NULL";

const THREADS: &str = "SELECT t.id,t.space_number,t.number,t.space_id,
        COALESCE(s.name,t.space_id) AS space,t.kind,t.name,t.created_at,t.opener,t.holder_id,
        COALESCE(h.name,t.holder_name) AS holder_name,
        COALESCE(h.number,t.holder_number) AS holder_number,t.outcome,t.resolved_by,t.resolver_id,
        t.resolved_at,t.severity,t.review_id,EXISTS (SELECT 1 FROM space_current_thread c
            WHERE c.thread_id=t.id AND c.space_id=t.holder_id) AS working
    FROM threads t LEFT JOIN spaces s ON s.id=t.space_id LEFT JOIN spaces h ON h.id=t.holder_id";

fn thread(row: &rusqlite::Row<'_>) -> rusqlite::Result<Thread> {
    let holder = match row.get::<_, Option<SpaceId>>("holder_id")? {
        None => Holder::User,
        Some(id) => Holder::Space(ThreadSpace {
            id,
            name: row.get("holder_name")?,
            number: row.get("holder_number")?,
        }),
    };
    let resolution = match crate::daemon::store::json_column::<Outcome>(row, "outcome")? {
        None => None,
        Some(outcome) => Some(Resolution {
            outcome,
            by: row.get("resolved_by")?,
            by_space: row.get("resolver_id")?,
            resolved_at: row.get("resolved_at")?,
        }),
    };
    let finding = match row.get("severity")? {
        Some(severity) => Some(Finding {
            severity,
            review: row.get("review_id")?,
        }),
        None => None,
    };
    Ok(Thread {
        id: row.get("id")?,
        number: ItemNumber {
            space: row.get("space_number")?,
            item: row.get("number")?,
        },
        space_id: row.get("space_id")?,
        space: row.get("space")?,
        kind: row.get("kind")?,
        finding,
        name: row.get("name")?,
        opener: row.get("opener")?,
        holder,
        created_at: row.get("created_at")?,
        posts: Vec::new(),
        resolution,
        working: row.get("working")?,
        changes: Vec::new(),
    })
}

const POSTS: &str = concat!(
    "SELECT p.id,p.thread_id,p.created_at,p.side,p.sender,p.sender_id,
        COALESCE(s.name,p.sender_name) AS sender_name,
        COALESCE(s.number,p.sender_number) AS sender_number,p.text,
        p.choices,p.recommendation,p.reason,p.choice,p.in_chat,p.forwards,p.forwarded_to,
        COALESCE(f.name,p.forwarded_name) AS forwarded_name,
        COALESCE(f.number,p.forwarded_number) AS forwarded_number,p.alert_id,",
    sent_files!("p.files"),
    " FROM thread_posts p LEFT JOIN spaces s ON s.id=p.sender_id
        LEFT JOIN spaces f ON f.id=p.forwarded_to"
);

fn post(row: &rusqlite::Row<'_>) -> rusqlite::Result<(i64, Post)> {
    let space = match row.get::<_, Option<SpaceId>>("sender_id")? {
        None => None,
        Some(id) => Some(ThreadSpace {
            id,
            name: row.get("sender_name")?,
            number: row.get("sender_number")?,
        }),
    };
    let forwarded_to = match (row.get::<_, bool>("forwards")?, row.get("forwarded_to")?) {
        (false, _) => None,
        (true, None) => Some(Holder::User),
        (true, Some(id)) => Some(Holder::Space(ThreadSpace {
            id,
            name: row.get("forwarded_name")?,
            number: row.get("forwarded_number")?,
        })),
    };
    let post = Post {
        id: row.get("id")?,
        created_at: row.get("created_at")?,
        side: row.get("side")?,
        sender: row.get("sender")?,
        space,
        post: NewPost {
            text: row.get("text")?,
            choices: (crate::daemon::store::json_column(row, "choices")?).unwrap_or_default(),
            recommendation: row.get("recommendation")?,
            reason: row.get("reason")?,
            choice: row.get("choice")?,
        },
        files: (crate::daemon::store::json_column(row, "files")?).unwrap_or_default(),
        code: Vec::new(),
        changes: Vec::new(),
        in_chat: row.get("in_chat")?,
        forwarded_to,
        alert_id: row.get("alert_id")?,
    };
    Ok((row.get("thread_id")?, post))
}

/// The threads `filter` keeps, with the parameters `params`, oldest first,
/// each with its posts.
pub(super) fn query(
    db: &Connection,
    filter: &str,
    params: impl rusqlite::Params,
) -> Result<Vec<Thread>> {
    let mut threads = db
        .prepare(&format!("{THREADS} {filter} ORDER BY t.id"))?
        .query_map(params, thread)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let ids: Vec<i64> = threads.iter().map(|thread| thread.id).collect();
    let mut posts = posts_of(db, &ids)?;
    let times: HashMap<i64, Vec<UnixTime>> = (posts.iter())
        .map(|(thread, posts)| {
            let speaking = posts.iter().filter(|post| post.speaks());
            (*thread, speaking.map(|post| post.created_at).collect())
        })
        .collect();
    let mut changes = crate::daemon::changes::records::entries(db, &ids, &times)?;
    for thread in &mut threads {
        thread.posts = posts.remove(&thread.id).unwrap_or_default();
        thread.changes = changes.remove(&thread.id).unwrap_or_default();
    }
    Ok(threads)
}

/// The posts of each of the threads `threads`, oldest first, with the
/// code and changes each points at.
fn posts_of(db: &Connection, threads: &[i64]) -> Result<HashMap<i64, Vec<Post>>> {
    let found = db
        .prepare(&format!(
            "{POSTS} WHERE p.thread_id IN (SELECT value FROM json_each(?1)) ORDER BY p.id"
        ))?
        .query_map([serde_json::to_string(threads)?], post)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let post_ids: Vec<i64> = found.iter().map(|(_, post)| post.id).collect();
    let mut code = super::code::records::of_posts(db, &post_ids)?;
    let mut changes = crate::daemon::changes::records::of_posts(db, &post_ids)?;
    let mut posts: HashMap<i64, Vec<Post>> = HashMap::new();
    for (thread, mut post) in found {
        post.code = code.remove(&post.id).unwrap_or_default();
        post.changes = changes.remove(&post.id).unwrap_or_default();
        posts.entry(thread).or_default().push(post);
    }
    Ok(posts)
}

/// The thread `number` names, while its space has that number.
pub(super) fn by_number(db: &Connection, number: ItemNumber) -> Result<Option<Thread>> {
    Ok(query(
        db,
        "WHERE t.space_number=?1 AND t.number=?2",
        params![number.space, number.item],
    )?
    .pop())
}

/// The thread whose record is `id`.
pub(super) fn by_id(db: &Connection, id: i64) -> Result<Option<Thread>> {
    Ok(query(db, "WHERE t.id=?1", [id])?.pop())
}

/// The thread `item` of the space `space_id`.
pub(super) fn by_item(db: &Connection, space_id: &SpaceId, item: u32) -> Result<Option<Thread>> {
    Ok(query(
        db,
        &format!("WHERE t.space_id=?1 AND t.number=?2 AND {NUMBERED}"),
        params![space_id, item],
    )?
    .pop())
}

/// The thread `number`, refusing a number that names none, or that names
/// an approval request.
pub(super) fn known(db: &Connection, number: ItemNumber) -> Result<Thread> {
    match by_number(db, number)? {
        Some(thread) => Ok(thread),
        None => Err(item_numbers::unknown(db, ItemKind::Thread, number)?),
    }
}

/// The numbered threads `space` takes part in, as the space it is numbered
/// in, its holder, an earlier holder or a space that linked it, or every
/// numbered thread without one, with `state` kept by `kept`.
pub(super) fn of_space(
    db: &Connection,
    space: Option<&SpaceId>,
    kept: &str,
) -> Result<Vec<Thread>> {
    query(
        db,
        &format!(
            "WHERE {NUMBERED} AND {kept} AND (?1 IS NULL OR t.space_id=?1 OR t.holder_id=?1
                OR EXISTS (SELECT 1 FROM thread_holders WHERE thread_id=t.id AND space_id=?1)
                OR EXISTS (SELECT 1 FROM thread_links WHERE thread_id=t.id AND space_id=?1))"
        ),
        [space],
    )
}

/// Every open thread with a post, oldest first.
pub(super) fn open_with_posts(db: &mut Connection) -> Result<Vec<Thread>> {
    query(
        db,
        &format!(
            "WHERE {NUMBERED} AND {OPEN}
                AND EXISTS (SELECT 1 FROM thread_posts WHERE thread_id=t.id)"
        ),
        [],
    )
}

/// The spaces every holder of the thread `id` has been.
pub(super) fn holders(db: &Connection, id: i64) -> Result<Vec<SpaceId>> {
    Ok(db
        .prepare("SELECT space_id FROM thread_holders WHERE thread_id=?1")?
        .query_map([id], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}

/// The spaces that linked the thread `id`.
pub(super) fn linkers(db: &Connection, id: i64) -> Result<Vec<SpaceId>> {
    Ok(db
        .prepare("SELECT space_id FROM thread_links WHERE thread_id=?1")?
        .query_map([id], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}

/// What a new thread is: its kind and name, who opened it, the space it is
/// numbered in and who holds it, none for the user, and a `review` thread's
/// finding. A space it names keeps the number and name it has now, which
/// name it while this machine has no record of it.
pub(in crate::daemon) struct NewThread<'a> {
    pub(super) kind: ThreadKind,
    pub(super) finding: Option<Finding>,
    pub(super) name: &'a str,
    pub(super) opener: Sender,
    pub(super) space_id: &'a SpaceId,
    pub(super) holder: Option<&'a ThreadSpace>,
}

/// Record `new` under its space's next number; the thread's record ID.
pub(super) fn insert_thread(tx: &Transaction<'_>, new: &NewThread<'_>) -> Result<i64> {
    let number = item_numbers::next(tx, new.space_id)?;
    tx.execute(
        "INSERT INTO threads(space_id,space_number,number,kind,name,created_at,opener,holder_id,
                holder_name,holder_number,severity,review_id)
            VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
        params![
            new.space_id,
            number.space,
            number.item,
            new.kind,
            new.name,
            crate::time::UnixTime::now(),
            new.opener,
            new.holder.map(|holder| &holder.id),
            new.holder.and_then(|holder| holder.name.as_ref()),
            new.holder.and_then(|holder| holder.number),
            new.finding.map(|finding| finding.severity),
            new.finding.and_then(|finding| finding.review)
        ],
    )?;
    let id = tx.last_insert_rowid();
    if let Some(holder) = new.holder {
        add_holder(tx, id, &holder.id)?;
    }
    Ok(id)
}

/// Who sends a post, from which side, and the space it sends from.
pub(super) struct Author<'a> {
    pub(super) side: Side,
    pub(super) sender: Sender,
    pub(super) space: Option<&'a ThreadSpace>,
}

/// A post to record in the thread `thread`: `post` with the files `files`
/// and the places in code `code`, in the chat when `in_chat`, or
/// forwarding the thread to `forwards`, `Some(None)` for the user.
pub(super) struct NewRecord<'a> {
    pub(super) thread: i64,
    pub(super) author: Author<'a>,
    pub(super) post: &'a NewPost,
    pub(super) files: &'a [SentFile],
    pub(super) code: &'a [NewCode],
    pub(super) in_chat: bool,
    pub(super) forwards: Option<Option<&'a ThreadSpace>>,
}

/// Record `new`; the post's ID.
pub(super) fn insert_post(tx: &Transaction<'_>, new: &NewRecord<'_>) -> Result<i64> {
    let sender = new.author.space;
    let forwarded_to = new.forwards.flatten();
    tx.execute(
        "INSERT INTO thread_posts(thread_id,created_at,side,sender,sender_id,sender_name,
                sender_number,text,choices,recommendation,reason,choice,files,in_chat,forwards,
                forwarded_to,forwarded_name,forwarded_number)
            VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18)",
        params![
            new.thread,
            crate::time::UnixTime::now(),
            new.author.side,
            new.author.sender,
            sender.map(|space| &space.id),
            sender.and_then(|space| space.name.as_ref()),
            sender.and_then(|space| space.number),
            new.post.text,
            serde_json::to_string(&new.post.choices)?,
            new.post.recommendation,
            new.post.reason,
            new.post.choice,
            sent_ids(new.files)?,
            new.in_chat,
            new.forwards.is_some(),
            forwarded_to.map(|space| &space.id),
            forwarded_to.and_then(|space| space.name.as_ref()),
            forwarded_to.and_then(|space| space.number)
        ],
    )?;
    let post = tx.last_insert_rowid();
    for code in new.code {
        super::code::records::insert(tx, post, code)?;
    }
    Ok(post)
}

/// Replace the text of the post `post` with `text`.
pub(super) fn replace_text(tx: &Transaction<'_>, post: i64, text: &str) -> Result<()> {
    tx.execute(
        "UPDATE thread_posts SET text=?2 WHERE id=?1",
        params![post, text],
    )?;
    Ok(())
}

/// Delete Shoal's posts in the thread `id` made at `since` or later, as the
/// ones that told its opener a closing did; how many there were.
pub(super) fn drop_shoal_posts_since(
    tx: &Transaction<'_>,
    id: i64,
    since: Option<crate::time::UnixTime>,
) -> Result<usize> {
    Ok(tx.execute(
        "DELETE FROM thread_posts WHERE thread_id=?1 AND sender='shoal' AND created_at>=?2",
        params![id, since],
    )?)
}

/// Record that the alert `alert` carried the post `post`.
pub(super) fn set_alert(tx: &Transaction<'_>, post: i64, alert: i64) -> Result<()> {
    tx.execute(
        "UPDATE thread_posts SET alert_id=?2 WHERE id=?1",
        params![post, alert],
    )?;
    Ok(())
}

/// Give the thread `id` to `holder`, none for the user.
pub(super) fn set_holder(
    tx: &Transaction<'_>,
    id: i64,
    holder: Option<&ThreadSpace>,
) -> Result<()> {
    tx.execute(
        "UPDATE threads SET holder_id=?2,holder_name=?3,holder_number=?4 WHERE id=?1",
        params![
            id,
            holder.map(|holder| &holder.id),
            holder.and_then(|holder| holder.name.as_ref()),
            holder.and_then(|holder| holder.number)
        ],
    )?;
    if let Some(holder) = holder {
        add_holder(tx, id, &holder.id)?;
    }
    Ok(())
}

fn add_holder(tx: &Transaction<'_>, id: i64, holder: &SpaceId) -> Result<()> {
    tx.execute(
        "INSERT OR IGNORE INTO thread_holders(thread_id,space_id) VALUES (?1,?2)",
        params![id, holder],
    )?;
    Ok(())
}

/// Record `resolution` on the thread `id`, or reopen it without one.
pub(super) fn set_resolution(
    tx: &Transaction<'_>,
    id: i64,
    resolution: Option<&Resolution>,
) -> Result<()> {
    let outcome = match resolution {
        Some(resolution) => Some(serde_json::to_string(&resolution.outcome)?),
        None => None,
    };
    tx.execute(
        "UPDATE threads SET outcome=?2,resolved_by=?3,resolver_id=?4,resolved_at=?5 WHERE id=?1",
        params![
            id,
            outcome,
            resolution.map(|resolution| resolution.by),
            resolution.and_then(|resolution| resolution.by_space.as_ref()),
            resolution.map(|resolution| resolution.resolved_at)
        ],
    )?;
    Ok(())
}

/// Link the thread `id` for the space `space`, or unlink it; whether that
/// changed anything.
pub(super) fn set_link(
    tx: &Transaction<'_>,
    space: &SpaceId,
    id: i64,
    linked: bool,
) -> Result<bool> {
    let changed = if linked {
        tx.execute(
            "INSERT OR IGNORE INTO thread_links(space_id,thread_id) VALUES (?1,?2)",
            params![space, id],
        )?
    } else {
        tx.execute(
            "DELETE FROM thread_links WHERE space_id=?1 AND thread_id=?2",
            params![space, id],
        )?
    };
    Ok(changed > 0)
}

/// Refuse `thread` once it takes more than one response carries, which
/// rolls back the post that made it so. It is measured with every space it
/// names at its longest name and number, since a rename or a renumbering
/// of a space it names later changes what it takes, and with its change
/// entries at their budget, since changes keep coming without a post.
pub(super) fn ensure_fits(thread: &Thread) -> Result<()> {
    let posts = serde_json::to_vec(&at_longest(thread))?.len();
    anyhow::ensure!(
        posts + MAX_ENTRIES_BYTES <= MAX_THREAD_BYTES,
        "thread {} would take more than {MAX_THREAD_BYTES} bytes; open a new thread to go on",
        thread.number
    );
    Ok(())
}

/// `thread` with the names and numbers of the spaces it names, and the
/// alerts of its posts, as long as they get.
fn at_longest(thread: &Thread) -> Thread {
    let name = SpaceName::derive(&"\u{10FFFF}".repeat(MAX_SPACE_NAME_LEN), "");
    let number = Some(SpaceNumber::new(u32::MAX));
    let longest = |space: &mut ThreadSpace| {
        space.name = Some(name.clone());
        space.number = number;
    };
    let mut thread = thread.clone();
    thread.space = name.clone();
    thread.number.space = SpaceNumber::new(u32::MAX);
    if let Holder::Space(holder) = &mut thread.holder {
        longest(holder);
    }
    thread.changes.clear();
    for post in &mut thread.posts {
        post.space.iter_mut().for_each(longest);
        for change in &mut post.changes {
            longest(&mut change.change.space);
        }
        if let Some(Holder::Space(to)) = &mut post.forwarded_to {
            longest(to);
        }
        post.alert_id = Some(i64::MAX);
    }
    thread
}

/// Refuse `thread` once it went to another holder than `seen`, the one
/// whose side a caller was checked to post or forward from, which it may
/// have lost meanwhile.
pub(super) fn ensure_held_by(thread: &Thread, seen: &Holder) -> Result<()> {
    let id = |holder: &Holder| holder.space().map(|space| space.id.clone());
    anyhow::ensure!(
        id(&thread.holder) == id(seen),
        "thread {} went to {} meanwhile; act on it again if it is still yours",
        thread.number,
        thread.holder.label()
    );
    Ok(())
}

/// The thread `id` as recorded now, which the caller just changed.
pub(super) fn reread(db: &Connection, id: i64) -> Result<Thread> {
    by_id(db, id)?.context("the thread was deleted meanwhile")
}

/// Renumber the thread `id` as the next item of the space `to`, which it
/// is numbered in from now on.
pub(super) fn renumber(tx: &Transaction<'_>, id: i64, to: &SpaceId) -> Result<ItemNumber> {
    let number = item_numbers::next(tx, to)?;
    tx.execute(
        "UPDATE threads SET space_id=?2,space_number=?3,number=?4 WHERE id=?1",
        params![id, to, number.space, number.item],
    )?;
    Ok(number)
}

/// The space `id` as text names it: by its number and name while it has
/// them, else by its ID.
pub(super) fn label(db: &Connection, id: &SpaceId) -> Result<String> {
    let named: Option<(Option<shoal_ids::space_ids::SpaceNumber>, SpaceName)> = db
        .query_row("SELECT number,name FROM spaces WHERE id=?1", [id], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .optional()?;
    Ok(match named {
        Some((Some(number), name)) => shoal_protocol::space_names::label(number, &name),
        Some((None, name)) => name.to_string(),
        None => id.to_string(),
    })
}
