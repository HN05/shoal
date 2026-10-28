//! `shoal queue` on a terminal: pick a waiting entry, then what to do with it.
use anyhow::Result;

use super::{changes, listing, selection};
use crate::cli::{context::Context, ui};

/// What the menu does with the entry it picks.
#[derive(Clone, Copy)]
enum Action {
    Start,
    Move,
    Cancel,
}

/// Pick a waiting entry, then an action. Workspace processes may not start
/// one; with nothing waiting, list the queue.
pub(super) async fn choose(ctx: &Context) -> Result<i32> {
    let waiting = selection::waiting(ctx).await?;
    if waiting.is_empty() {
        return listing::list(ctx, None).await;
    }
    let entry = selection::pick_entry(ctx, &waiting, "Queue> ").await?;
    let mut actions = vec![(Action::Move, "Move"), (Action::Cancel, "Cancel")];
    if !crate::env::inherits_scope(&ctx.paths.state) {
        actions.insert(0, (Action::Start, "Start now"));
    }
    let action = ui::pick_choice(ctx, &format!("{}> ", entry.name), &actions)?;
    let entry = Some(entry.id);
    match action {
        Action::Start => changes::start(ctx, entry).await,
        Action::Move => changes::move_entry(ctx, entry, None).await,
        Action::Cancel => changes::cancel(ctx, entry).await,
    }
}
