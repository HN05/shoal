//! `shoal thread list` and `shoal view thread`: choose a thread on a
//! terminal, read it, and reply when it waits for the user; or print them.
use anyhow::{Context as _, Result};
use shoal_protocol::{
    space_names::SpaceSelector,
    surfaces::SurfaceAction,
    threads::{Answer, NewPost, Thread, ThreadNumber, ThreadState},
};

use super::render;
use crate::{
    cli::{
        context::Context,
        output::{Palette, Style},
        ui::{self, Answering, Entries, WidthLimit},
    },
    client::threads,
};

/// List the threads of `space`, else the caller's, in `state`: a picker on
/// a terminal unless `print`, else the list.
pub(in crate::cli::commands) async fn list(
    ctx: &Context,
    space: Option<SpaceSelector>,
    state: ThreadState,
    print: bool,
) -> Result<i32> {
    let listing = threads::list(&ctx.paths, space, state).await?;
    if ctx.interactive_output() && !print && !listing.threads.is_empty() {
        let thread = pick(ctx, listing.threads)?;
        return read(ctx, thread).await;
    }
    ctx.show(&listing, |listing| {
        let palette = Palette::stdout(ctx.json);
        for thread in &listing.threads {
            outln!("{}", render::row(thread, palette));
        }
        if listing.omitted > 0 {
            outln!("{} older threads not shown", listing.omitted);
        } else if listing.threads.is_empty() {
            outln!("No threads");
        }
    })?;
    Ok(0)
}

/// Show the thread `number`, offering a reply on a terminal when it waits
/// for the user.
pub(in crate::cli::commands) async fn view(ctx: &Context, number: ThreadNumber) -> Result<i32> {
    let thread = threads::show(&ctx.paths, number).await?;
    read(ctx, thread).await
}

/// Show `thread`, and on a terminal read the user's reply when it waits
/// for them.
pub(in crate::cli::commands) async fn read(ctx: &Context, thread: Thread) -> Result<i32> {
    if ctx.interactive_output() && thread.waits_for_user() && !crate::env::is_scoped() {
        return reply(ctx, thread).await;
    }
    ctx.show(&thread, |thread| {
        outln!("{}", render::details(thread, Palette::stdout(ctx.json)))
    })?;
    Ok(0)
}

pub(super) fn pick(ctx: &Context, threads: Vec<Thread>) -> Result<Thread> {
    let picked = ui::pick(ctx, "Thread> ", picker_rows(&threads))?;
    threads
        .into_iter()
        .find(|thread| thread.number.to_string() == picked)
        .context("picker returned an unknown thread")
}

/// One aligned row per thread, as the inbox shows it: its number, its
/// space, its name cut to fit the picker, and where it stands.
fn picker_rows(threads: &[Thread]) -> Entries {
    let cells = threads
        .iter()
        .map(|thread| {
            [
                (thread.number.to_string(), Some(Style::Muted)),
                (thread.space.to_string(), None),
                (thread.one_line(), None),
                (render::status(thread), Some(Style::Muted)),
            ]
        })
        .collect();
    let limit = ui::picker_width().map(|width| WidthLimit { column: 2, width });
    let labels = ui::aligned_to(cells, Palette::picker(), limit);
    threads
        .iter()
        .map(|thread| thread.number.to_string())
        .zip(labels)
        .collect()
}

/// Show `thread` and post the user's reply: an action of the other side's
/// latest surface, picked with the values it takes, or a reply in words.
/// Ctrl-C or end of input cancels.
async fn reply(ctx: &Context, thread: Thread) -> Result<i32> {
    eprintln!("{}", render::details(&thread, Palette::stderr(ctx.json)));
    let number = ThreadNumber::Full(thread.number);
    let posted = match answerable(&thread) {
        Some((post, asked, actions)) => match ui::pick_answer(ctx, &asked, &actions)? {
            Answering::Action { id, values } => {
                let answer = Answer {
                    post: Some(post),
                    action: id,
                    values,
                };
                threads::answer(&ctx.paths, number, answer, None).await?
            }
            Answering::Words(text) => post_words(ctx, number, text).await?,
        },
        None => {
            let text = ui::input_or_cancel(ctx, "Reply")?.ok_or(ui::SelectionCanceled)?;
            post_words(ctx, number, text).await?
        }
    };
    ctx.emit(&format!("Replied in thread {}.", posted.number), &posted)?;
    Ok(0)
}

/// The post the user answers in `thread`, the other side's latest surface,
/// when it offers actions: its ID, its author, text and surface as the
/// picker's header, and its actions.
fn answerable(thread: &Thread) -> Option<(i64, String, Vec<SurfaceAction>)> {
    let other = thread.last_turn()?.side?;
    let post = thread.surface_from(other)?;
    let surface = post.post.ui.as_ref()?;
    let actions = surface.actions();
    let asked = std::iter::once(format!("{}: {}", post.author(), post.post.text))
        .chain(
            surface
                .text_lines(None)
                .into_iter()
                .map(|line| format!("  {line}")),
        )
        .collect::<Vec<_>>()
        .join("\n");
    (!actions.is_empty()).then_some((post.id, asked, actions))
}

/// Post `text` in the thread `number` as the user.
async fn post_words(ctx: &Context, number: ThreadNumber, text: String) -> Result<Thread> {
    let post = NewPost {
        text,
        ..NewPost::default()
    };
    let posted = threads::post(
        &ctx.paths,
        number,
        post,
        Vec::new(),
        Vec::new(),
        Vec::new(),
        None,
    );
    posted.await
}
