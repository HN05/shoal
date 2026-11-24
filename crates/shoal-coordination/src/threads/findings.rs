//! The `review` threads of reviews: those Shoal opens for a local review's
//! findings, held by the reviewed workspace, opening one with its finding
//! as the opening post, which holds its posts until the review is
//! submitted, and reopening one a later review found still present; reading a review's threads, releasing those of a review its
//! writer submitted and dropping those of one it cancelled, each in the
//! review's own transaction, telling no one, since the review's one alert
//! names its threads; and posting the reviewer's answers in Shoal's, which
//! reach their holders.
use crate::{Coordination, daemon::Daemon};
use anyhow::{Context as _, Result};
use rusqlite::{Connection, Transaction, params};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::{code::CodeAt, reviews::Severity};
use shoal_protocol::{
    item_numbers::ItemNumber,
    reviews::ReviewId,
    threads::{
        Finding, MAX_POST_LEN, NewPost, OutcomeKind, Sender, Side, Thread, ThreadKind, ThreadSpace,
    },
    validate::{MAX_MESSAGE_LEN, shorten},
};
use shoal_refs::CommitId;

use super::{
    code::NewCode,
    delivery::{self, Delivered},
    records::{self, Author, NewRecord, NewThread, Opener},
};

/// A finding of a local review, whose thread Shoal opens in the reviewed
/// workspace, `workspace`.
pub struct FindingThread<'a> {
    pub review: ReviewId,
    pub workspace: &'a SpaceId,
    pub severity: Severity,
    pub title: &'a str,
    pub body: &'a str,
    /// Where the finding is in the reviewed commit.
    pub code: Vec<FindingCode<'a>>,
}

/// Where a finding is in the reviewed commit `commit`: its file, lines and
/// the blob of the file there, and the snippet of its lines.
pub struct FindingCode<'a> {
    pub commit: &'a CommitId,
    pub at: &'a CodeAt,
    pub snippet: Option<&'a str>,
}

/// A holder's post that ends a finding's thread, which waits for the
/// reviewer's answer: the thread's record and the post's.
pub struct AwaitedPost {
    pub thread: i64,
    pub post: i64,
}

/// Open the `review` thread of `finding` from Shoal in the workspace that
/// holds it, its title the thread's name and its body the opening post,
/// pointing at its code, holding its posts until its review is submitted;
/// the thread's record ID.
pub fn open(tx: &Transaction<'_>, finding: &FindingThread<'_>) -> Result<i64> {
    let name = shorten(one_line(finding.title), MAX_MESSAGE_LEN);
    let new = NewThread {
        kind: ThreadKind::Review,
        finding: Some(Finding {
            severity: Some(finding.severity),
            review: Some(finding.review),
        }),
        held: true,
        name: &name,
        opener: Opener::Shoal,
        space_id: finding.workspace,
        holder: Some(&ThreadSpace::here(finding.workspace.clone())),
    };
    let id = records::insert_thread(tx, &new)?;
    let code = (finding.code.iter())
        .map(|code| finding_code(tx, finding.workspace, code))
        .collect::<Result<Vec<_>>>()?;
    let body = match post_text(finding.body) {
        body if body.trim().is_empty() => name.clone(),
        body => body,
    };
    record_shoal_post(tx, id, &body, &code)?;
    Ok(id)
}

