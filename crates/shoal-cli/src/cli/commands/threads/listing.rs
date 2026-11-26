//! `shoal thread list` and `shoal view thread`.
use anyhow::{Context as _, Result};
use shoal_protocol::{
    item_numbers::ItemNumber,
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

pub(in crate::cli::commands) async fn view(ctx: &Context, number: ThreadNumber) -> Result<i32> {
    let thread = threads::show(&ctx.paths, number).await?;
    read(ctx, thread).await
}

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

fn answerable(thread: &Thread) -> Option<(i64, String, Vec<SurfaceAction>)> {
    let other = thread.last_turn()?.side?;
    let post = thread.surface_from(other)?;
    let surface = post.post.ui.as_ref()?;
    let actions = surface.actions();
    let lines = std::iter::once(format!("{}: {}", post.author(), post.post.text)).chain(
        surface
            .text_lines(None)
            .into_iter()
            .map(|line| format!("  {line}")),
    );
    let asked = header(lines, thread.number);
    (!actions.is_empty()).then_some((post.id, asked, actions))
}

/// Most bytes the picker's header takes, which fzf receives as one
/// argument the system limits.
const MAX_HEADER_BYTES: usize = 8 * 1024;

/// `lines` as the header of the picker that answers the thread `number`:
/// as many whole lines as fit [`MAX_HEADER_BYTES`], then where to read the
/// rest.
pub(super) fn header(lines: impl Iterator<Item = String>, number: ItemNumber) -> String {
    let rest = format!("… (shoal view thread {number} shows the rest)");
    let mut shown = Vec::new();
    let mut used = 0;
    for line in lines {
        let line = shoal_protocol::validate::shorten(line, MAX_HEADER_BYTES / 2);
        if used + line.len() + 1 + rest.len() > MAX_HEADER_BYTES {
            shown.push(rest);
            return shown.join("\n");
        }
        used += line.len() + 1;
        shown.push(line);
    }
    shown.join("\n")
}

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
