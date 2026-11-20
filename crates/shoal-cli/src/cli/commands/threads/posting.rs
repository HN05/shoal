//! `shoal send <thread>`: post in a thread, optionally offering choices
//! with a recommendation or picking one the other side offered, wait for
//! the other side's next post, or post the turn's final message from the
//! agent's chat instead.
use std::path::PathBuf;

use anyhow::{Context as _, Result};
use shoal_protocol::{
    space_names::SpaceSelector,
    threads::{NewPost, Post, Side, Thread, ThreadNumber},
};

use crate::{
    cli::{
        args::threads::PostArgs,
        context::Context,
        installed::{arguments, replace_option},
        ui::{self, Fallback},
    },
    client::threads,
    installed::Executable,
};

/// A post `shoal send` makes in a thread.
pub(in crate::cli) struct Posting {
    pub(in crate::cli) thread: ThreadNumber,
    /// The text given inline; none with --body-file, --choose or --in-chat.
    pub(in crate::cli) text: Option<String>,
    pub(in crate::cli) body_file: Option<PathBuf>,
    pub(in crate::cli) attach: Vec<String>,
    pub(in crate::cli) options: PostArgs,
}

pub(in crate::cli) async fn post(ctx: &Context, mut posting: Posting) -> Result<i32> {
    let from = super::acting_space(ctx).await?;
    if let Some(after) = posting.options.resume {
        return resume(ctx, posting.thread, after).await;
    }
    if posting.options.in_chat {
        return in_chat(ctx, posting.thread).await;
    }
    let thread = posting.thread;
    let wait = posting.options.wait;
    let attachments = crate::cli::commands::send::attachments(&posting.attach)?;
    shoal_protocol::files::check_sent(&attachments)?;
    let code = super::code_refs(std::mem::take(&mut posting.options.code))?;
    let post = new_post(posting)?;
    let posted = threads::post(&ctx.paths, thread, post, attachments, code, from).await?;
    let mine = posted.posts.last().context("the post was not recorded")?;
    if !wait {
        let to = match mine.side {
            Side::Opener => Side::Holder,
            Side::Holder => Side::Opener,
        };
        let message = format!(
            "Posted in thread {}, for {}.",
            posted.number,
            posted.party(to)
        );
        ctx.emit(&message, &posted)?;
        return Ok(0);
    }
    let after = mine.id;
    let replied = await_reply(ctx, &posted, after, false).await?;
    print_reply(ctx, &replied, after).await
}

/// What `posting` says, its text read from `--body-file` when it names
/// one, without the file's trailing newline.
fn new_post(posting: Posting) -> Result<NewPost> {
    let Posting {
        text,
        body_file,
        options,
        ..
    } = posting;
    let text = match body_file {
        Some(path) => std::fs::read_to_string(&path)
            .with_context(|| format!("read {}", path.display()))?
            .trim_end()
            .to_owned(),
        None => text.unwrap_or_default(),
    };
    Ok(NewPost {
        text,
        choices: options.choices,
        recommendation: options.recommend,
        reason: options.reason,
        choice: options.choose,
    })
}

/// Mark the agent's turn as posting its final message in the thread
/// `number`.
async fn in_chat(ctx: &Context, number: ThreadNumber) -> Result<i32> {
    let space = ui::select_space(ctx, None, Fallback::OwnWorkspace).await?;
    let thread = threads::post_in_chat(&ctx.paths, space, number).await?;
    ctx.emit(
        &format!(
            "Write your post as your final message and end your turn; it goes to thread {}.",
            thread.number
        ),
        &thread,
    )?;
    Ok(0)
}

/// Continue an earlier `send --wait` after the post `after` of the thread
/// `number`.
async fn resume(ctx: &Context, number: ThreadNumber, after: i64) -> Result<i32> {
    let thread = threads::show(&ctx.paths, number).await?;
    let replied = await_reply(ctx, &thread, after, true).await?;
    print_reply(ctx, &replied, after).await
}

/// `thread` once the other side posts after the post `after`, or it is
/// resolved. Waits across daemon restarts; after an upgrade, the
/// installed release resumes the wait.
async fn await_reply(
    ctx: &Context,
    thread: &Thread,
    after: i64,
    restarted: bool,
) -> Result<Thread> {
    let space = SpaceSelector::id(&thread.space_id);
    let item = thread.number.item;
    let executable = Executable::current();
    threads::await_post(&ctx.paths, &space, item, after, restarted)
        .await
        .map_err(|error| executable.resume_or(error, || resume_arguments(after)))
}

/// This command's arguments, resuming the wait after the post `after`
/// without posting again.
fn resume_arguments(after: i64) -> Vec<std::ffi::OsString> {
    replace_option(arguments(), "--resume", &after.to_string())
}

/// Print the other side's posts after the post `after`, or how the thread
/// was settled; printed here, their alerts need not reach the agent again.
async fn print_reply(ctx: &Context, thread: &Thread, after: i64) -> Result<i32> {
    let mine = (thread.posts.iter())
        .find(|post| post.id == after)
        .map(|post| post.side);
    let replies: Vec<&Post> = (thread.posts.iter())
        .filter(|post| post.id > after && Some(post.side) != mine && post.speaks())
        .collect();
    let text = match (&replies[..], &thread.resolution) {
        ([], Some(resolution)) => format!(
            "Thread {} was {}.",
            thread.number,
            resolution.outcome.describe()
        ),
        (replies, _) => (replies.iter())
            .map(|post| format!("{}: {}", post.author(), post.post.text))
            .collect::<Vec<_>>()
            .join("\n"),
    };
    ctx.emit(&text, thread)?;
    crate::stdout::flush()?;
    let read: Vec<i64> = replies.iter().filter_map(|post| post.alert_id).collect();
    if !read.is_empty() {
        crate::client::alerts::mark_read(&ctx.paths, read).await?;
    }
    Ok(0)
}