/// The reference to `code` the opening post of a finding's thread makes, in
/// the worktree of the workspace `workspace` that holds it.
fn finding_code(db: &Connection, workspace: &SpaceId, code: &FindingCode<'_>) -> Result<NewCode> {
    let (space_name, repository_id): (_, Option<_>) = db.query_row(
        "SELECT name,repository_id FROM spaces WHERE id=?1",
        [workspace],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    Ok(NewCode {
        space_id: workspace.clone(),
        space_name,
        repository_id: repository_id.context("a workspace belongs to a repository")?,
        commit: Some(code.commit.clone()),
        at: code.at.clone(),
        snippet: code.snippet.map(str::to_owned),
    })
}

/// The record ID of the thread `number` when it is a finding the workspace
/// `workspace` holds resolved `fixed`, which a later review may reopen.
pub fn fixed_finding(
    db: &Connection,
    workspace: &SpaceId,
    number: ItemNumber,
) -> Result<Option<i64>> {
    let thread = records::by_number(db, number)?;
    Ok(thread
        .filter(|thread| is_fixed_finding(thread, workspace))
        .map(|thread| thread.id))
}

/// Reopen the thread `id` with a post from Shoal saying `why`, while it is
/// a finding the workspace `workspace` holds resolved `fixed`; whether it
/// did.
pub fn reopen(tx: &Transaction<'_>, workspace: &SpaceId, id: i64, why: &str) -> Result<bool> {
    if !is_fixed_finding(&records::reread(tx, id)?, workspace) {
        return Ok(false);
    }
    records::set_resolution(tx, id, None)?;
    let text = format!(
        "A later review found this still present: {}",
        post_text(why)
    );
    record_shoal_post(tx, id, &text, &[])?;
    Ok(true)
}

/// Whether `thread` is a `review` thread Shoal opened that the workspace
/// `workspace` holds resolved `fixed`.
fn is_fixed_finding(thread: &Thread, workspace: &SpaceId) -> bool {
    let fixed = (thread.resolution.as_ref())
        .is_some_and(|resolution| resolution.outcome.kind() == OutcomeKind::Fixed);
    let held = thread
        .holder
        .space()
        .is_some_and(|holder| holder.id == *workspace);
    let shoals = thread.kind == ThreadKind::Review && thread.opener == Sender::Shoal;
    shoals && fixed && held
}

/// Delete the threads of review `review`, which its writer cancelled while
/// it was pending.
pub fn drop_of_review(tx: &Transaction<'_>, review: ReviewId) -> Result<()> {
    tx.execute("DELETE FROM threads WHERE review_id=?1", [review.0])?;
    Ok(())
}

/// Let the threads of review `review`, which its writer submitted, post as
/// any thread does.
pub fn release_of_review(tx: &Transaction<'_>, review: ReviewId) -> Result<()> {
    tx.execute("UPDATE threads SET held=0 WHERE review_id=?1", [review.0])?;
    Ok(())
}

/// The threads review `review` opened, oldest first.
pub fn of_review(db: &Connection, review: ReviewId) -> Result<Vec<Thread>> {
    records::query(db, "WHERE t.review_id=?1", [review.0])
}

/// The posts of their holders that end the open `review` threads Shoal
/// opened for a review, counting the posts that speak from a side, which
/// wait for the reviewer's answer.
pub fn awaiting_answers(db: &Connection) -> Result<Vec<AwaitedPost>> {
    Ok(db
        .prepare(
            "SELECT t.id,p.id FROM threads t JOIN thread_posts p ON p.id=(SELECT id
                    FROM thread_posts WHERE thread_id=t.id AND forwards=0 AND side IS NOT NULL
                        ORDER BY id DESC LIMIT 1)
                WHERE t.review_id IS NOT NULL AND t.kind='review' AND t.opener='shoal'
                AND t.outcome IS NULL AND t.space_number IS NOT NULL AND p.side='holder'
                ORDER BY t.id",
        )?
        .query_map([], |row| {
            Ok(AwaitedPost {
                thread: row.get(0)?,
                post: row.get(1)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?)
}

/// Whether `thread` waits for the reviewer's answer, as
/// [`awaiting_answers`] lists it.
pub fn awaits_reviewer(thread: &Thread) -> bool {
    let review = thread
        .finding
        .is_some_and(|finding| finding.review.is_some());
    review && thread.opener == Sender::Shoal && thread.awaits() == Some(Side::Opener)
}

impl Coordination {
    /// Post the reviewer's answer `text` from Shoal in the finding's thread
    /// `id`, telling its holder, unless it was resolved meanwhile.
    pub async fn answer_finding(&self, daemon: &dyn Daemon, id: i64, text: String) -> Result<()> {
        let text = post_text(&text);
        let outside = self.records.clone();
        let delivered = self
            .store
            .write(move |tx| {
                if !records::reread(tx, id)?.is_open() {
                    return Ok(None);
                }
                record_shoal_post(tx, id, &text, &[])?;
                let thread = records::reread(tx, id)?;
                records::ensure_fits(&thread)?;
                let mut delivered = Delivered::default();
                delivery::deliver_post(tx, &*outside, &thread, &mut delivered)?;
                Ok(Some(delivered))
            })
            .await?;
        if let Some(delivered) = delivered {
            self.announce_threads(daemon, delivered).await;
        }
        Ok(())
    }
}

/// The threads whose records are `ids`, in that order.
pub fn by_ids(db: &Connection, ids: &[i64]) -> Result<Vec<Thread>> {
    let mut threads = records::query(
        db,
        "WHERE t.id IN (SELECT value FROM json_each(?1))",
        params![serde_json::to_string(ids)?],
    )?;
    threads.sort_by_key(|thread| ids.iter().position(|id| *id == thread.id));
    Ok(threads)
}

/// Record a post from Shoal, on the opener's side, in the thread `id`
/// with `text` pointing at `code`, without delivering it.
fn record_shoal_post(tx: &Transaction<'_>, id: i64, text: &str, code: &[NewCode]) -> Result<()> {
    let post = NewPost {
        text: text.to_owned(),
        ..NewPost::default()
    };
    let new = NewRecord {
        thread: id,
        author: Author {
            side: Some(Side::Opener),
            sender: Sender::Shoal,
            space: None,
        },
        post: &post,
        files: &[],
        code,
        in_chat: false,
        forwards: None,
    };
    records::insert_post(tx, &new)?;
    Ok(())
}

/// `text` on one line, as a thread's name takes it.
fn one_line(text: &str) -> String {
    let text: String = (text.chars())
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    text.trim().to_owned()
}

/// `text` as a post takes it: lines without other control characters, as
/// long as a post may be.
fn post_text(text: &str) -> String {
    let text: String = (text.chars())
        .map(|c| match c {
            '\t' => ' ',
            c if c.is_control() && c != '\n' => ' ',
            c => c,
        })
        .collect();
    shorten(text.trim().to_owned(), MAX_POST_LEN)
}
