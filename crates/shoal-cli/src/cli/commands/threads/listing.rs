//! `shoal thread list` and `shoal view thread`: choose a thread on a
//! terminal, read it, and reply when it waits for the user; or print them.
use anyhow::{Context as _, Result};
use shoal_protocol::{
    space_names::SpaceSelector,
    threads::{NewPost, Thread, ThreadNumber, ThreadState},
};

use super::render;
use crate::{
    cli::{
        context::Context,
        output::{Palette, Style},
        ui::{self, Entries, WidthLimit},
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

/// Show `thread` and post the user's reply: Enter alone takes the
/// recommendation of its latest post, and cancels without one; Ctrl-C or
/// end of input cancels.
async fn reply(ctx: &Context, thread: Thread) -> Result<i32> {
    eprintln!("{}", render::details(&thread, Palette::stderr(ctx.json)));
    let recommendation = (thread.last_post()).and_then(|post| post.post.recommendation.clone());
    let prompt = match &recommendation {
        Some(recommendation) => format!("Reply [{recommendation}]"),
        None => "Reply".to_owned(),
    };
    let response = ui::read_input(ctx, &prompt)?.ok_or(ui::SelectionCanceled)?;
    let text = match (response.as_str(), recommendation) {
        ("", Some(recommendation)) => recommendation,
        ("", None) => return Err(ui::SelectionCanceled.into()),
        _ => response,
    };
    let post = NewPost {
        text,
        ..NewPost::default()
    };
    let number = ThreadNumber::Full(thread.number);
    let posted = threads::post(
        &ctx.paths,
        number,
        post,
        Vec::new(),
        Vec::new(),
        Vec::new(),
        None,
    );
    let posted = posted.await?;
    ctx.emit(&format!("Replied in thread {}.", posted.number), &posted)?;
    Ok(0)
}
