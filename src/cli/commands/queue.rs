//! `shoal queue`: the agent queue of every repository, and starting, moving
//! and cancelling its entries, named or picked.
use anyhow::{Result, ensure};

use super::{agent_queue, listing::ListFilter};
use crate::{
    cli::{args::sessions::QueueCommand, client::request, context::Context, ui},
    model::{MovedEntry, QueueEntry, QueuePlace},
    protocol::QueueMethod,
    state::{QueueKind, QueueState},
};

pub(super) async fn run(
    ctx: &Context,
    command: Option<QueueCommand>,
    repository: Option<String>,
    print: bool,
) -> Result<i32> {
    match command {
        None if repository.is_none() && !print && ctx.interactive_output() => choose(ctx).await,
        None => list(ctx, repository).await,
        Some(QueueCommand::Start { entry }) => start(ctx, entry).await,
        Some(QueueCommand::Move { entry, position }) => move_entry(ctx, entry, position).await,
        Some(QueueCommand::Cancel { entry }) => cancel(ctx, entry).await,
    }
}

/// List the entries of `repository`, of every repository without one.
async fn list(ctx: &Context, repository: Option<String>) -> Result<i32> {
    let filter = ListFilter::resolve(ctx, repository, Vec::new()).await?;
    let entries = agent_queue::listed(ctx, &filter).await?;
    let rows = agent_queue::rows(ctx, &entries).await?;
    ctx.show(&entries, |_| {
        if rows.is_empty() {
            outln!("No queued agents");
        }
        for row in rows {
            outln!("{row}");
        }
    })?;
    Ok(0)
}

/// What `shoal queue` on a terminal does with the entry it picks.
#[derive(Clone, Copy)]
enum Action {
    Start,
    Move,
    Cancel,
}

/// Pick a waiting entry, then what to do with it. Workspace processes may
/// not start one.
async fn choose(ctx: &Context) -> Result<i32> {
    let waiting = waiting(ctx).await?;
    if waiting.is_empty() {
        return list(ctx, None).await;
    }
    let entry = pick_entry(ctx, &waiting, "Queue> ").await?;
    let mut actions = vec![(Action::Move, "Move"), (Action::Cancel, "Cancel")];
    if !crate::env::inherits_scope(&ctx.paths.state) {
        actions.insert(0, (Action::Start, "Start now"));
    }
    let action = ui::pick_choice(ctx, &format!("{}> ", entry.name), &actions)?;
    let entry = Some(entry.id);
    match action {
        Action::Start => start(ctx, entry).await,
        Action::Move => move_entry(ctx, entry, None).await,
        Action::Cancel => cancel(ctx, entry).await,
    }
}

/// `shoal queue start`: start the entry now, regardless of the limits.
async fn start(ctx: &Context, entry: Option<String>) -> Result<i32> {
    // A process that cleared its scope token is still the workspace's.
    ensure!(
        !crate::env::inherits_scope(&ctx.paths.state),
        "start queued agents outside scoped executions"
    );
    let entry = selected(ctx, entry, "Start> ").await?;
    let entry: QueueEntry = request(&ctx.paths, QueueMethod::QueueStart { entry }).await?;
    ctx.emit(
        &format!("Starting {} from the agent queue", entry.name),
        &entry,
    )?;
    Ok(0)
}

/// `shoal queue cancel`: remove the entry; a restore's agent stays stopped.
async fn cancel(ctx: &Context, entry: Option<String>) -> Result<i32> {
    require_scope_token(ctx)?;
    let entry = selected(ctx, entry, "Cancel> ").await?;
    let entry: QueueEntry = request(&ctx.paths, QueueMethod::QueueRemove { entry }).await?;
    let message = match entry.kind {
        QueueKind::Launch => format!("Removed {} from the agent queue", entry.name),
        QueueKind::Restore => format!(
            "Removed the restore of {0} from the agent queue; restore it with shoal resume {0}",
            entry.name
        ),
    };
    ctx.emit(&message, &entry)?;
    Ok(0)
}

