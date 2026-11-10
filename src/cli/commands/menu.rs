//! The bare `shoal` picker, which `shoal inbox` and `shoal list` also open
//! on a terminal: a list per view that Tab cycles, reloading its rows from
//! `shoal internal menu-rows`. Each view binds only the keys that mean
//! something there. A selection plus key becomes an ordinary [`Command`].
mod execute;
mod keys;
mod linked;
mod rows;
mod views;

use anyhow::{Context as _, Result, ensure};
use clap::ValueEnum;

use keys::Action;
use rows::{Row, rows};
pub(crate) use views::View;

use crate::client::space_names::SpaceSelector;
use crate::{
    cli::{
        Command,
        context::Context,
        internal,
        ui::{self, Entries, ViewPicker},
    },
    env,
    workers::Worker,
};

/// Open the picker on `start`, or bare on the inbox unless nothing needs
/// the user, searching for the current workspace's items when it has any.
pub(super) async fn choose(ctx: &Context, start: Option<View>) -> Result<Choice> {
    let scoped = env::inherits_scope(&ctx.paths.state);
    let views = View::available(scoped);
    let (view, entries) = first_view(ctx, start, views, scoped).await?;
    let query = match start {
        None if view == View::Inbox => current_items(ctx, &entries).await?,
        _ => None,
    };
    let next = (views.len() > 1)
        .then(|| internal::shell_command(&ctx.paths, Worker::MenuNext))
        .transpose()?;
    let picked = ui::pick_in_views(
        ctx,
        entries,
        ViewPicker {
            view: view.description(scoped, next.is_some()),
            keys: &Action::ALL.map(Action::key),
            next,
            query,
        },
    )?;
    keys::command(ctx, Action::of(picked.action)?, Row::parse(picked.id)?).await
}

/// An exact search for the current workspace's name when `entries` hold
/// items of it.
async fn current_items(ctx: &Context, entries: &Entries) -> Result<Option<String>> {
    let workspaces = crate::client::workspaces::list(&ctx.paths).await?;
    let Some(current) = ui::current_workspace(&workspaces)? else {
        return Ok(None);
    };
    let has_items = entries.iter().any(|(id, _)| {
        Row::parse(id.clone()).is_ok_and(|row| row.space() == Some(SpaceSelector::id(&current.id)))
    });
    Ok(has_items.then(|| format!("'{}", current.name)))
}

/// The view to open and its rows: `start`, or the first available view
/// with rows.
async fn first_view(
    ctx: &Context,
    start: Option<View>,
    views: &[View],
    scoped: bool,
) -> Result<(View, Entries)> {
    if let Some(view) = start {
        return Ok((view, rows(ctx, view, scoped).await?));
    }
    for &view in &views[..views.len() - 1] {
        let entries = rows(ctx, view, scoped).await?;
        if !entries.is_empty() {
            return Ok((view, entries));
        }
    }
    let last = views[views.len() - 1];
    Ok((last, rows(ctx, last, scoped).await?))
}

/// Print `view`'s rows for the picker to reload, refusing the views a
/// workspace process may not see, whatever variables it cleared.
pub(super) async fn print_rows(ctx: &Context, view: View) -> Result<i32> {
    let scoped = env::inherits_scope(&ctx.paths.state);
    ensure!(
        View::available(scoped).contains(&view),
        "workspace processes cannot see the inbox or notifications; ask the user"
    );
    for (id, label) in rows(ctx, view, scoped).await? {
        outln!("{}", ui::picker_line(&id, &label));
    }
    Ok(0)
}

/// Print the fzf actions that switch from the view fzf shows, named by its
/// prompt, to the next one.
pub(super) fn print_next(ctx: &Context) -> Result<i32> {
    let scoped = env::inherits_scope(&ctx.paths.state);
    let views = View::available(scoped);
    let next = *ui::next_picker_view(views, |view| view.prompt())?;
    let name = next.to_possible_value().context("views have names")?;
    let reload = internal::shell_command(
        &ctx.paths,
        Worker::MenuRows {
            view: name.get_name(),
        },
    )?;
    outln!(
        "{}",
        next.description(scoped, true)
            .transition(&Action::ALL.map(Action::key), &reload)
    );
    Ok(0)
}

/// What the picker chose: a command to run, or the exit code of an action
/// it finished itself.
pub(super) enum Choice {
    Run(Box<Command>),
    Done(i32),
}
