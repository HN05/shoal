//! `shoal queue` on a terminal: pick a waiting entry, then what to do with it.
use anyhow::Result;

use super::{changes, listing, selection};
use crate::{
    cli::{context::Context, ui},
    state::QueueKind,
};

/// What the menu does with the entry it picks.
#[derive(Clone, Copy)]
enum Action {
    Start,
    StartAttached,
    Cancel,
}

/// Pick a waiting entry, then an action, starting one only where the
/// `limits` permission lets; with nothing waiting, list the queue.
pub(super) async fn choose(ctx: &Context) -> Result<i32> {
    let waiting = selection::waiting(ctx).await?;
    if waiting.is_empty() {
        return listing::list(ctx, None).await;
    }
    let entry = selection::pick_entry(ctx, &waiting, "Queue> ").await?;
    let mut actions = vec![(Action::Cancel, "Cancel")];
    let limits = crate::config::permissions::PermissionKey::Limits;
    if crate::cli::commands::authorization::permits(ctx, limits).await? {
        actions.insert(0, (Action::Start, "Start now"));
        // A CI job runs without a session.
        if entry.kind != QueueKind::Job {
            actions.insert(1, (Action::StartAttached, "Start and attach"));
        }
    }
    let action = ui::pick_choice(ctx, &format!("{}> ", entry.name), &actions)?;
    let entry = Some(entry.id.into());
    match action {
        Action::Start => changes::start(ctx, entry, false).await,
        Action::StartAttached => changes::start(ctx, entry, true).await,
        Action::Cancel => changes::cancel(ctx, entry).await,
    }
}