/// `shoal queue move`: place the entry, or a running agent's restore, at
/// `position`, or in front of the entry chosen on a terminal.
async fn move_entry(
    ctx: &Context,
    entry: Option<String>,
    position: Option<QueuePlace>,
) -> Result<i32> {
    require_scope_token(ctx)?;
    let entry = selected(ctx, entry, "Move> ").await?;
    let place = match position {
        Some(place) => place,
        None => pick_place(ctx, &entry).await?,
    };
    let moved: MovedEntry = request(&ctx.paths, QueueMethod::QueueMove { entry, place }).await?;
    let place = ordinal(moved.entry.position.unwrap_or_default());
    let message = if moved.stopped {
        format!(
            "Stopped {}; its restore is {place} in the queue",
            moved.entry.name
        )
    } else {
        format!("Moved {}; it is {place} in the queue", moved.entry.name)
    };
    ctx.emit(&message, &moved)?;
    Ok(0)
}

/// Refuse a workspace process that cleared its scope token, which the
/// daemon would take for the user; with the token it confines the change.
fn require_scope_token(ctx: &Context) -> Result<()> {
    ensure!(
        !crate::env::inherits_scope(&ctx.paths.state) || crate::env::is_scoped(),
        "workspace processes change the agent queue only with their scope token"
    );
    Ok(())
}

/// The waiting entries in queue order.
async fn waiting(ctx: &Context) -> Result<Vec<QueueEntry>> {
    let mut entries = agent_queue::entries(ctx).await?;
    entries.retain(|entry| entry.state == QueueState::Queued);
    Ok(entries)
}

/// The entry `selector` names, or one picked among the waiting entries.
async fn selected(ctx: &Context, selector: Option<String>, prompt: &str) -> Result<String> {
    if let Some(selector) = selector {
        return Ok(selector);
    }
    let waiting = waiting(ctx).await?;
    ensure!(!waiting.is_empty(), "no agents wait in the queue");
    Ok(pick_entry(ctx, &waiting, prompt).await?.id)
}

/// Pick one of `entries`.
async fn pick_entry(ctx: &Context, entries: &[QueueEntry], prompt: &str) -> Result<QueueEntry> {
    let id = ui::pick(ctx, prompt, labelled(ctx, entries).await?)?;
    entries
        .iter()
        .find(|entry| entry.id == id)
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("picker returned an unknown entry"))
}

/// Pick the place of the entry `selector` names, in front of another
/// waiting entry or last.
async fn pick_place(ctx: &Context, selector: &str) -> Result<QueuePlace> {
    ensure!(
        ctx.interactive(),
        "name the position to move {selector} to, or last"
    );
    let (moved, others): (Vec<_>, Vec<_>) = waiting(ctx)
        .await?
        .into_iter()
        .partition(|entry| entry.id == selector || entry.name == selector);
    let name = moved.first().map_or(selector, |entry| entry.name.as_str());
    let place = ui::pick_place(
        ctx,
        &format!("Move {name} in front of> "),
        labelled(ctx, &others).await?,
    )?;
    Ok(match place {
        ui::Place::Before(id) => QueuePlace::Before(id),
        ui::Place::Last => QueuePlace::Last,
    })
}

/// `entries` as picker entries: each ID with its listing row.
async fn labelled(ctx: &Context, entries: &[QueueEntry]) -> Result<ui::Entries> {
    let rows = agent_queue::rows(ctx, entries).await?;
    Ok(entries
        .iter()
        .map(|entry| entry.id.clone())
        .zip(rows)
        .collect())
}

/// `n` as an English ordinal: 1st, 2nd, 3rd, 4th, 11th, 21st.
fn ordinal(n: usize) -> String {
    let suffix = match (n % 10, n % 100) {
        (_, 11..=13) => "th",
        (1, _) => "st",
        (2, _) => "nd",
        (3, _) => "rd",
        _ => "th",
    };
    format!("{n}{suffix}")
}

#[cfg(test)]
mod tests;
