//! `shoal send <thread>`: post in a thread, optionally offering a surface,
//! the other side's interface to pick or fill in, or answering one of its
//! actions, wait for the other side's next post, or mark the agent's turn
//! to post its final message instead.
use std::path::PathBuf;

use anyhow::{Context as _, Result};
use shoal_protocol::{
    space_names::SpaceSelector,
    surfaces::Surface,
    threads::{Answer, NewPost, Post, Side, Thread, ThreadNumber},
};

use crate::{
    cli::{
        args::threads::PostArgs,
        context::Context,
        ui::{self, Fallback},
    },
    client::threads,
    installed::{
        Executable,
        arguments::{arguments, replace_option},
    },
};

/// A post `shoal send` makes in a thread.
pub(in crate::cli) struct Posting {
    pub(in crate::cli) thread: ThreadNumber,
    /// The text given inline; none with --body-file, --action or --in-chat.
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
    let thread = posting.thread;
    let wait = posting.options.wait;
    let attachments = crate::cli::commands::send::attachments(&posting.attach)?;
    shoal_protocol::files::check_sent(&attachments)?;
    let code = super::code_refs(std::mem::take(&mut posting.options.code))?;
    let changes = std::mem::take(&mut posting.options.changes);
    let post = new_post(posting)?;
    let posted = threads::post(&ctx.paths, thread, post, attachments, code, changes, from);
    let posted = posted.await?;
    let mine = posted.posts.last().context("the post was not recorded")?;
    if !wait {
        let message = match mine.side {
            Some(side) => format!(
                "Posted in thread {}, for {}.",
                posted.number,
                posted.party(side.other())
            ),
            None => format!(
                "Posted in thread {} as a participant, for {} and {}.",
                posted.number,
                posted.party(Side::Opener),
                posted.party(Side::Holder)
            ),
        };
        ctx.emit(&message, &posted)?;
        return Ok(0);
    }
    let after = mine.id;
    let replied = await_reply(ctx, &posted, after, false).await?;
    print_reply(ctx, &replied, after).await
}

/// What `posting` says: its text, read from `--body-file` when it names
/// one, the surface it offers and the action it answers.
fn new_post(posting: Posting) -> Result<NewPost> {
    let Posting {
        text,
        body_file,
        options,
        ..
    } = posting;
    let text = match body_file {
        Some(path) => read_text(&path)?,
        None => text.unwrap_or_default(),
    };
    Ok(NewPost {
        text,
        ui: offered(&options)?,
        answer: (options.action).map(|action| answering(action, options.values)),
    })
}

/// The text of the file `path`, without its trailing newline.
fn read_text(path: &std::path::Path) -> Result<String> {
    let text = std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    Ok(text.trim_end().to_owned())
}

/// The surface `options` offer: the file `--ui` names, or the one the
/// `--choice` shorthand builds.
fn offered(options: &PostArgs) -> Result<Option<Surface>> {
    if let Some(path) = &options.ui {
        let surface = Surface::parse(&read_text(path)?);
        return surface
            .with_context(|| format!("read the surface in {}", path.display()))
            .map(Some);
    }
    if options.choices.is_empty() {
        return Ok(None);
    }
    let (recommend, reason) = (options.recommend.as_deref(), options.reason.as_deref());
    Ok(Some(Surface::choices(&options.choices, recommend, reason)))
}

/// The answer that takes the action `action` with `values`, a key given
/// more than once selecting each of its values.
fn answering(action: String, values: Vec<(String, String)>) -> Answer {
    let mut given = serde_json::Map::new();
    for (key, value) in values {
        let value = serde_json::Value::String(value);
        match given.remove(&key) {
            None => given.insert(key, value),
            Some(serde_json::Value::Array(mut several)) => {
                several.push(value);
                given.insert(key, serde_json::Value::Array(several))
            }
            Some(first) => given.insert(key, serde_json::Value::Array(vec![first, value])),
        };
    }
    Answer {
        post: None,
        action,
        values: given,
    }
}

/// Mark the agent's turn as posting its final message in the thread
/// `number`.
pub(in crate::cli) async fn post_in_chat(ctx: &Context, number: ThreadNumber) -> Result<i32> {
    super::refuse_workspace_processes(ctx)?;
    let space = ui::select_space(ctx, None, Fallback::OwnSpace).await?;
    let marked = threads::post_in_chat(&ctx.paths, space, number).await?;
    crate::cli::commands::send::in_chat::show(ctx, &marked)
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
    let replies: Vec<&Post> = (thread.replies_to(after).into_iter().flatten()).collect();
    let text = match (&replies[..], &thread.resolution) {
        ([], Some(resolution)) => format!(
            "Thread {} was {}.",
            thread.number,
            resolution.outcome.describe()
        ),
        (replies, _) => (replies.iter())
            .map(|post| reply_text(thread, post))
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

/// A reply of `thread` as `send --wait` prints it: who sent it, its text,
/// and what it offers or answers.
fn reply_text(thread: &Thread, post: &Post) -> String {
    let interaction = thread.interaction_lines(post).into_iter();
    std::iter::once(format!("{}: {}", post.author(), post.post.text))
        .chain(interaction.map(|line| format!("  {line}")))
        .collect::<Vec<_>>()
        .join("\n")
}
