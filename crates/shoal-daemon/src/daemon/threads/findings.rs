//! The `review` threads Shoal opens for a local review's findings, held by
//! the reviewed workspace: opening one with its finding as the opening
//! post, reopening one a later review found still present, and reading a
//! review's. Each runs in the review's own transaction and tells no one,
//! since the review's one alert names its threads.
use anyhow::{Context as _, Result};
use rusqlite::{Connection, Transaction, params};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::{
    item_numbers::ItemNumber,
    reviews::ReviewId,
    threads::{Finding, MAX_POST_LEN, NewPost, OutcomeKind, Sender, Side, Thread, ThreadKind},
    validate::{MAX_MESSAGE_LEN, shorten},
};
use shoal_reviews::daemon::FindingThread;

use super::{
    code::NewCode,
    records::{self, Author, NewRecord, NewThread},
};

/// Open the `review` thread of `finding` from Shoal in the workspace that
/// holds it, its title the thread's name and its body the opening post,
/// pointing at its code.
pub(in crate::daemon) fn open(tx: &Transaction<'_>, finding: &FindingThread<'_>) -> Result<()> {
    let name = shorten(one_line(finding.title), MAX_MESSAGE_LEN);
    let new = NewThread {
        kind: ThreadKind::Review,
        finding: Some(Finding {
            severity: finding.severity,
            review: Some(finding.review),
        }),
        name: &name,
        opener: Sender::Shoal,
        space_id: finding.workspace,
        holder: Some(finding.workspace),
    };
    let id = records::insert_thread(tx, &new)?;
    let code = finding_code(tx, finding)?;
    let body = match post_text(finding.body) {
        body if body.trim().is_empty() => name.clone(),
        body => body,
    };
    record_shoal_post(tx, id, &body, &[code])
}

/// The reference the opening post of `finding`'s thread makes, in the
/// worktree of the workspace that holds it.
fn finding_code(db: &Connection, finding: &FindingThread<'_>) -> Result<NewCode> {
    let (space_name, repository_id): (_, Option<_>) = db.query_row(
        "SELECT name,repository_id FROM spaces WHERE id=?1",
        [finding.workspace],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    Ok(NewCode {
        space_id: finding.workspace.clone(),
        space_name,
        repository_id: repository_id.context("a workspace belongs to a repository")?,
        commit: Some(finding.code.commit.clone()),
        at: finding.code.at.clone(),
        snippet: finding.code.snippet.clone(),
    })
}

/// Reopen the thread `number` with a post from Shoal saying `why`, when it
/// is a `review` thread the workspace `workspace` holds resolved `fixed`;
/// the thread's record ID, none for any other.
pub(in crate::daemon) fn reopen(
    tx: &Transaction<'_>,
    workspace: &SpaceId,
    number: ItemNumber,
    why: &str,
) -> Result<Option<i64>> {
    let Some(thread) = records::by_number(tx, number)? else {
        return Ok(None);
    };
    let fixed = (thread.resolution.as_ref())
        .is_some_and(|resolution| resolution.outcome.kind() == OutcomeKind::Fixed);
    let held = thread
        .holder
        .space()
        .is_some_and(|holder| holder.id == *workspace);
    if thread.kind != ThreadKind::Review || !fixed || !held {
        return Ok(None);
    }
    records::set_resolution(tx, thread.id, None)?;
    let text = format!(
        "A later review found this still present: {}",
        post_text(why)
    );
    record_shoal_post(tx, thread.id, &text, &[])?;
    Ok(Some(thread.id))
}

/// The threads review `review` opened, oldest first.
pub(in crate::daemon) fn of_review(db: &Connection, review: ReviewId) -> Result<Vec<Thread>> {
    records::query(db, "WHERE t.review_id=?1", [review.0])
}

/// The threads whose records are `ids`, in that order.
pub(in crate::daemon) fn by_ids(db: &Connection, ids: &[i64]) -> Result<Vec<Thread>> {
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
            side: Side::Opener,
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
