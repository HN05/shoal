//! `shoal thread switch`, `add` and `remove`.
use anyhow::Result;
use shoal_protocol::{
    changes::ChangeNumber,
    space_names::SpaceSelector,
    threads::{Thread, ThreadNumber, ThreadState},
};

use crate::{
    cli::{
        commands::changes,
        context::Context,
        ui::{self, Fallback},
    },
    client::threads,
};

pub(in crate::cli::commands) enum Switch {
    To(Option<ThreadNumber>),
    Show,
}

pub(in crate::cli::commands) async fn switch(
    ctx: &Context,
    switch: Switch,
    space: Option<SpaceSelector>,
) -> Result<i32> {
    let space = ui::select_space(ctx, space, Fallback::OwnSpace).await?;
    let to = match switch {
        Switch::To(thread) => Some(thread),
        Switch::Show => picked(ctx, &space).await?.map(Some),
    };
    let Some(to) = to else {
        let current = threads::current(&ctx.paths, space).await?;
        ctx.emit(
            &format!("Your changes are for {}.", described(current.as_ref())),
            &current,
        )?;
        return Ok(0);
    };
    let switched = threads::switch(&ctx.paths, space, to).await?;
    let message = format!(
        "Your changes from now on are for {}.",
        described(switched.as_ref())
    );
    ctx.emit(&message, &switched)?;
    Ok(0)
}

/// An open thread of `space` picked on a terminal; none elsewhere, or
/// without one to pick.
async fn picked(ctx: &Context, space: &SpaceSelector) -> Result<Option<ThreadNumber>> {
    if !ctx.interactive_output() {
        return Ok(None);
    }
    let listing = threads::list(&ctx.paths, Some(space.clone()), ThreadState::Open).await?;
    if listing.threads.is_empty() {
        return Ok(None);
    }
    let thread = super::listing::pick(ctx, listing.threads)?;
    Ok(Some(ThreadNumber::Full(thread.number)))
}

fn described(thread: Option<&Thread>) -> String {
    match thread {
        Some(thread) => format!("thread {}: {}", thread.number, thread.one_line()),
        None => "no thread".into(),
    }
}

pub(in crate::cli::commands) async fn add(
    ctx: &Context,
    thread: ThreadNumber,
    numbers: Vec<ChangeNumber>,
    space: Option<SpaceSelector>,
) -> Result<i32> {
    let space = changes::own_space(ctx, space, &numbers).await?;
    let added = threads::add_changes(&ctx.paths, thread, space, numbers).await?;
    let message = format!("Put {} into thread {thread}.", changes::labels(&added));
    ctx.emit(&message, &added)?;
    Ok(0)
}

pub(in crate::cli::commands) async fn remove(
    ctx: &Context,
    thread: ThreadNumber,
    numbers: Vec<ChangeNumber>,
    space: Option<SpaceSelector>,
) -> Result<i32> {
    let space = changes::own_space(ctx, space, &numbers).await?;
    let removed = threads::remove_changes(&ctx.paths, thread, space, numbers).await?;
    let message = format!("Took {} out of thread {thread}.", changes::labels(&removed));
    ctx.emit(&message, &removed)?;
    Ok(0)
}
